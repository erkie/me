//! The language servers we know how to start, and finding them.
//!
//! Servers are grouped: one group serves related languages (a single
//! typescript-language-server handles JavaScript, TypeScript and TSX). A
//! group lists alternatives in order of preference; the first one installed
//! is used. `settings.json` can pick another command per group.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

pub struct ServerDef {
    pub name: &'static str,
    pub command: &'static str,
    pub args: &'static [&'static str],
}

pub struct Group {
    /// Settings key ("rust", "typescript", "python", …).
    pub key: &'static str,
    /// Our language names (as in `me_syntax`) and their LSP language ids.
    pub languages: &'static [(&'static str, &'static str)],
    pub servers: &'static [ServerDef],
}

const fn server(
    name: &'static str,
    command: &'static str,
    args: &'static [&'static str],
) -> ServerDef {
    ServerDef {
        name,
        command,
        args,
    }
}

pub static GROUPS: &[Group] = &[
    Group {
        key: "rust",
        languages: &[("Rust", "rust")],
        servers: &[server("rust-analyzer", "rust-analyzer", &[])],
    },
    Group {
        key: "go",
        languages: &[("Go", "go")],
        servers: &[server("gopls", "gopls", &[])],
    },
    Group {
        key: "typescript",
        languages: &[
            ("TypeScript", "typescript"),
            ("TSX", "typescriptreact"),
            ("JavaScript", "javascript"),
        ],
        servers: &[
            server(
                "typescript-language-server",
                "typescript-language-server",
                &["--stdio"],
            ),
            server("vtsls", "vtsls", &["--stdio"]),
        ],
    },
    Group {
        key: "python",
        languages: &[("Python", "python")],
        servers: &[
            server("pyright", "pyright-langserver", &["--stdio"]),
            server("basedpyright", "basedpyright-langserver", &["--stdio"]),
            server("pylsp", "pylsp", &[]),
            server("jedi-language-server", "jedi-language-server", &[]),
        ],
    },
    Group {
        key: "c",
        languages: &[("C", "c"), ("C++", "cpp")],
        servers: &[server("clangd", "clangd", &[])],
    },
    Group {
        key: "java",
        languages: &[("Java", "java")],
        servers: &[server("jdtls", "jdtls", &[])],
    },
    Group {
        key: "ruby",
        languages: &[("Ruby", "ruby")],
        servers: &[
            server("ruby-lsp", "ruby-lsp", &[]),
            server("solargraph", "solargraph", &["stdio"]),
        ],
    },
    Group {
        key: "swift",
        languages: &[("Swift", "swift")],
        servers: &[server("sourcekit-lsp", "sourcekit-lsp", &[])],
    },
    Group {
        key: "lua",
        languages: &[("Lua", "lua")],
        servers: &[server("lua-language-server", "lua-language-server", &[])],
    },
    Group {
        key: "shell",
        languages: &[("Shell", "shellscript")],
        servers: &[server(
            "bash-language-server",
            "bash-language-server",
            &["start"],
        )],
    },
];

/// The group serving `language`, and the document's language id.
pub fn group_for(language: &str) -> Option<(&'static Group, &'static str)> {
    GROUPS.iter().find_map(|group| {
        group
            .languages
            .iter()
            .find(|(name, _)| *name == language)
            .map(|(_, id)| (group, *id))
    })
}

/// The `PATH` to look for servers in, and to start them with: the login
/// shell's (apps launched from the Dock or Finder get a bare one), this
/// process's, and the usual install locations, including version managers'
/// shims (rbenv, asdf, mise, pyenv, nodenv), which pick the version for the
/// folder the server runs in.
pub fn search_path() -> &'static OsString {
    static PATH: OnceLock<OsString> = OnceLock::new();
    PATH.get_or_init(|| {
        let mut dirs: Vec<PathBuf> = Vec::new();
        if let Some(shell_path) = login_shell_path() {
            dirs.extend(std::env::split_paths(&shell_path));
        }
        if let Some(path) = std::env::var_os("PATH") {
            dirs.extend(std::env::split_paths(&path));
        }
        if let Some(home) = dirs::home_dir() {
            for dir in [
                ".cargo/bin",
                "go/bin",
                ".local/bin",
                ".bun/bin",
                ".volta/bin",
                ".npm-global/bin",
                ".deno/bin",
                ".rbenv/shims",
                ".asdf/shims",
                ".local/share/mise/shims",
                ".pyenv/shims",
                ".nodenv/shims",
                ".gem/bin",
            ] {
                dirs.push(home.join(dir));
            }
        }
        for dir in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"] {
            dirs.push(PathBuf::from(dir));
        }
        let mut seen = std::collections::HashSet::new();
        dirs.retain(|d| !d.as_os_str().is_empty() && seen.insert(d.clone()));
        std::env::join_paths(dirs).unwrap_or_default()
    })
}

/// `PATH` as the user's interactive login shell sets it (nvm, cargo, pyenv
/// and Homebrew are usually set up in shell startup files).
#[cfg(unix)]
fn login_shell_path() -> Option<OsString> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    const MARKER: &str = "__ME_PATH__";
    let shell = std::env::var_os("SHELL").unwrap_or_else(|| "/bin/sh".into());
    let mut child = Command::new(&shell)
        .args([
            "-l",
            "-i",
            "-c",
            &format!("printf '{MARKER}%s{MARKER}' \"$PATH\""),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // A slow or stuck shell startup must not hold anything up for long.
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    let start = out.find(MARKER)? + MARKER.len();
    let end = start + out[start..].find(MARKER)?;
    Some(out[start..end].into())
}

#[cfg(not(unix))]
fn login_shell_path() -> Option<OsString> {
    None
}

/// Finds `command` (a name or a path) on [`search_path`].
pub fn which(command: &str) -> Option<PathBuf> {
    let command = expand_home(command);
    if command.components().count() > 1 {
        return is_executable(&command).then_some(command);
    }
    let extensions: &[&str] = if cfg!(windows) {
        &[".exe", ".cmd", ".bat", ""]
    } else {
        &[""]
    };
    std::env::split_paths(search_path()).find_map(|dir| {
        extensions.iter().find_map(|ext| {
            let mut name = command.as_os_str().to_owned();
            name.push(ext);
            let candidate = dir.join(name);
            is_executable(&candidate).then_some(candidate)
        })
    })
}

fn expand_home(command: &str) -> PathBuf {
    match (command.strip_prefix("~/"), dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(command),
    }
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_cover_related_languages() {
        let (group, id) = group_for("TSX").unwrap();
        assert_eq!((group.key, id), ("typescript", "typescriptreact"));
        assert_eq!(group_for("C++").unwrap().0.key, "c");
        assert!(group_for("Markdown").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn finds_executables() {
        assert!(which("sh").is_some());
        assert_eq!(which("/bin/sh"), Some(PathBuf::from("/bin/sh")));
        assert!(which("surely-not-a-real-language-server").is_none());
    }
}

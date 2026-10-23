//! OS integration: installing the `editor` command and registering as the
//! default editor.

use std::path::PathBuf;

use gpui::{App, PromptLevel};

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub const BUNDLE_ID: &str = "com.erkie.editor";

/// Content types we offer to become the default editor for.
#[cfg(target_os = "macos")]
const CONTENT_TYPES: &[&str] = &[
    "public.plain-text",
    "public.source-code",
    "public.script",
    "public.shell-script",
    "public.json",
    "public.xml",
    "public.yaml",
    "public.python-script",
    "public.ruby-script",
    "public.c-source",
    "public.c-header",
    "public.c-plus-plus-source",
    "public.swift-source",
    "public.css",
    "public.make-source",
    "com.netscape.javascript-source",
    "net.daringfireball.markdown",
    "com.erkie.editor.rust-source",
    "com.erkie.editor.toml",
    "com.erkie.editor.typescript-source",
    "com.erkie.editor.go-source",
];

#[cfg(target_os = "linux")]
const MIME_TYPES: &[&str] = &[
    "text/plain",
    "text/markdown",
    "text/x-markdown",
    "text/x-rust",
    "text/x-python",
    "text/x-csrc",
    "text/x-chdr",
    "text/x-c++src",
    "text/x-go",
    "text/javascript",
    "application/json",
    "application/x-yaml",
    "application/toml",
    "application/x-shellscript",
];

fn notify(title: String, detail: Option<String>, cx: &mut App) {
    if let Some(window) = cx.active_window() {
        window
            .update(cx, |_, window, cx| {
                let _ = window.prompt(PromptLevel::Info, &title, detail.as_deref(), &["OK"], cx);
            })
            .ok();
    } else {
        eprintln!("{title}");
    }
}

/// The bundled CLI binary.
fn cli_binary() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    // Inside Editor.app: Contents/MacOS/Editor -> Contents/Resources/bin/editor
    let bundled = exe.parent()?.parent()?.join("Resources/bin/editor");
    if bundled.exists() {
        return Some(bundled);
    }
    let sibling = exe.with_file_name("editor");
    sibling.exists().then_some(sibling)
}

pub fn install_cli(cx: &mut App) {
    let Some(cli) = cli_binary() else {
        notify(
            "Could not find the bundled 'editor' command.".into(),
            None,
            cx,
        );
        return;
    };
    let result = install_cli_at(&cli);
    match result {
        Ok(path) => notify(
            format!("Installed 'editor' at {}", path.display()),
            Some("Open a folder with `editor .`. To use it for git commits, add `export EDITOR=\"editor -w\"` to your shell profile.".into()),
            cx,
        ),
        Err(err) => notify("Could not install the 'editor' command.".into(), Some(err), cx),
    }
}

fn install_cli_at(cli: &std::path::Path) -> Result<PathBuf, String> {
    let system = PathBuf::from("/usr/local/bin/editor");
    let link = |dest: &std::path::Path| -> std::io::Result<()> {
        if let Some(dir) = dest.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let _ = std::fs::remove_file(dest);
        #[cfg(unix)]
        return std::os::unix::fs::symlink(cli, dest);
        #[cfg(not(unix))]
        return std::fs::copy(cli, dest).map(drop);
    };
    if link(&system).is_ok() {
        return Ok(system);
    }
    #[cfg(target_os = "macos")]
    {
        // /usr/local/bin usually needs admin rights: ask macOS for them.
        let script = format!(
            "do shell script \"mkdir -p /usr/local/bin && ln -sf '{}' /usr/local/bin/editor\" with administrator privileges",
            cli.display()
        );
        let status = std::process::Command::new("osascript")
            .args(["-e", &script])
            .status();
        if status.is_ok_and(|s| s.success()) {
            return Ok(system);
        }
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("HOME is not set")?;
    let user = home.join(".local/bin/editor");
    link(&user).map_err(|e| e.to_string())?;
    Ok(user)
}

pub fn make_default_editor(cx: &mut App) {
    match register_default() {
        Ok(n) => notify(
            format!("Editor is now the default app for {n} kinds of text and code files."),
            Some("You can change this per file type in Finder's Get Info → Open With.".into()),
            cx,
        ),
        Err(err) => notify(
            "Could not register as the default editor.".into(),
            Some(err),
            cx,
        ),
    }
}

#[cfg(target_os = "macos")]
fn register_default() -> Result<usize, String> {
    use core_foundation::base::TCFType;
    use core_foundation::string::{CFString, CFStringRef};

    #[link(name = "CoreServices", kind = "framework")]
    unsafe extern "C" {
        fn LSSetDefaultRoleHandlerForContentType(
            content_type: CFStringRef,
            role: u32,
            handler_bundle_id: CFStringRef,
        ) -> i32;
    }
    const ROLES_ALL: u32 = 0xFFFF_FFFF;

    let bundle = CFString::new(BUNDLE_ID);
    let mut ok = 0;
    let mut last_error = 0;
    for uti in CONTENT_TYPES {
        let uti = CFString::new(uti);
        // SAFETY: both CFStrings are valid for the duration of the call.
        let status = unsafe {
            LSSetDefaultRoleHandlerForContentType(
                uti.as_concrete_TypeRef(),
                ROLES_ALL,
                bundle.as_concrete_TypeRef(),
            )
        };
        if status == 0 {
            ok += 1;
        } else {
            last_error = status;
        }
    }
    if ok == 0 {
        return Err(format!(
            "Launch Services returned error {last_error}. Is Editor.app in /Applications?"
        ));
    }
    Ok(ok)
}

#[cfg(target_os = "linux")]
fn register_default() -> Result<usize, String> {
    let status = std::process::Command::new("xdg-mime")
        .arg("default")
        .arg("editor.desktop")
        .args(MIME_TYPES)
        .status()
        .map_err(|e| format!("xdg-mime: {e}"))?;
    if status.success() {
        Ok(MIME_TYPES.len())
    } else {
        Err(format!("xdg-mime exited with {status}"))
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn register_default() -> Result<usize, String> {
    Err("Not supported on this platform yet.".into())
}

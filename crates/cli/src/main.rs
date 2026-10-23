//! `editor`: open files and folders in the editor app.
//!
//! This binary has no GUI dependencies so it starts in about a millisecond.
//! It talks to the running app over a socket (see `editor-ipc`), launching
//! the app first if needed.

use std::env;
use std::ffi::OsString;
use std::io::{self, IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use editor_ipc::{OpenPath, Request, parse_path_arg};

const HELP: &str = "\
Usage: editor [options] [path[:line[:column]] ...]

Opens files and folders in Editor, launching it if needed.

Examples:
  editor .                 open the current folder
  editor README.md         open a file
  editor src/main.rs:42:7  open a file at line 42, column 7
  git commit               with EDITOR=\"editor -w\", edit the message in Editor

Options:
  -w, --wait        wait for the opened files to be closed before returning
  -n, --new-window  open in a new window
  -                 read text from stdin into a new tab
  -h, --help        show this help
  -v, --version     show the version
";

#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    wait: bool,
    new_window: bool,
    stdin: bool,
    paths: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
enum Parsed {
    Run(Args),
    Help,
    Version,
    Error(String),
}

fn parse_args(args: impl IntoIterator<Item = OsString>) -> Parsed {
    let mut out = Args::default();
    let mut only_paths = false;
    for arg in args {
        let arg = arg.to_string_lossy().into_owned();
        if only_paths {
            out.paths.push(arg);
            continue;
        }
        match arg.as_str() {
            "--" => only_paths = true,
            "-" => out.stdin = true,
            "-h" | "--help" => return Parsed::Help,
            "-v" | "--version" => return Parsed::Version,
            "-w" | "--wait" => out.wait = true,
            "-n" | "--new-window" => out.new_window = true,
            s if s.starts_with("--") => return Parsed::Error(format!("unknown option {s}")),
            s if s.starts_with('-') && s.len() > 1 => {
                // Combined short flags: -wn
                for ch in s[1..].chars() {
                    match ch {
                        'w' => out.wait = true,
                        'n' => out.new_window = true,
                        'h' => return Parsed::Help,
                        'v' => return Parsed::Version,
                        _ => return Parsed::Error(format!("unknown option -{ch}")),
                    }
                }
            }
            _ => out.paths.push(arg),
        }
    }
    Parsed::Run(out)
}

fn main() -> ExitCode {
    let args = match parse_args(env::args_os().skip(1)) {
        Parsed::Run(args) => args,
        Parsed::Help => {
            print!("{HELP}");
            return ExitCode::SUCCESS;
        }
        Parsed::Version => {
            println!("editor {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Parsed::Error(message) => {
            eprintln!("editor: {message}\n\n{HELP}");
            return ExitCode::from(2);
        }
    };
    match run(args) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("editor: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Args) -> io::Result<ExitCode> {
    let cwd = env::current_dir()?;
    let paths: Vec<OpenPath> = args.paths.iter().map(|p| parse_path_arg(p, &cwd)).collect();
    let stdin = if args.stdin {
        let mut text = String::new();
        if io::stdin().is_terminal() {
            eprintln!("editor: reading from stdin (Ctrl+D to finish)");
        }
        io::stdin().read_to_string(&mut text)?;
        Some(text)
    } else {
        None
    };
    let request = Request::Open {
        cwd,
        paths,
        stdin,
        wait: args.wait,
        new_window: args.new_window,
    };
    send(request, args.wait)
}

#[cfg(unix)]
fn send(request: Request, wait: bool) -> io::Result<ExitCode> {
    use editor_ipc::{Client, Response};

    let socket = editor_ipc::socket_path();
    let mut client = match Client::connect(&socket) {
        Ok(client) => client,
        Err(_) => {
            launch_app()?;
            let timeout = env::var("EDITOR_CONNECT_TIMEOUT_MS")
                .ok()
                .and_then(|ms| ms.parse().ok())
                .map_or(Duration::from_secs(10), Duration::from_millis);
            connect_with_retry(&socket, timeout)?
        }
    };
    client.send(&request)?;

    let mut waiting: Vec<u64> = Vec::new();
    loop {
        match client.recv()? {
            Some(Response::Opened { ids }) => {
                if !wait || ids.is_empty() {
                    return Ok(ExitCode::SUCCESS);
                }
                waiting = ids;
            }
            Some(Response::Closed { id }) => {
                waiting.retain(|w| *w != id);
                if waiting.is_empty() {
                    return Ok(ExitCode::SUCCESS);
                }
            }
            Some(Response::Error { message }) => {
                return Err(io::Error::other(message));
            }
            Some(Response::Pong) => {}
            None => {
                return if wait && !waiting.is_empty() {
                    Err(io::Error::other(
                        "the editor quit before the file was closed",
                    ))
                } else {
                    Ok(ExitCode::SUCCESS)
                };
            }
        }
    }
}

#[cfg(unix)]
fn connect_with_retry(socket: &Path, timeout: Duration) -> io::Result<editor_ipc::Client> {
    let start = Instant::now();
    loop {
        match editor_ipc::Client::connect(socket) {
            Ok(client) => return Ok(client),
            Err(err) if start.elapsed() > timeout => {
                return Err(io::Error::new(
                    err.kind(),
                    format!("could not reach the editor at {}: {err}", socket.display()),
                ));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

/// Without a socket transport (Windows, for now) pass paths on the command
/// line. Wait mode is not supported there yet.
#[cfg(not(unix))]
fn send(request: Request, _wait: bool) -> io::Result<ExitCode> {
    let Request::Open { paths, .. } = request else {
        unreachable!()
    };
    let app = find_app()?;
    let mut cmd = Command::new(app);
    for p in paths {
        cmd.arg(p.path);
    }
    cmd.spawn()?;
    Ok(ExitCode::SUCCESS)
}

/// Launches the app without bringing its default window up; the request we
/// send next decides what opens.
fn launch_app() -> io::Result<()> {
    #[cfg(target_os = "macos")]
    if let Some(bundle) = find_bundle() {
        let status = Command::new("open")
            .arg("-a")
            .arg(&bundle)
            .args(["--args", "--from-cli"])
            .status()?;
        if !status.success() {
            return Err(io::Error::other(format!(
                "could not launch {}",
                bundle.display()
            )));
        }
        return Ok(());
    }
    let app = find_app()?;
    spawn_detached(&app)
}

/// The GUI binary: `$EDITOR_APP`, or `editor-app` next to this binary.
fn find_app() -> io::Result<PathBuf> {
    if let Some(app) = env::var_os("EDITOR_APP") {
        return Ok(PathBuf::from(app));
    }
    let exe = env::current_exe()?;
    let exe = exe.canonicalize().unwrap_or(exe);
    let name = if cfg!(windows) {
        "editor-app.exe"
    } else {
        "editor-app"
    };
    let sibling = exe.with_file_name(name);
    if sibling.exists() {
        return Ok(sibling);
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!(
            "could not find the editor app (looked for {})",
            sibling.display()
        ),
    ))
}

/// `Editor.app` containing this binary (`Contents/Resources/bin/editor`),
/// or the installed app.
#[cfg(target_os = "macos")]
fn find_bundle() -> Option<PathBuf> {
    if env::var_os("EDITOR_APP").is_some() {
        return None;
    }
    let exe = env::current_exe().ok()?;
    let exe = exe.canonicalize().unwrap_or(exe);
    if let Some(bundle) = exe
        .ancestors()
        .find(|p| p.extension().is_some_and(|e| e == "app"))
        .map(Path::to_path_buf)
    {
        return Some(bundle);
    }
    // A dev build next to a sibling editor-app runs that instead.
    if exe.with_file_name("editor-app").exists() {
        return None;
    }
    let installed = PathBuf::from("/Applications/Editor.app");
    installed.exists().then_some(installed)
}

fn spawn_detached(app: &Path) -> io::Result<()> {
    let mut cmd = Command::new(app);
    cmd.arg("--from-cli")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Own process group: Ctrl+C in this terminal must not kill the app.
        cmd.process_group(0);
    }
    cmd.spawn().map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Parsed {
        parse_args(args.iter().map(OsString::from))
    }

    #[test]
    fn flags_and_paths() {
        assert_eq!(
            parse(&["-w", ".", "b.md:3"]),
            Parsed::Run(Args {
                wait: true,
                paths: vec![".".into(), "b.md:3".into()],
                ..Default::default()
            })
        );
        assert_eq!(
            parse(&["-wn", "-", "--", "-weird"]),
            Parsed::Run(Args {
                wait: true,
                new_window: true,
                stdin: true,
                paths: vec!["-weird".into()]
            })
        );
        assert_eq!(parse(&["--help"]), Parsed::Help);
        assert_eq!(parse(&["-v"]), Parsed::Version);
        assert!(matches!(parse(&["--nope"]), Parsed::Error(_)));
        assert!(matches!(parse(&["-x"]), Parsed::Error(_)));
    }
}

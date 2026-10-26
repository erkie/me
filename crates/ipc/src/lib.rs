//! The protocol between the `me` CLI and the running app.
//!
//! Transport: a per-user Unix domain socket carrying newline-delimited JSON.
//! The CLI sends one [`Request`]; the app answers with a stream of
//! [`Response`]s. In wait mode (`-w`) the connection stays open until the
//! app reports every opened document closed.

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Environment variable that overrides the socket path (tests, dev builds).
pub const SOCKET_ENV: &str = "ME_SOCKET";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenPath {
    pub path: PathBuf,
    /// 1-based.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// 1-based.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// Open files and folders. With no paths and no stdin, just activate.
    Open {
        cwd: PathBuf,
        paths: Vec<OpenPath>,
        /// Text read from stdin (`me -`), opened as an untitled tab.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stdin: Option<String>,
        wait: bool,
        new_window: bool,
    },
    Ping,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    /// The request was handled. `ids` are the documents the CLI waits on.
    Opened {
        ids: Vec<u64>,
    },
    /// A waited-on document was closed.
    Closed {
        id: u64,
    },
    Error {
        message: String,
    },
    Pong,
}

pub fn write_message<T: Serialize>(mut writer: impl Write, message: &T) -> io::Result<()> {
    let mut line = serde_json::to_vec(message)?;
    line.push(b'\n');
    writer.write_all(&line)?;
    writer.flush()
}

/// Reads one message. `Ok(None)` at end of stream.
pub fn read_message<T: for<'de> Deserialize<'de>>(
    reader: &mut impl BufRead,
) -> io::Result<Option<T>> {
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    serde_json::from_str(&line)
        .map(Some)
        .map_err(io::Error::other)
}

/// The socket path for the current user.
pub fn socket_path() -> PathBuf {
    if let Some(path) = std::env::var_os(SOCKET_ENV) {
        return PathBuf::from(path);
    }
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(std::env::temp_dir);
    base.join(format!("me-{}", user_id())).join("me.sock")
}

#[cfg(unix)]
fn user_id() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

#[cfg(not(unix))]
fn user_id() -> u32 {
    0
}

/// Splits a CLI argument like `src/main.rs:12:5` into a path and position.
/// An argument naming an existing file is never split, so files with `:` in
/// their names still open.
pub fn parse_path_arg(arg: &str, cwd: &Path) -> OpenPath {
    let absolute = |p: &str| {
        let p = Path::new(p);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            cwd.join(p)
        }
    };
    let whole = absolute(arg);
    if whole.exists() {
        return OpenPath {
            path: normalize(&whole),
            line: None,
            column: None,
        };
    }
    let mut parts = arg.rsplitn(3, ':');
    let (last, mid, first) = (parts.next(), parts.next(), parts.next());
    if let (Some(path), Some(line), Some(col)) = (first, mid, last)
        && let (Ok(line), Ok(col)) = (line.parse(), col.parse())
        && !path.is_empty()
    {
        return OpenPath {
            path: normalize(&absolute(path)),
            line: Some(line),
            column: Some(col),
        };
    }
    if let Some((path, line)) = arg.rsplit_once(':')
        && let Ok(line) = line.parse()
        && !path.is_empty()
    {
        return OpenPath {
            path: normalize(&absolute(path)),
            line: Some(line),
            column: None,
        };
    }
    OpenPath {
        path: normalize(&whole),
        line: None,
        column: None,
    }
}

/// Removes `.` and `..` components without touching the file system (the
/// file may not exist yet).
pub fn normalize(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(unix)]
pub use unix::*;

#[cfg(unix)]
mod unix {
    use super::*;
    use std::io::BufReader;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};

    /// A connection from the CLI side.
    pub struct Client {
        reader: BufReader<UnixStream>,
        writer: UnixStream,
    }

    impl Client {
        pub fn connect(path: &Path) -> io::Result<Self> {
            let stream = UnixStream::connect(path)?;
            Ok(Self {
                writer: stream.try_clone()?,
                reader: BufReader::new(stream),
            })
        }

        pub fn send(&mut self, request: &Request) -> io::Result<()> {
            write_message(&mut self.writer, request)
        }

        pub fn recv(&mut self) -> io::Result<Option<Response>> {
            read_message(&mut self.reader)
        }
    }

    /// The app side: a listening socket.
    pub struct Server {
        listener: UnixListener,
        path: PathBuf,
    }

    #[derive(Debug)]
    pub enum BindError {
        /// Another instance is already listening.
        AlreadyRunning,
        Io(io::Error),
    }

    impl std::fmt::Display for BindError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                BindError::AlreadyRunning => write!(f, "another instance is already running"),
                BindError::Io(e) => write!(f, "{e}"),
            }
        }
    }

    impl std::error::Error for BindError {}

    impl Server {
        pub fn bind(path: &Path) -> Result<Self, BindError> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(BindError::Io)?;
                let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
            }
            if path.exists() {
                if UnixStream::connect(path).is_ok() {
                    return Err(BindError::AlreadyRunning);
                }
                // Stale socket from a crashed instance.
                let _ = std::fs::remove_file(path);
            }
            let listener = UnixListener::bind(path).map_err(BindError::Io)?;
            Ok(Self {
                listener,
                path: path.to_path_buf(),
            })
        }

        pub fn path(&self) -> &Path {
            &self.path
        }

        /// Blocks until the next client connects.
        pub fn accept(&self) -> io::Result<Connection> {
            let (stream, _) = self.listener.accept()?;
            Ok(Connection {
                writer: stream.try_clone()?,
                reader: BufReader::new(stream),
            })
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    /// One CLI connection, on the app side.
    pub struct Connection {
        reader: BufReader<UnixStream>,
        writer: UnixStream,
    }

    impl Connection {
        pub fn recv(&mut self) -> io::Result<Option<Request>> {
            read_message(&mut self.reader)
        }

        pub fn send(&mut self, response: &Response) -> io::Result<()> {
            write_message(&mut self.writer, response)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_format_is_stable() {
        let req = Request::Open {
            cwd: "/w".into(),
            paths: vec![OpenPath {
                path: "/w/a.md".into(),
                line: Some(3),
                column: None,
            }],
            stdin: None,
            wait: true,
            new_window: false,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(
            json,
            r#"{"type":"open","cwd":"/w","paths":[{"path":"/w/a.md","line":3}],"wait":true,"new_window":false}"#
        );
        assert_eq!(
            serde_json::to_string(&Response::Closed { id: 7 }).unwrap(),
            r#"{"type":"closed","id":7}"#
        );
    }

    #[test]
    fn path_args() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path();
        std::fs::write(cwd.join("weird:12"), "").unwrap();

        let p = parse_path_arg("src/main.rs:12:5", cwd);
        assert_eq!(
            (p.path, p.line, p.column),
            (cwd.join("src/main.rs"), Some(12), Some(5))
        );
        let p = parse_path_arg("./a/../b.md:7", cwd);
        assert_eq!(
            (p.path, p.line, p.column),
            (cwd.join("b.md"), Some(7), None)
        );
        let p = parse_path_arg("weird:12", cwd);
        assert_eq!((p.path, p.line), (cwd.join("weird:12"), None));
        let abs = if cfg!(windows) {
            r"C:\abs\x.txt"
        } else {
            "/abs/x.txt"
        };
        let p = parse_path_arg(abs, cwd);
        assert_eq!(p.path, PathBuf::from(abs));
        let p = parse_path_arg(".", cwd);
        assert_eq!(p.path, cwd.to_path_buf());
    }

    #[cfg(unix)]
    #[test]
    fn round_trip_over_socket() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sock/me.sock");
        let server = Server::bind(&path).unwrap();
        assert!(matches!(
            Server::bind(&path),
            Err(BindError::AlreadyRunning)
        ));

        let handle = std::thread::spawn(move || {
            // The AlreadyRunning probe above left an empty connection in the
            // backlog; real servers must skip those too.
            loop {
                let mut conn = server.accept().unwrap();
                match conn.recv().unwrap() {
                    None => continue,
                    Some(request) => {
                        assert_eq!(request, Request::Ping);
                        conn.send(&Response::Pong).unwrap();
                        break;
                    }
                }
            }
        });
        let mut client = Client::connect(&path).unwrap();
        client.send(&Request::Ping).unwrap();
        assert_eq!(client.recv().unwrap(), Some(Response::Pong));
        handle.join().unwrap();
        assert_eq!(client.recv().unwrap(), None);
    }

    #[cfg(unix)]
    #[test]
    fn stale_socket_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("me.sock");
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        assert!(path.exists());
        assert!(Server::bind(&path).is_ok());
    }
}

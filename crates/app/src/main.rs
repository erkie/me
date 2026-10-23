//! Editor: a fast, native editor for reading markdown and code.

mod actions;
mod app_state;
mod assets;
mod editor;
mod file_tree;
mod find_bar;
mod finder;
mod ipc;
mod markdown;
mod menus;
mod platform;
mod search_panel;
mod theme;
mod workspace;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use editor_ipc::{OpenPath, Request, parse_path_arg};
use futures::StreamExt;
use futures::channel::mpsc;
use gpui::App;

struct Args {
    /// Launched by the `editor` CLI, which will send what to open over the
    /// socket; don't open a default window.
    from_cli: bool,
    paths: Vec<OpenPath>,
}

fn parse_args() -> Args {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let mut args = Args { from_cli: false, paths: Vec::new() };
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--from-cli" => args.from_cli = true,
            // macOS passes this when launched from Finder on old systems.
            s if s.starts_with("-psn_") => {}
            s if s.starts_with("--") => {}
            s => args.paths.push(parse_path_arg(s, &cwd)),
        }
    }
    args
}

fn main() {
    let started = Instant::now();
    let args = parse_args();

    // Single instance: if the app is already running, hand our paths to it.
    #[cfg(unix)]
    let server = {
        let socket = editor_ipc::socket_path();
        match editor_ipc::Server::bind(&socket) {
            Ok(server) => Some(server),
            Err(editor_ipc::BindError::AlreadyRunning) => {
                forward_to_running(&socket, args.paths);
                return;
            }
            Err(err) => {
                eprintln!("editor: CLI integration disabled: {err}");
                None
            }
        }
    };

    let app = gpui_platform::application().with_assets(assets::Assets);

    // Files opened from Finder ("Open With", double click, drag on the Dock).
    let (urls_tx, mut urls_rx) = mpsc::unbounded::<Vec<String>>();
    app.on_open_urls(move |urls| {
        let _ = urls_tx.unbounded_send(urls);
    });
    app.on_reopen(|cx| {
        if app_state::workspaces(cx).is_empty() {
            app_state::open_window(None, cx);
        }
    });

    app.run(move |cx: &mut App| {
        gpui_component::init(cx);
        theme::init(cx.window_appearance(), cx);
        actions::bind_keys(cx);
        file_tree::bind_keys(cx);
        app_state::init(cx);
        menus::init(cx);

        #[cfg(unix)]
        if let Some(server) = server {
            ipc::start(server, cx);
        }

        cx.spawn(async move |cx| {
            while let Some(urls) = urls_rx.next().await {
                let paths: Vec<OpenPath> = urls
                    .iter()
                    .filter_map(|u| u.strip_prefix("file://"))
                    .map(|p| OpenPath { path: PathBuf::from(percent_decode(p)), line: None, column: None })
                    .collect();
                if !paths.is_empty() {
                    cx.update(|cx| app_state::open(paths, None, false, cx));
                }
            }
        })
        .detach();

        if !args.paths.is_empty() {
            let request = Request::Open {
                cwd: std::env::current_dir().unwrap_or_default(),
                paths: args.paths,
                stdin: None,
                wait: false,
                new_window: false,
            };
            app_state::handle_request(request, None, cx);
        } else if args.from_cli {
            // The CLI's request is on its way. If it never arrives, still
            // show a window rather than run invisibly.
            cx.spawn(async move |cx| {
                cx.background_executor().timer(Duration::from_secs(3)).await;
                cx.update(|cx| {
                    if app_state::workspaces(cx).is_empty() {
                        app_state::open_window(None, cx);
                    }
                });
            })
            .detach();
        } else {
            app_state::open_window(None, cx);
        }
        cx.activate(true);

        if std::env::var_os("EDITOR_TRACE_STARTUP").is_some() {
            eprintln!("startup: first window requested after {:?}", started.elapsed());
            cx.on_next_frame_hack(started);
        }
    });
}

trait StartupTrace {
    fn on_next_frame_hack(&mut self, started: Instant);
}

impl StartupTrace for App {
    /// Reports when the first frame has been drawn.
    fn on_next_frame_hack(&mut self, started: Instant) {
        if let Some(window) = self.windows().first().copied() {
            window
                .update(self, move |_, window, _| {
                    window.on_next_frame(move |_, _| {
                        eprintln!("startup: first frame after {:?}", started.elapsed());
                    });
                })
                .ok();
        }
    }
}

#[cfg(unix)]
fn forward_to_running(socket: &std::path::Path, paths: Vec<OpenPath>) {
    let request = Request::Open {
        cwd: std::env::current_dir().unwrap_or_default(),
        paths,
        stdin: None,
        wait: false,
        new_window: false,
    };
    match editor_ipc::Client::connect(socket) {
        Ok(mut client) => {
            let _ = client.send(&request);
            let _ = client.recv();
        }
        Err(err) => eprintln!("editor: could not reach the running instance: {err}"),
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len() + 0
            && let Ok(byte) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

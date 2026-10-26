//! Me: a fast, native editor for reading markdown and code.

// Release builds on Windows open no console window next to the app.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod actions;
mod app_state;
mod assets;
mod editor;
mod file_icons;
mod file_tree;
mod find_bar;
mod finder;
mod goto_line;
mod image_preview;
mod ipc;
mod lsp;
mod markdown;
mod menus;
mod mermaid;
mod paint;
mod palette;
mod picker;
mod platform;
mod recent;
mod rename;
mod search_panel;
mod session;
mod settings;
mod symbols;
mod theme;
mod updater;
mod workspace;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::channel::mpsc;
use gpui::App;
use me_ipc::{OpenPath, Request, parse_path_arg};

struct Args {
    /// Launched by the `me` CLI, which will send what to open over the
    /// socket; don't open a default window.
    from_cli: bool,
    paths: Vec<OpenPath>,
}

fn parse_args() -> Args {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let mut args = Args {
        from_cli: false,
        paths: Vec::new(),
    };
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

static STARTED: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

/// Traces a named startup phase (first occurrence only matters).
pub fn trace_phase(label: &str) {
    if let Some(started) = STARTED.get() {
        trace(*started, label);
    }
}

/// Called from the workspace's first render.
pub fn trace_first_frame() {
    static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !DONE.swap(true, std::sync::atomic::Ordering::Relaxed)
        && let Some(started) = STARTED.get()
    {
        trace(*started, "first frame laid out");
    }
}

/// `ME_TRACE_STARTUP=1` prints how long each startup phase took.
fn trace(started: Instant, label: &str) {
    if std::env::var_os("ME_TRACE_STARTUP").is_some() {
        eprintln!(
            "startup: {label:<28} {:>7.1} ms",
            started.elapsed().as_secs_f64() * 1000.
        );
    }
}

fn main() {
    let started = *STARTED.get_or_init(Instant::now);
    let args = parse_args();

    // Single instance: if the app is already running, hand our paths to it.
    #[cfg(unix)]
    let server = {
        let socket = me_ipc::socket_path();
        match me_ipc::Server::bind(&socket) {
            Ok(server) => Some(server),
            Err(me_ipc::BindError::AlreadyRunning) => {
                forward_to_running(&socket, args.paths);
                return;
            }
            Err(err) => {
                eprintln!("me: CLI integration disabled: {err}");
                None
            }
        }
    };

    trace(started, "socket bound");
    let app = gpui_platform::application().with_assets(assets::Assets);
    trace(started, "platform created");

    // Files opened from Finder ("Open With", double click, drag on the Dock).
    let (urls_tx, mut urls_rx) = mpsc::unbounded::<Vec<String>>();
    app.on_open_urls(move |urls| {
        let _ = urls_tx.unbounded_send(urls);
    });
    app.on_reopen(|cx| {
        if app_state::workspaces(cx).is_empty() {
            app_state::restore_or_open(cx);
        }
    });

    app.run(move |cx: &mut App| {
        trace(started, "app running");
        gpui_component::init(cx);
        trace(started, "components initialized");
        settings::init(cx);
        theme::init(cx.window_appearance(), cx);
        trace(started, "theme resolved");
        actions::bind_keys(cx);
        file_tree::bind_keys(cx);
        app_state::init(cx);
        recent::init(cx);
        menus::init(cx);
        updater::init(cx);

        #[cfg(unix)]
        if let Some(server) = server {
            ipc::start(server, cx);
        }

        cx.spawn(async move |cx| {
            while let Some(urls) = urls_rx.next().await {
                let paths: Vec<OpenPath> = urls
                    .iter()
                    .filter_map(|u| u.strip_prefix("file://"))
                    .map(|p| OpenPath {
                        path: PathBuf::from(percent_decode(p)),
                        line: None,
                        column: None,
                    })
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
                        app_state::restore_or_open(cx);
                    }
                });
            })
            .detach();
        } else {
            app_state::restore_or_open(cx);
        }
        trace(started, "app setup done");
        cx.activate(true);
    });
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
    match me_ipc::Client::connect(socket) {
        Ok(mut client) => {
            let _ = client.send(&request);
            let _ = client.recv();
        }
        Err(err) => eprintln!("me: could not reach the running instance: {err}"),
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

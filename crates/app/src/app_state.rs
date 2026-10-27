//! App-wide state: open windows, tab ids, and CLI clients waiting (`-w`) for
//! tabs to close.

use std::collections::HashMap;
use std::path::PathBuf;

use futures::channel::mpsc::UnboundedSender;
use gpui::{
    App, AppContext, Bounds, Entity, Global, Point, TitlebarOptions, WindowBackgroundAppearance,
    WindowBounds, WindowHandle, WindowId, WindowOptions, point, px, size,
};
use gpui_component::Root;
use me_ipc::{OpenPath, Request, Response};

use crate::session;
use crate::workspace::{OpenOptions, Workspace};

pub type Reply = UnboundedSender<Response>;

#[derive(Default)]
pub struct AppState {
    next_tab_id: u64,
    windows: Vec<(WindowHandle<Root>, Entity<Workspace>)>,
    waiters: HashMap<u64, Vec<Reply>>,
    last_origin: Option<Point<gpui::Pixels>>,
    /// The app is quitting: windows closing now stay in the session.
    quitting: bool,
}

impl Global for AppState {}

pub fn init(cx: &mut App) {
    cx.set_global(AppState::default());
    cx.on_window_closed(|cx, window_id| window_closed(window_id, cx))
        .detach();
    // Quitting keeps every window, unsaved text included, for next time.
    cx.on_app_quit(|cx| {
        cx.global_mut::<AppState>().quitting = true;
        for (handle, ws) in workspaces(cx) {
            handle
                .update(cx, |_, window, cx| {
                    ws.update(cx, |ws, cx| ws.save_session(true, true, Some(window), cx))
                })
                .ok();
        }
        async {}
    })
    .detach();
}

/// Whether a `me -w` process is waiting for tab `id` to close.
pub fn is_waited(id: u64, cx: &App) -> bool {
    cx.global::<AppState>().waiters.contains_key(&id)
}

pub fn next_tab_id(cx: &mut App) -> u64 {
    let state = cx.global_mut::<AppState>();
    state.next_tab_id += 1;
    state.next_tab_id
}

/// Tells any waiting CLI that tab `id` is gone.
pub fn tab_closed(id: u64, cx: &mut App) {
    if let Some(waiters) = cx.global_mut::<AppState>().waiters.remove(&id) {
        for reply in waiters {
            let _ = reply.unbounded_send(Response::Closed { id });
        }
    }
}

fn window_closed(window_id: WindowId, cx: &mut App) {
    let state = cx.global_mut::<AppState>();
    let Some(ix) = state
        .windows
        .iter()
        .position(|(h, _)| h.window_id() == window_id)
    else {
        return;
    };
    let (_, workspace) = state.windows.remove(ix);
    // Closing never asks about unsaved files: they stay in the window's
    // session and come back with it.
    let quitting = cx.global::<AppState>().quitting;
    workspace.update(cx, |ws, cx| ws.save_session(quitting, true, None, cx));
    for id in workspace.read(cx).tab_ids() {
        tab_closed(id, cx);
    }
}

pub fn workspaces(cx: &App) -> Vec<(WindowHandle<Root>, Entity<Workspace>)> {
    cx.global::<AppState>().windows.clone()
}

/// Opens a window, for `root` if given, with that folder's saved tabs.
pub fn open_window(
    root: Option<PathBuf>,
    cx: &mut App,
) -> Option<(WindowHandle<Root>, Entity<Workspace>)> {
    let saved = root
        .as_ref()
        .and_then(|root| session::load(&session::key_for_root(root)));
    open_window_with(root, saved, None, cx)
}

/// Brings back the windows open when the app last quit, and windows with
/// unsaved text; with none, opens an empty window.
pub fn restore_or_open(cx: &mut App) {
    let open_keys: Vec<String> = workspaces(cx)
        .iter()
        .map(|(_, ws)| ws.read(cx).session_key.clone())
        .collect();
    let mut restored = 0;
    for (key, saved) in session::list() {
        let wanted = saved.open || (session::is_scratch(&key) && saved.has_unsaved());
        if !wanted || open_keys.contains(&key) {
            continue;
        }
        if saved.root.as_ref().is_some_and(|root| !root.is_dir()) {
            continue;
        }
        let root = saved.root.clone();
        if open_window_with(root, Some(saved), Some(key), cx).is_some() {
            restored += 1;
        }
    }
    if restored == 0 && workspaces(cx).is_empty() {
        open_window(None, cx);
    }
}

/// Opens a window and restores `saved` into it. `key` names the session to
/// keep using (for windows without a folder).
fn open_window_with(
    root: Option<PathBuf>,
    saved: Option<session::WindowSession>,
    key: Option<String>,
    cx: &mut App,
) -> Option<(WindowHandle<Root>, Entity<Workspace>)> {
    let default_size = size(px(1180.), px(780.));
    let mut bounds = Bounds::centered(None, default_size, cx);
    // Cascade new windows like every native app.
    if let Some(last) = cx.global::<AppState>().last_origin
        && !cx.global::<AppState>().windows.is_empty()
    {
        bounds.origin = point(last.x + px(26.), last.y + px(26.));
    }
    // Where it was last time, if that's still on a screen.
    if let Some([x, y, w, h]) = saved.as_ref().and_then(|s| s.bounds) {
        let previous = Bounds::new(point(px(x), px(y)), size(px(w), px(h)));
        let visible = cx
            .displays()
            .iter()
            .any(|d| d.bounds().intersects(&previous));
        if visible && w >= 480. && h >= 320. {
            bounds = previous;
        }
    }
    cx.global_mut::<AppState>().last_origin = Some(bounds.origin);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some("Me".into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(13.), px(13.))),
        }),
        window_background: if cfg!(target_os = "macos") {
            WindowBackgroundAppearance::Blurred
        } else {
            WindowBackgroundAppearance::Opaque
        },
        window_min_size: Some(size(px(480.), px(320.))),
        app_id: Some("me".into()),
        ..Default::default()
    };
    let mut workspace = None;
    let handle = cx
        .open_window(options, |window, cx| {
            crate::trace_phase("platform window created");
            let ws = cx.new(|cx| Workspace::new(root, window, cx));
            ws.update(cx, |ws, cx| {
                if let Some(key) = key {
                    ws.session_key = key;
                }
                if let Some(saved) = saved {
                    ws.restore_session(saved, window, cx);
                }
            });
            workspace = Some(ws.clone());
            window.focus(&ws.read(cx).focus_handle_for_window(), cx);
            crate::trace_phase("workspace built");
            cx.new(|cx| Root::new(ws, window, cx))
        })
        .ok()?;
    let workspace = workspace?;
    cx.global_mut::<AppState>()
        .windows
        .push((handle, workspace.clone()));
    Some((handle, workspace))
}

/// The workspace to open a file in: the window whose folder contains it,
/// else the active window, else the most recent one.
fn target_for_file(path: &PathBuf, cx: &App) -> Option<(WindowHandle<Root>, Entity<Workspace>)> {
    let windows = workspaces(cx);
    let by_root = windows
        .iter()
        .filter(|(_, ws)| ws.read(cx).root().is_some_and(|r| path.starts_with(r)))
        .max_by_key(|(_, ws)| ws.read(cx).root().map(|r| r.components().count()))
        .cloned();
    if by_root.is_some() {
        return by_root;
    }
    let active = cx.active_window().map(|w| w.window_id());
    windows
        .iter()
        .find(|(h, _)| Some(h.window_id()) == active)
        .or(windows.last())
        .cloned()
}

/// Handles one CLI (or Finder) request. Replies go to `reply` if given.
pub fn handle_request(request: Request, reply: Option<Reply>, cx: &mut App) {
    match request {
        Request::Ping => {
            if let Some(reply) = reply {
                let _ = reply.unbounded_send(Response::Pong);
            }
        }
        Request::Open {
            cwd: _,
            paths,
            stdin,
            wait,
            new_window,
        } => {
            let ids = open(paths, stdin, new_window, cx);
            if let Some(reply) = reply {
                if wait {
                    let state = cx.global_mut::<AppState>();
                    for id in &ids {
                        state.waiters.entry(*id).or_default().push(reply.clone());
                    }
                }
                let _ = reply.unbounded_send(Response::Opened {
                    ids: if wait { ids } else { Vec::new() },
                });
            }
            cx.activate(true);
        }
    }
}

/// Opens folders (each in its own window, reusing one already showing it)
/// and files. Returns the tab ids of the opened files.
pub fn open(
    paths: Vec<OpenPath>,
    stdin: Option<String>,
    new_window: bool,
    cx: &mut App,
) -> Vec<u64> {
    crate::recent::note(
        paths
            .iter()
            .filter(|p| p.path.exists())
            .map(|p| p.path.clone()),
        cx,
    );
    let (dirs, files): (Vec<OpenPath>, Vec<OpenPath>) =
        paths.into_iter().partition(|p| p.path.is_dir());
    let mut target: Option<(WindowHandle<Root>, Entity<Workspace>)> = None;

    for dir in dirs {
        let existing = (!new_window)
            .then(|| {
                workspaces(cx)
                    .into_iter()
                    .find(|(_, ws)| ws.read(cx).root() == Some(dir.path.as_path()))
            })
            .flatten();
        let window = match existing {
            Some(found) => Some(found),
            None => {
                // Reuse an empty window without a folder.
                let empty = (!new_window)
                    .then(|| {
                        workspaces(cx).into_iter().find(|(_, ws)| {
                            let ws = ws.read(cx);
                            ws.root().is_none() && ws.tab_ids().is_empty()
                        })
                    })
                    .flatten();
                match empty {
                    Some((handle, ws)) => {
                        let path = dir.path.clone();
                        let saved = session::load(&session::key_for_root(&path));
                        handle
                            .update(cx, |_, window, cx| {
                                ws.update(cx, |ws, cx| {
                                    ws.set_root(path, window, cx);
                                    if let Some(saved) = saved {
                                        ws.restore_session(saved, window, cx);
                                    }
                                })
                            })
                            .ok();
                        Some((handle, ws))
                    }
                    None => open_window(Some(dir.path.clone()), cx),
                }
            }
        };
        if let Some((handle, _)) = &window {
            handle
                .update(cx, |_, window, _| window.activate_window())
                .ok();
        }
        target = window.or(target);
    }

    let mut ids = Vec::new();
    for file in files {
        let window = match (&target, new_window) {
            (Some(t), _) => Some(t.clone()),
            (None, true) => open_window(None, cx),
            (None, false) => target_for_file(&file.path, cx).or_else(|| open_window(None, cx)),
        };
        let Some((handle, ws)) = window else { continue };
        let options = OpenOptions {
            pin: true,
            line: file.line,
            column: file.column,
            ..Default::default()
        };
        let id = handle
            .update(cx, |_, window, cx| {
                window.activate_window();
                ws.update(cx, |ws, cx| {
                    ws.open_path(file.path.clone(), options, window, cx)
                })
            })
            .ok();
        ids.extend(id);
        target = Some((handle, ws));
    }

    if let Some(text) = stdin {
        let window = target
            .clone()
            .or_else(|| workspaces(cx).last().cloned())
            .or_else(|| open_window(None, cx));
        if let Some((handle, ws)) = window {
            let id = handle
                .update(cx, |_, window, cx| {
                    ws.update(cx, |ws, cx| ws.open_text(&text, window, cx))
                })
                .ok();
            ids.extend(id);
            target = Some((handle, ws));
        }
    }

    if target.is_none() {
        // Plain `me`: show something.
        match workspaces(cx).last().cloned() {
            Some((handle, _)) => {
                handle
                    .update(cx, |_, window, _| window.activate_window())
                    .ok();
            }
            None => restore_or_open(cx),
        }
    }
    ids
}

/// Cmd+Q. Nothing to ask: every window's unsaved text is kept in its
/// session (see `init`).
pub fn quit(cx: &mut App) {
    cx.quit();
}

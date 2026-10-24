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

use crate::workspace::{OpenOptions, Workspace};

pub type Reply = UnboundedSender<Response>;

#[derive(Default)]
pub struct AppState {
    next_tab_id: u64,
    windows: Vec<(WindowHandle<Root>, Entity<Workspace>)>,
    waiters: HashMap<u64, Vec<Reply>>,
    last_origin: Option<Point<gpui::Pixels>>,
}

impl Global for AppState {}

pub fn init(cx: &mut App) {
    cx.set_global(AppState::default());
    cx.on_window_closed(|cx, window_id| window_closed(window_id, cx))
        .detach();
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
    for id in workspace.read(cx).tab_ids() {
        tab_closed(id, cx);
    }
}

pub fn workspaces(cx: &App) -> Vec<(WindowHandle<Root>, Entity<Workspace>)> {
    cx.global::<AppState>().windows.clone()
}

pub fn open_window(
    root: Option<PathBuf>,
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
            workspace = Some(ws.clone());
            let guarded = ws.downgrade();
            window.on_window_should_close(cx, move |window, cx| {
                let Some(ws) = guarded.upgrade() else {
                    return true;
                };
                confirm_close(&ws, window, cx)
            });
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
                        handle
                            .update(cx, |_, window, cx| {
                                ws.update(cx, |ws, cx| ws.set_root(path, window, cx))
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
            None => {
                open_window(None, cx);
            }
        }
    }
    ids
}

/// Asks before closing a window with unsaved tabs. Returns whether to close
/// right away; after a "Close" answer the window is removed explicitly.
fn confirm_close(ws: &Entity<Workspace>, window: &mut gpui::Window, cx: &mut App) -> bool {
    let dirty = ws.read(cx).dirty_count(cx);
    if dirty == 0 || ws.read(cx).close_confirmed {
        return true;
    }
    let message = if dirty == 1 {
        "Close the window with 1 unsaved file?".to_string()
    } else {
        format!("Close the window with {dirty} unsaved files?")
    };
    let answer = window.prompt(
        gpui::PromptLevel::Warning,
        &message,
        Some("Your changes will be lost."),
        &["Close", "Cancel"],
        cx,
    );
    let ws = ws.downgrade();
    window
        .spawn(cx, async move |cx| {
            if answer.await.ok() == Some(0) {
                cx.update(|window, cx| {
                    if let Some(ws) = ws.upgrade() {
                        ws.update(cx, |ws, _| ws.close_confirmed = true);
                    }
                    window.remove_window();
                })
                .ok();
            }
        })
        .detach();
    false
}

/// Cmd+Q: asks first if any window has unsaved files.
pub fn quit(cx: &mut App) {
    let windows = workspaces(cx);
    let dirty: usize = windows
        .iter()
        .map(|(_, ws)| ws.read(cx).dirty_count(cx))
        .sum();
    if dirty == 0 {
        cx.quit();
        return;
    }
    let Some((handle, _)) = windows
        .iter()
        .find(|(_, ws)| ws.read(cx).dirty_count(cx) > 0)
        .cloned()
    else {
        cx.quit();
        return;
    };
    handle
        .update(cx, |_, window, cx| {
            window.activate_window();
            let answer = window.prompt(
                gpui::PromptLevel::Warning,
                &format!(
                    "Quit with {dirty} unsaved file{}?",
                    if dirty == 1 { "" } else { "s" }
                ),
                Some("Your changes will be lost."),
                &["Quit", "Cancel"],
                cx,
            );
            window
                .spawn(cx, async move |cx| {
                    if answer.await.ok() == Some(0) {
                        cx.update(|_, cx| cx.quit()).ok();
                    }
                })
                .detach();
        })
        .ok();
}

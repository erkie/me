//! The native menu bar and app-level actions.

use std::path::PathBuf;

use editor_ipc::OpenPath;
use gpui::{App, Menu, MenuItem, OsAction, PathPromptOptions, PromptLevel, SystemMenuType};

use crate::actions::*;
use crate::app_state;
use crate::platform;

pub fn init(cx: &mut App) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &NewWindow, cx| {
        app_state::open_window(None, cx);
    });
    cx.on_action(|_: &Open, cx| prompt_open(true, cx));
    cx.on_action(|_: &OpenFolder, cx| prompt_open(false, cx));
    cx.on_action(|_: &CloseWindow, cx| {
        if let Some(window) = cx.active_window() {
            window.update(cx, |_, window, _| window.remove_window()).ok();
        }
    });
    cx.on_action(|_: &Minimize, cx| {
        if let Some(window) = cx.active_window() {
            window.update(cx, |_, window, _| window.minimize_window()).ok();
        }
    });
    cx.on_action(|_: &Zoom, cx| {
        if let Some(window) = cx.active_window() {
            window.update(cx, |_, window, _| window.zoom_window()).ok();
        }
    });
    cx.on_action(|_: &About, cx| {
        if let Some(window) = cx.active_window() {
            window
                .update(cx, |_, window, cx| {
                    let _ = window.prompt(
                        PromptLevel::Info,
                        &format!("Editor {}", env!("CARGO_PKG_VERSION")),
                        Some("A fast, native editor for reading markdown and code."),
                        &["OK"],
                        cx,
                    );
                })
                .ok();
        }
    });
    cx.on_action(|_: &InstallCli, cx| platform::install_cli(cx));
    cx.on_action(|_: &MakeDefaultEditor, cx| platform::make_default_editor(cx));
    set_menus(cx);
}

fn prompt_open(files: bool, cx: &mut App) {
    let rx = cx.prompt_for_paths(PathPromptOptions {
        files,
        directories: true,
        multiple: true,
        prompt: Some("Open".into()),
    });
    cx.spawn(async move |cx| {
        let Ok(Ok(Some(paths))) = rx.await else { return };
        cx.update(|cx| {
            let paths: Vec<OpenPath> = paths
                .into_iter()
                .map(|path: PathBuf| OpenPath { path, line: None, column: None })
                .collect();
            app_state::open(paths, None, false, cx);
        });
    })
    .detach();
}

fn set_menus(cx: &mut App) {
    cx.set_menus([
        Menu::new("Editor").items([
            MenuItem::action("About Editor", About),
            MenuItem::separator(),
            MenuItem::action("Install 'editor' Command…", InstallCli),
            MenuItem::action("Make Default Editor…", MakeDefaultEditor),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide Editor", Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
            MenuItem::action("Quit Editor", Quit),
        ]),
        Menu::new("File").items([
            MenuItem::action("New File", NewFile),
            MenuItem::action("New Window", NewWindow),
            MenuItem::separator(),
            MenuItem::action("Open…", Open),
            MenuItem::action("Open Folder…", OpenFolder),
            MenuItem::action("Go to File…", ToggleFileFinder),
            MenuItem::action("Reopen Closed Tab", ReopenClosedTab),
            MenuItem::separator(),
            MenuItem::action("Save", Save),
            MenuItem::action("Save As…", SaveAs),
            MenuItem::separator(),
            MenuItem::action("Close Tab", CloseTab),
            MenuItem::action("Close Window", CloseWindow),
        ]),
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", Undo, OsAction::Undo),
            MenuItem::os_action("Redo", Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", Cut, OsAction::Cut),
            MenuItem::os_action("Copy", Copy, OsAction::Copy),
            MenuItem::os_action("Paste", Paste, OsAction::Paste),
            MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
            MenuItem::separator(),
            MenuItem::action("Toggle Comment", ToggleComment),
            MenuItem::action("Duplicate Line", DuplicateLine),
            MenuItem::action("Delete Line", DeleteLine),
            MenuItem::action("Move Line Up", MoveLineUp),
            MenuItem::action("Move Line Down", MoveLineDown),
            MenuItem::action("Indent", IndentLines),
            MenuItem::action("Outdent", OutdentLines),
        ]),
        Menu::new("Selection").items([
            MenuItem::action("Expand Selection to Line", SelectLine),
            MenuItem::action("Split into Lines", SplitIntoLines),
            MenuItem::separator(),
            MenuItem::action("Add Next Occurrence", AddNextOccurrence),
            MenuItem::action("Skip Occurrence", SkipOccurrence),
            MenuItem::action("Select All Occurrences", SelectAllOccurrences),
            MenuItem::separator(),
            MenuItem::action("Add Cursor Above", AddCursorAbove),
            MenuItem::action("Add Cursor Below", AddCursorBelow),
            MenuItem::action("Single Selection", Cancel),
        ]),
        Menu::new("Find").items([
            MenuItem::action("Find…", Find),
            MenuItem::action("Find Next", FindNext),
            MenuItem::action("Find Previous", FindPrevious),
            MenuItem::action("Use Selection for Find", UseSelectionForFind),
            MenuItem::separator(),
            MenuItem::action("Find in Project…", ToggleProjectSearch),
        ]),
        Menu::new("View").items([
            MenuItem::action("Toggle Sidebar", ToggleSidebar),
            MenuItem::action("Reveal in Sidebar", RevealInSidebar),
            MenuItem::action("Toggle Markdown Preview", TogglePreview),
        ]),
        Menu::new("Window").items([
            MenuItem::action("Minimize", Minimize),
            MenuItem::action("Zoom", Zoom),
            MenuItem::separator(),
            MenuItem::action("Next Tab", NextTab),
            MenuItem::action("Previous Tab", PreviousTab),
        ]),
    ]);
}

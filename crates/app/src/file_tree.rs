//! The sidebar file tree: lazily listed, virtualized, ignore-aware, with a
//! context menu for creating, renaming and trashing files.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use gpui::{
    App, AppContext, ClickEvent, ClipboardItem, Context, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    ParentElement, PromptLevel, Render, ScrollStrategy, SharedString, StatefulInteractiveElement,
    Styled, Subscription, UniformListScrollHandle, Window, actions, div, prelude::FluentBuilder,
    px, svg, uniform_list,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::native_menu::NativeMenu;
use me_project::{DirEntry, list_dir};

use crate::actions::{CopyPath, CopyRelativePath, RevealInFinder};
use crate::{file_icons, platform, theme};

actions!(
    file_tree,
    [
        SelectPrev,
        SelectNext,
        Expand,
        Collapse,
        OpenSelected,
        NewFileHere,
        NewFolderHere,
        RenameEntry,
        TrashEntry,
        OpenWithDefaultApp,
    ]
);

pub const ROW_HEIGHT: f32 = 26.;

pub enum FileTreeEvent {
    /// Open a file. `pin` is false for a single click (preview tab).
    Open { path: PathBuf, pin: bool },
    /// A file or folder was renamed; open tabs follow it.
    Renamed { from: PathBuf, to: PathBuf },
    /// A file or folder went to the trash.
    Trashed(PathBuf),
}

#[derive(Clone)]
struct Row {
    entry: DirEntry,
    depth: usize,
    /// The name field for a file or folder being created.
    pending: bool,
}

enum EditKind {
    NewFile,
    NewFolder,
    Rename(PathBuf),
}

/// A name being typed in the tree: a new entry in `dir`, or a rename.
struct Editing {
    kind: EditKind,
    dir: PathBuf,
    input: Entity<InputState>,
    _subscription: Subscription,
}

pub struct FileTree {
    root: PathBuf,
    focus_handle: FocusHandle,
    scroll: UniformListScrollHandle,
    listings: HashMap<PathBuf, Vec<DirEntry>>,
    expanded: HashSet<PathBuf>,
    rows: Vec<Row>,
    selected: Option<PathBuf>,
    editing: Option<Editing>,
}

impl EventEmitter<FileTreeEvent> for FileTree {}

impl Focusable for FileTree {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl FileTree {
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            root: root.clone(),
            focus_handle: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            listings: HashMap::new(),
            expanded: HashSet::new(),
            rows: Vec::new(),
            selected: None,
            editing: None,
        };
        this.expanded.insert(root);
        this.rebuild();
        this
    }

    fn listing(&mut self, dir: &Path) -> Vec<DirEntry> {
        if let Some(entries) = self.listings.get(dir) {
            return entries.clone();
        }
        let entries = list_dir(dir).unwrap_or_default();
        self.listings.insert(dir.to_path_buf(), entries.clone());
        entries
    }

    fn rebuild(&mut self) {
        // Depth-first, keeping directory order. A name being created shows
        // as the first row of its folder.
        fn walk(this: &mut FileTree, dir: &Path, depth: usize, rows: &mut Vec<Row>) {
            if let Some(editing) = &this.editing
                && editing.dir == dir
                && !matches!(editing.kind, EditKind::Rename(_))
            {
                rows.push(Row {
                    entry: DirEntry {
                        name: String::new(),
                        path: dir.join(""),
                        is_dir: matches!(editing.kind, EditKind::NewFolder),
                        is_symlink: false,
                        ignored: false,
                    },
                    depth,
                    pending: true,
                });
            }
            for entry in this.listing(dir) {
                let expanded = entry.is_dir && this.expanded.contains(&entry.path);
                let path = entry.path.clone();
                rows.push(Row {
                    entry,
                    depth,
                    pending: false,
                });
                if expanded {
                    walk(this, &path, depth + 1, rows);
                }
            }
        }
        let mut rows = Vec::new();
        let root = self.root.clone();
        walk(self, &root, 0, &mut rows);
        self.rows = rows;
    }

    /// Re-lists every expanded directory (after the window regains focus,
    /// a save, or a file operation).
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.listings.clear();
        self.rebuild();
        cx.notify();
    }

    fn toggle(&mut self, path: &Path, cx: &mut Context<Self>) {
        if !self.expanded.remove(path) {
            self.expanded.insert(path.to_path_buf());
        }
        self.rebuild();
        cx.notify();
    }

    /// Expands the parents of `path` and selects it.
    pub fn reveal(&mut self, path: &Path, cx: &mut Context<Self>) {
        if !path.starts_with(&self.root) {
            return;
        }
        let mut changed = false;
        for ancestor in path.ancestors().skip(1) {
            if !ancestor.starts_with(&self.root) {
                break;
            }
            changed |= self.expanded.insert(ancestor.to_path_buf());
        }
        if changed {
            self.rebuild();
        }
        self.selected = Some(path.to_path_buf());
        if let Some(ix) = self.rows.iter().position(|r| r.entry.path == path) {
            self.scroll.scroll_to_item(ix, ScrollStrategy::Center);
        }
        cx.notify();
    }

    pub fn select_quietly(&mut self, path: Option<&Path>, cx: &mut Context<Self>) {
        let path = path.map(Path::to_path_buf);
        if self.selected != path {
            self.selected = path;
            cx.notify();
        }
    }

    fn click(
        &mut self,
        ix: usize,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(row) = self.rows.get(ix).cloned() else {
            return;
        };
        if row.pending {
            return;
        }
        window.focus(&self.focus_handle, cx);
        self.selected = Some(row.entry.path.clone());
        if row.entry.is_dir {
            self.toggle(&row.entry.path, cx);
        } else {
            let pin = event.click_count() >= 2;
            cx.emit(FileTreeEvent::Open {
                path: row.entry.path,
                pin,
            });
        }
        cx.notify();
    }

    fn selected_ix(&self) -> Option<usize> {
        let selected = self.selected.as_ref()?;
        self.rows.iter().position(|r| &r.entry.path == selected)
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            return;
        }
        let ix = match self.selected_ix() {
            Some(ix) => (ix as isize + delta).clamp(0, self.rows.len() as isize - 1) as usize,
            None => 0,
        };
        self.selected = Some(self.rows[ix].entry.path.clone());
        self.scroll.scroll_to_item(ix, ScrollStrategy::Top);
        cx.notify();
    }

    fn expand_selected(&mut self, expand: bool, cx: &mut Context<Self>) {
        let Some(ix) = self.selected_ix() else { return };
        let row = self.rows[ix].clone();
        if row.entry.is_dir {
            let is_expanded = self.expanded.contains(&row.entry.path);
            if expand != is_expanded {
                self.toggle(&row.entry.path, cx);
                return;
            }
        }
        if !expand && let Some(parent) = row.entry.path.parent().filter(|p| *p != self.root) {
            self.selected = Some(parent.to_path_buf());
            cx.notify();
        }
    }

    fn open_selected(&mut self, cx: &mut Context<Self>) {
        let Some(ix) = self.selected_ix() else { return };
        let row = self.rows[ix].clone();
        if row.entry.is_dir {
            self.toggle(&row.entry.path, cx);
        } else {
            cx.emit(FileTreeEvent::Open {
                path: row.entry.path,
                pin: true,
            });
        }
    }

    // ---------------------------------------------------------------------
    // Context menu and file operations
    // ---------------------------------------------------------------------

    /// What file operations act on: the selection, or the folder itself.
    fn target(&self) -> PathBuf {
        self.selected.clone().unwrap_or_else(|| self.root.clone())
    }

    /// The folder new entries go into.
    fn target_dir(&self) -> PathBuf {
        let target = self.target();
        if target.is_dir() {
            target
        } else {
            target
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| self.root.clone())
        }
    }

    fn show_menu(
        &mut self,
        path: Option<PathBuf>,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        self.selected = path.clone();
        cx.notify();
        let is_root = path.is_none();
        let is_file = path.as_ref().is_some_and(|p| !p.is_dir());
        let mut menu = NativeMenu::new()
            .menu("New File…", Box::new(NewFileHere))
            .menu("New Folder…", Box::new(NewFolderHere))
            .separator()
            .menu(platform::REVEAL_LABEL, Box::new(RevealInFinder));
        if is_file {
            menu = menu.menu("Open with Default App", Box::new(OpenWithDefaultApp));
        }
        menu = menu
            .separator()
            .menu("Copy Path", Box::new(CopyPath))
            .menu("Copy Relative Path", Box::new(CopyRelativePath));
        if !is_root {
            menu = menu
                .separator()
                .menu("Rename…", Box::new(RenameEntry))
                .menu("Move to Trash", Box::new(TrashEntry));
        }
        menu.show(event.position, window, cx);
    }

    fn begin_edit(&mut self, kind: EditKind, window: &mut Window, cx: &mut Context<Self>) {
        let (dir, value) = match &kind {
            EditKind::Rename(path) => (
                path.parent().unwrap_or(&self.root).to_path_buf(),
                path.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            ),
            _ => (self.target_dir(), String::new()),
        };
        let placeholder = match kind {
            EditKind::NewFolder => "Folder name",
            _ => "File name",
        };
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder(placeholder);
            state.set_value(value.clone(), window, cx);
            // Select the name without its extension, as Finder does.
            let stem = match value.rfind('.') {
                Some(dot) if dot > 0 => dot,
                _ => value.len(),
            };
            state.set_selected_range(0..stem, cx);
            state
        });
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } | InputEvent::Blur => this.commit(window, cx),
                _ => {}
            },
        );
        if dir != self.root {
            self.expanded.insert(dir.clone());
        }
        self.editing = Some(Editing {
            kind,
            dir,
            input: input.clone(),
            _subscription: subscription,
        });
        self.rebuild();
        if let Some(ix) = self
            .rows
            .iter()
            .position(|r| r.pending || self.is_renaming(&r.entry.path))
        {
            self.scroll.scroll_to_item(ix, ScrollStrategy::Center);
        }
        input.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn is_renaming(&self, path: &Path) -> bool {
        matches!(&self.editing, Some(Editing { kind: EditKind::Rename(p), .. }) if p == path)
    }

    fn cancel_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing.take().is_some() {
            self.rebuild();
            window.focus(&self.focus_handle, cx);
            cx.notify();
        }
    }

    fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editing) = self.editing.take() else {
            return;
        };
        let name = editing.input.read(cx).value().trim().to_string();
        window.focus(&self.focus_handle, cx);
        let unchanged = matches!(&editing.kind, EditKind::Rename(from)
            if from.file_name().is_some_and(|n| n.to_string_lossy() == name));
        if name.is_empty() || unchanged {
            self.rebuild();
            cx.notify();
            return;
        }
        let path = editing.dir.join(&name);
        let result = match &editing.kind {
            EditKind::NewFile => create_file(&path),
            EditKind::NewFolder => std::fs::create_dir_all(&path).map_err(|e| e.to_string()),
            EditKind::Rename(from) => rename(from, &path),
        };
        match result {
            Ok(()) => {
                match editing.kind {
                    EditKind::NewFile => cx.emit(FileTreeEvent::Open {
                        path: path.clone(),
                        pin: true,
                    }),
                    EditKind::NewFolder => {
                        self.expanded.insert(path.clone());
                    }
                    EditKind::Rename(from) => {
                        // Keep renamed folders expanded.
                        let moved: Vec<PathBuf> = self
                            .expanded
                            .iter()
                            .filter(|p| p.starts_with(&from))
                            .cloned()
                            .collect();
                        for old in moved {
                            self.expanded.remove(&old);
                            if let Ok(rest) = old.strip_prefix(&from) {
                                self.expanded.insert(path.join(rest));
                            }
                        }
                        cx.emit(FileTreeEvent::Renamed {
                            from,
                            to: path.clone(),
                        });
                    }
                }
                self.listings.clear();
                self.rebuild();
                self.reveal(&path, cx);
            }
            Err(err) => {
                self.rebuild();
                let _ = window.prompt(
                    PromptLevel::Critical,
                    &format!("Couldn't create or rename “{name}”"),
                    Some(&err),
                    &["OK"],
                    cx,
                );
            }
        }
        cx.notify();
    }

    fn trash(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.selected.clone().filter(|p| *p != self.root) else {
            return;
        };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Move “{name}” to the Trash?"),
            Some("You can restore it from the Trash."),
            &["Move to Trash", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await.ok() != Some(0) {
                return;
            }
            let result = trash::delete(&path);
            this.update_in(cx, |this, window, cx| match result {
                Ok(()) => {
                    this.selected = None;
                    this.refresh(cx);
                    cx.emit(FileTreeEvent::Trashed(path));
                }
                Err(err) => {
                    let _ = window.prompt(
                        PromptLevel::Critical,
                        &format!("Couldn't move “{name}” to the Trash"),
                        Some(&err.to_string()),
                        &["OK"],
                        cx,
                    );
                }
            })
            .ok();
        })
        .detach();
    }

    fn relative(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned()
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing.is_some() && event.keystroke.key == "escape" {
            self.cancel_edit(window, cx);
            cx.stop_propagation();
        }
    }
}

fn create_file(path: &Path) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map(drop)
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::AlreadyExists => "A file with that name already exists.".into(),
            _ => e.to_string(),
        })
}

fn rename(from: &Path, to: &Path) -> Result<(), String> {
    // Allow changing only the case of a name on case-insensitive disks.
    let same_file = from.to_string_lossy().to_lowercase() == to.to_string_lossy().to_lowercase();
    if to.exists() && !same_file {
        return Err("Something with that name already exists.".into());
    }
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::rename(from, to).map_err(|e| e.to_string())
}

pub fn bind_keys(cx: &mut App) {
    let ctx = Some("FileTree");
    cx.bind_keys([
        gpui::KeyBinding::new("up", SelectPrev, ctx),
        gpui::KeyBinding::new("down", SelectNext, ctx),
        gpui::KeyBinding::new("right", Expand, ctx),
        gpui::KeyBinding::new("left", Collapse, ctx),
        gpui::KeyBinding::new("enter", OpenSelected, ctx),
        gpui::KeyBinding::new("secondary-down", OpenSelected, ctx),
        gpui::KeyBinding::new("f2", RenameEntry, ctx),
        gpui::KeyBinding::new("secondary-backspace", TrashEntry, ctx),
        gpui::KeyBinding::new("delete", TrashEntry, ctx),
    ]);
}

impl Render for FileTree {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::palette(cx).clone();
        let focused = self.focus_handle.contains_focused(window, cx);
        let rows = self.rows.clone();
        let selected = self.selected.clone();
        let expanded = self.expanded.clone();
        let editing = self.editing.as_ref().map(|e| {
            let renaming = match &e.kind {
                EditKind::Rename(path) => Some(path.clone()),
                _ => None,
            };
            (renaming, e.input.clone())
        });
        div()
            .id("file-tree-container")
            .key_context("FileTree")
            .track_focus(&self.focus_handle)
            .capture_key_down(cx.listener(Self::on_key))
            // While a name is being typed, keys belong to the name field.
            .when(self.editing.is_none(), |el| {
                el.on_action(cx.listener(|this, _: &SelectPrev, _, cx| this.move_selection(-1, cx)))
                    .on_action(
                        cx.listener(|this, _: &SelectNext, _, cx| this.move_selection(1, cx)),
                    )
                    .on_action(
                        cx.listener(|this, _: &Expand, _, cx| this.expand_selected(true, cx)),
                    )
                    .on_action(
                        cx.listener(|this, _: &Collapse, _, cx| this.expand_selected(false, cx)),
                    )
                    .on_action(cx.listener(|this, _: &OpenSelected, _, cx| this.open_selected(cx)))
                    .on_action(
                        cx.listener(|this, _: &TrashEntry, window, cx| this.trash(window, cx)),
                    )
            })
            .on_action(cx.listener(|this, _: &NewFileHere, window, cx| {
                this.begin_edit(EditKind::NewFile, window, cx)
            }))
            .on_action(cx.listener(|this, _: &NewFolderHere, window, cx| {
                this.begin_edit(EditKind::NewFolder, window, cx)
            }))
            .on_action(cx.listener(|this, _: &RenameEntry, window, cx| {
                if let Some(path) = this.selected.clone().filter(|p| *p != this.root) {
                    this.begin_edit(EditKind::Rename(path), window, cx)
                }
            }))
            .on_action(cx.listener(|this, _: &RevealInFinder, _, cx| {
                cx.reveal_path(&this.target());
            }))
            .on_action(cx.listener(|this, _: &OpenWithDefaultApp, _, cx| {
                cx.open_with_system(&this.target());
            }))
            .on_action(cx.listener(|this, _: &CopyPath, _, cx| {
                let path = this.target().to_string_lossy().into_owned();
                cx.write_to_clipboard(ClipboardItem::new_string(path));
            }))
            .on_action(cx.listener(|this, _: &CopyRelativePath, _, cx| {
                let path = this.relative(&this.target());
                cx.write_to_clipboard(ClipboardItem::new_string(path));
            }))
            // Right-click on empty space: the menu for the folder itself.
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.show_menu(None, event, window, cx)
                }),
            )
            .size_full()
            .child(
                uniform_list(
                    "file-tree",
                    rows.len(),
                    cx.processor(move |_this, range: std::ops::Range<usize>, _window, cx| {
                        range
                            .map(|ix| {
                                let row = &rows[ix];
                                let is_selected =
                                    !row.pending && selected.as_ref() == Some(&row.entry.path);
                                let is_expanded = expanded.contains(&row.entry.path);
                                let input = editing.as_ref().and_then(|(renaming, input)| {
                                    let editing_row = if row.pending {
                                        true
                                    } else {
                                        renaming.as_ref() == Some(&row.entry.path)
                                    };
                                    editing_row.then(|| input.clone())
                                });
                                render_row(
                                    ix,
                                    row,
                                    is_selected,
                                    is_expanded,
                                    focused,
                                    input,
                                    &palette,
                                    cx,
                                )
                            })
                            .collect()
                    }),
                )
                .track_scroll(&self.scroll)
                .size_full()
                .pb(px(8.)),
            )
    }
}

#[allow(clippy::too_many_arguments)]
fn render_row(
    ix: usize,
    row: &Row,
    selected: bool,
    expanded: bool,
    focused: bool,
    input: Option<Entity<InputState>>,
    palette: &theme::Palette,
    cx: &mut Context<FileTree>,
) -> gpui::AnyElement {
    let entry = &row.entry;
    let text_color = if entry.ignored {
        palette.ignored_text
    } else {
        palette.text
    };
    let chevron = if expanded {
        "icons/chevron-down.svg"
    } else {
        "icons/chevron-right.svg"
    };
    // Folders get a disclosure chevron, files their type icon in the same
    // column (the Seti layout VS Code and Cursor use).
    let (icon_path, icon_color, icon_size): (SharedString, _, f32) = if entry.is_dir {
        (chevron.into(), palette.text_tertiary, 11.)
    } else {
        let icon = file_icons::for_name(&entry.name, palette.is_dark);
        let color = if entry.ignored {
            icon.color.opacity(0.5)
        } else {
            icon.color
        };
        (icon.path, color, 24.)
    };
    let name: SharedString = entry.name.clone().into();
    // The hovered element needs an id: GPUI only repaints on hover changes
    // for elements that keep state.
    let row_el = div()
        .id("row")
        .w_full()
        .h(px(ROW_HEIGHT))
        .pl(px(6. + row.depth as f32 * 14.))
        .pr(px(8.))
        .flex()
        .items_center()
        .gap(px(5.))
        .rounded(px(5.))
        .text_size(px(14.))
        .text_color(text_color)
        .when(selected, |el| {
            el.bg(if focused {
                palette.row_selected
            } else {
                palette.row_selected_inactive
            })
        })
        .when(!selected && input.is_none(), |el| {
            el.hover(|s| s.bg(palette.hover))
        })
        .child(
            div()
                .w(px(18.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    svg()
                        .path(icon_path)
                        .size(px(icon_size))
                        .flex_none()
                        .text_color(icon_color),
                ),
        )
        .child(match input {
            Some(input) => div()
                .flex_1()
                .min_w(px(0.))
                .h(px(22.))
                .px(px(4.))
                .flex()
                .items_center()
                .rounded(px(4.))
                .border_1()
                .border_color(palette.accent)
                .bg(palette.editor_bg)
                .child(Input::new(&input).appearance(false).text_size(px(14.)))
                .into_any_element(),
            None => div()
                .flex_1()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(name)
                .into_any_element(),
        });
    let path = (!row.pending).then(|| entry.path.clone());
    div()
        .id(ix)
        .w_full()
        .px(px(8.))
        .child(row_el)
        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
            this.click(ix, event, window, cx)
        }))
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                cx.stop_propagation();
                if let Some(path) = path.clone() {
                    this.show_menu(Some(path), event, window, cx);
                }
            }),
        )
        .into_any_element()
}

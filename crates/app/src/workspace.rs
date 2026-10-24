//! One window: sidebar (files or project search), tabs, the active editor or
//! markdown preview, the find bar, the Cmd+T finder and a status bar.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use me_lsp::Status;
use me_syntax::SymbolKind;

use gpui::{
    AnyElement, App, AppContext, ClickEvent, Context, CursorStyle, Entity, FocusHandle, Focusable,
    InteractiveElement, IntoElement, KeyContext, MouseButton, MouseDownEvent, MouseMoveEvent,
    ParentElement, Pixels, Point, PromptLevel, Render, SharedString, StatefulInteractiveElement,
    Styled, Subscription, Task, Window, anchored, deferred, div, prelude::FluentBuilder, px, svg,
};

use crate::actions::*;
use crate::app_state;
use crate::editor::diagnostics::Severity;
use crate::editor::{EditorEvent, EditorView};
use crate::file_tree::{FileTree, FileTreeEvent};
use crate::find_bar::{FindBar, FindBarEvent};
use crate::finder::{FinderEvent, FinderView};
use crate::goto_line::{GoToLineEvent, GoToLinePrompt};
use crate::image_preview::{ImageEvent, ImagePreview, is_image, is_svg};
use crate::lsp::{FileEdit, LanguageServers, Query, Target};
use crate::markdown::{MarkdownEvent, MarkdownPreview, is_markdown};
use crate::palette::{CommandPalette, PaletteEvent};
use crate::picker::{Badge, Picker, PickerEvent, PickerItem};
use crate::rename::{RenameEvent, RenamePrompt};
use crate::search_panel::{SearchPanel, SearchPanelEvent};
use crate::session;
use crate::symbols::{self, Location, SymbolIndex};
use crate::theme;

/// Height of the title/tab bar. On macOS the traffic lights sit in it.
pub const TITLE_BAR_HEIGHT: f32 = if cfg!(target_os = "macos") { 38. } else { 34. };
const STATUS_BAR_HEIGHT: f32 = 26.;
const BREADCRUMBS_HEIGHT: f32 = 26.;
const TAB_TEXT_SIZE: f32 = 13.5;
const MIN_SIDEBAR: f32 = 160.;
/// How long after the last edit the session is saved.
const SESSION_SAVE_DELAY: std::time::Duration = std::time::Duration::from_secs(3);
const MAX_SIDEBAR: f32 = 600.;
/// How long to wait for a language server before falling back to the
/// built-in symbol index.
const LSP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(4);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SidebarMode {
    Files,
    Search,
}

/// What a tab shows instead of its source.
#[derive(Clone)]
pub enum Preview {
    Markdown(Entity<MarkdownPreview>),
    Image(Entity<ImagePreview>),
}

impl Preview {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self {
            Preview::Markdown(p) => p.focus_handle(cx),
            Preview::Image(p) => p.focus_handle(cx),
        }
    }

    fn element(&self) -> AnyElement {
        match self {
            Preview::Markdown(p) => p.clone().into_any_element(),
            Preview::Image(p) => p.clone().into_any_element(),
        }
    }
}

pub struct Tab {
    pub id: u64,
    pub editor: Entity<EditorView>,
    pub preview: Option<Preview>,
    pub show_preview: bool,
    /// False for raster images, which have no text to show.
    pub has_source: bool,
    /// Unpinned tabs are "preview tabs": the next single-click open reuses
    /// them (Sublime and VS Code behaviour). Editing pins them.
    pub pinned: bool,
    _subscriptions: Vec<Subscription>,
}

impl Tab {
    fn content_focus(&self, cx: &App) -> FocusHandle {
        match (&self.preview, self.show_preview) {
            (Some(preview), true) => preview.focus_handle(cx),
            _ => self.editor.focus_handle(cx),
        }
    }

    fn showing_preview(&self) -> bool {
        self.show_preview && self.preview.is_some()
    }

    /// The picture of a raster image, which has no source and is painted on
    /// instead of edited.
    fn raster(&self) -> Option<&Entity<ImagePreview>> {
        match (&self.preview, self.has_source) {
            (Some(Preview::Image(image)), false) => Some(image),
            _ => None,
        }
    }

    fn is_dirty(&self, cx: &App) -> bool {
        match self.raster() {
            Some(image) => image.read(cx).is_dirty(),
            None => self.editor.read(cx).is_dirty(),
        }
    }

    fn save(&self, window: &mut Window, cx: &mut App) -> Task<anyhow::Result<()>> {
        match self.raster() {
            Some(image) => image.update(cx, |image, cx| image.save(window, cx)),
            None => self.editor.update(cx, |editor, cx| editor.save(window, cx)),
        }
    }
}

/// A tab being dragged to a new position.
#[derive(Clone)]
struct DraggedTab {
    id: u64,
    title: SharedString,
    icon: Option<crate::file_icons::FileIcon>,
}

impl Render for DraggedTab {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::palette(cx);
        div()
            .h(px(28.))
            .px(px(10.))
            .flex()
            .items_center()
            .gap(px(6.))
            .rounded(px(6.))
            .border_1()
            .border_color(palette.border)
            .bg(palette.tab_active_bg)
            .shadow_md()
            .font_family(theme::fonts(cx).ui.clone())
            .text_size(px(TAB_TEXT_SIZE))
            .text_color(palette.text)
            .when_some(self.icon.clone(), |el, icon| {
                el.child(
                    svg()
                        .path(icon.path)
                        .size(px(22.))
                        .mx(px(-3.))
                        .text_color(icon.color),
                )
            })
            .child(self.title.clone())
    }
}

#[derive(Clone, Debug, Default)]
pub struct OpenOptions {
    pub pin: bool,
    pub line: Option<u32>,
    pub column: Option<u32>,
    /// Leave focus where it is (a single click in the file tree).
    pub keep_focus: bool,
}

pub struct Workspace {
    focus_handle: FocusHandle,
    root: Option<PathBuf>,
    file_tree: Option<Entity<FileTree>>,
    finder: Option<Entity<FinderView>>,
    finder_open: bool,
    search_panel: Option<Entity<SearchPanel>>,
    sidebar_mode: SidebarMode,
    sidebar_visible: bool,
    sidebar_width: f32,
    resizing_sidebar: bool,
    find_bar: Entity<FindBar>,
    find_bar_open: bool,
    go_to_line: Entity<GoToLinePrompt>,
    go_to_line_open: bool,
    palette: Entity<CommandPalette>,
    palette_open: bool,
    /// Where focus returns when the palette closes (and where its command
    /// runs).
    palette_return: Option<FocusHandle>,
    tabs: Vec<Tab>,
    active: Option<usize>,
    closed: Vec<PathBuf>,
    recent: Vec<PathBuf>,
    should_move_window: bool,
    /// Where this window's session is kept (see [`session`]).
    pub session_key: String,
    /// The document revision of each tab's buffer last written to the
    /// session, so unchanged buffers aren't written again.
    written_revisions: HashMap<u64, u64>,
    save_task: Option<Task<()>>,
    /// The window's frame when last seen, for saving after it closed.
    last_bounds: Option<[f32; 4]>,
    /// Definitions in the folder's code, built the first time it's needed.
    symbol_index: Option<Arc<SymbolIndex>>,
    /// The folder's language servers.
    lsp: Option<Entity<LanguageServers>>,
    /// A go to definition waiting for the index to finish.
    pending_definition: Option<Task<()>>,
    picker: Entity<Picker>,
    picker_open: bool,
    picker_return: Option<FocusHandle>,
    /// Where each picker item goes.
    picker_targets: Vec<Location>,
    /// The picker lists problems: show the one picked.
    picker_problems: bool,
    rename: Entity<RenamePrompt>,
    rename_target: Option<RenameTarget>,
    /// Go back / forward through jumps (go to definition, symbol pickers).
    nav_back: Vec<Location>,
    nav_forward: Vec<Location>,
    /// A short message in the status bar ("No definition for foo").
    flash: Option<SharedString>,
    flash_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

/// A rename being typed.
struct RenameTarget {
    editor: Entity<EditorView>,
    /// The char the rename was asked at.
    pos: usize,
    old: String,
    /// Where the box goes (under the name), if the name is on screen.
    anchor: Option<Point<Pixels>>,
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Workspace {
    pub fn new(root: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let find_bar = cx.new(|cx| FindBar::new(window, cx));
        let go_to_line = cx.new(|cx| GoToLinePrompt::new(window, cx));
        let palette = cx.new(|cx| CommandPalette::new(window, cx));
        let picker = cx.new(|cx| Picker::new(window, cx));
        let rename = cx.new(|cx| RenamePrompt::new(window, cx));
        let mut subscriptions = vec![
            cx.subscribe_in(&palette, window, Self::on_palette_event),
            cx.subscribe_in(&picker, window, Self::on_picker_event),
            cx.subscribe_in(&rename, window, Self::on_rename_event),
            cx.subscribe_in(&find_bar, window, Self::on_find_bar_event),
            cx.subscribe_in(
                &go_to_line,
                window,
                |this, _, event: &GoToLineEvent, window, cx| {
                    this.go_to_line_open = false;
                    if let GoToLineEvent::Go { line, column } = event
                        && let Some(editor) = this.active_editor()
                    {
                        editor.update(cx, |e, cx| e.go_to(*line, *column, cx));
                    }
                    this.focus_active(window, cx);
                    cx.notify();
                },
            ),
        ];
        if let Some(updater) = crate::updater::entity(cx) {
            subscriptions.push(cx.observe(&updater, |_, _, cx| cx.notify()));
        }
        subscriptions.push(cx.observe_window_appearance(window, |_, window, cx| {
            theme::apply(window.appearance(), cx);
        }));
        subscriptions.push(cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.on_window_activated(cx);
            } else {
                // Leaving the window (or the app): keep the session current
                // in case the app doesn't get to quit cleanly.
                this.save_session(true, false, Some(window), cx);
            }
        }));
        let mut this = Self {
            focus_handle: cx.focus_handle(),
            root: None,
            file_tree: None,
            finder: None,
            finder_open: false,
            search_panel: None,
            sidebar_mode: SidebarMode::Files,
            sidebar_visible: true,
            sidebar_width: 240.,
            resizing_sidebar: false,
            find_bar,
            find_bar_open: false,
            go_to_line,
            go_to_line_open: false,
            palette,
            palette_open: false,
            palette_return: None,
            tabs: Vec::new(),
            active: None,
            closed: Vec::new(),
            recent: Vec::new(),
            should_move_window: false,
            session_key: match &root {
                Some(root) => session::key_for_root(root),
                None => session::new_scratch_key(),
            },
            written_revisions: HashMap::new(),
            save_task: None,
            last_bounds: None,
            symbol_index: None,
            lsp: None,
            pending_definition: None,
            picker,
            picker_open: false,
            picker_return: None,
            picker_targets: Vec::new(),
            picker_problems: false,
            rename,
            rename_target: None,
            nav_back: Vec::new(),
            nav_forward: Vec::new(),
            flash: None,
            flash_task: None,
            _subscriptions: subscriptions,
        };
        if let Some(root) = root {
            this.set_root(root, window, cx);
        }
        this
    }

    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    pub fn focus_handle_for_window(&self) -> FocusHandle {
        self.focus_handle.clone()
    }

    pub fn set_root(&mut self, root: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if session::is_scratch(&self.session_key) {
            session::delete(&self.session_key);
        }
        self.session_key = session::key_for_root(&root);
        self.symbol_index = None;
        let lsp = cx.new(|cx| LanguageServers::new(root.clone(), cx));
        self._subscriptions
            .push(cx.observe(&lsp, |_, _, cx| cx.notify()));
        let editors: Vec<Entity<EditorView>> = self.tabs.iter().map(|t| t.editor.clone()).collect();
        for editor in editors {
            lsp.update(cx, |lsp, cx| lsp.track(&editor, cx));
        }
        self.lsp = Some(lsp);
        let tree = cx.new(|cx| FileTree::new(root.clone(), cx));
        self._subscriptions.push(cx.subscribe_in(
            &tree,
            window,
            |this, _, event: &FileTreeEvent, window, cx| match event {
                FileTreeEvent::Open { path, pin } => {
                    this.open_path(
                        path.clone(),
                        OpenOptions {
                            pin: *pin,
                            keep_focus: !*pin,
                            ..Default::default()
                        },
                        window,
                        cx,
                    );
                }
                FileTreeEvent::Renamed { from, to } => this.paths_moved(from, to, window, cx),
                FileTreeEvent::Trashed(path) => {
                    let ids: Vec<u64> = this
                        .tabs
                        .iter()
                        .filter(|t| {
                            !t.is_dirty(cx)
                                && t.editor
                                    .read(cx)
                                    .path()
                                    .is_some_and(|p| p.starts_with(path))
                        })
                        .map(|t| t.id)
                        .collect();
                    for id in ids {
                        if let Some(ix) = this.tabs.iter().position(|t| t.id == id) {
                            this.remove_tab(ix, window, cx);
                        }
                    }
                }
            },
        ));
        let finder = cx.new(|cx| FinderView::new(root.clone(), window, cx));
        self._subscriptions.push(cx.subscribe_in(
            &finder,
            window,
            |this, _, event: &FinderEvent, window, cx| match event {
                FinderEvent::Open { path, line } => {
                    this.finder_open = false;
                    let (line, column) = line.map_or((None, None), |(l, c)| (Some(l), c));
                    this.open_path(
                        path.clone(),
                        OpenOptions {
                            pin: true,
                            line,
                            column,
                            ..Default::default()
                        },
                        window,
                        cx,
                    );
                }
                FinderEvent::Dismiss => {
                    this.finder_open = false;
                    this.focus_active(window, cx);
                    cx.notify();
                }
            },
        ));
        let search = cx.new(|cx| SearchPanel::new(root.clone(), window, cx));
        self._subscriptions
            .push(cx.subscribe_in(&search, window, Self::on_search_event));
        self.root = Some(root);
        self.file_tree = Some(tree);
        self.finder = Some(finder);
        self.search_panel = Some(search);
        self.update_title(window, cx);
        cx.notify();
    }

    fn on_window_activated(&mut self, cx: &mut Context<Self>) {
        if let Some(tree) = &self.file_tree {
            tree.update(cx, |tree, cx| tree.refresh(cx));
        }
        if let Some(index) = &self.symbol_index {
            index.refresh(|| {});
        }
        for tab in &self.tabs {
            tab.editor
                .update(cx, |editor, cx| editor.reload_if_changed(cx));
            if let (Some(Preview::Image(image)), false) = (&tab.preview, tab.has_source) {
                image.update(cx, |image, cx| image.reload(cx));
            }
        }
    }

    pub fn tab_ids(&self) -> Vec<u64> {
        self.tabs.iter().map(|t| t.id).collect()
    }

    fn active_tab(&self) -> Option<&Tab> {
        self.active.and_then(|ix| self.tabs.get(ix))
    }

    pub fn active_editor(&self) -> Option<Entity<EditorView>> {
        self.active_tab().map(|t| t.editor.clone())
    }

    fn active_path(&self, cx: &App) -> Option<PathBuf> {
        self.active_editor()
            .and_then(|e| e.read(cx).path().map(Path::to_path_buf))
    }

    /// `path` relative to the folder, or absolute when outside it.
    fn relative_path(&self, path: &Path) -> String {
        self.root
            .as_ref()
            .and_then(|root| path.strip_prefix(root).ok())
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned()
    }

    /// Closes every unmodified tab `keep` says no to.
    fn close_tabs_where(
        &mut self,
        mut close: impl FnMut(usize) -> bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ids: Vec<u64> = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(ix, t)| close(*ix) && !t.is_dirty(cx))
            .map(|(_, t)| t.id)
            .collect();
        for id in ids {
            if let Some(ix) = self.tabs.iter().position(|t| t.id == id) {
                self.remove_tab(ix, window, cx);
            }
        }
    }

    /// Tabs follow a file or folder renamed from `from` to `to`.
    fn paths_moved(&mut self, from: &Path, to: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let moved = |path: &Path| -> Option<PathBuf> {
            path.strip_prefix(from).ok().map(|rest| {
                if rest.as_os_str().is_empty() {
                    to.to_path_buf()
                } else {
                    to.join(rest)
                }
            })
        };
        for tab in &self.tabs {
            let new_path = tab.editor.read(cx).path().and_then(moved);
            if let Some(new_path) = new_path {
                if let Some(Preview::Image(image)) = &tab.preview {
                    image.update(cx, |image, _| image.set_path(new_path.clone()));
                }
                tab.editor.update(cx, |e, cx| e.set_path(new_path, cx));
            }
        }
        for path in self.recent.iter_mut().chain(self.closed.iter_mut()) {
            if let Some(new_path) = moved(path) {
                *path = new_path;
            }
        }
        self.update_title(window, cx);
        cx.notify();
    }

    /// Moves the dragged tab to `to_id`'s place, or to the end.
    fn move_tab(&mut self, from_id: u64, to_id: Option<u64>, cx: &mut Context<Self>) {
        let Some(from) = self.tabs.iter().position(|t| t.id == from_id) else {
            return;
        };
        let to = match to_id {
            Some(id) => match self.tabs.iter().position(|t| t.id == id) {
                Some(ix) => ix,
                None => return,
            },
            None => self.tabs.len() - 1,
        };
        if from == to {
            return;
        }
        let active_id = self.active.map(|a| self.tabs[a].id);
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        self.active = active_id.and_then(|id| self.tabs.iter().position(|t| t.id == id));
        cx.notify();
    }

    fn show_tab_menu(
        &mut self,
        id: u64,
        position: gpui::Point<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self.tabs.iter().position(|t| t.id == id) else {
            return;
        };
        // Menu commands act on the active tab.
        self.activate(ix, window, cx);
        let has_path = self.tabs[ix].editor.read(cx).path().is_some();
        let mut menu = gpui_component::native_menu::NativeMenu::new()
            .menu("Close Tab", Box::new(CloseTab))
            .menu("Close Other Tabs", Box::new(CloseOtherTabs))
            .menu("Close Tabs to the Right", Box::new(CloseTabsToTheRight));
        if has_path {
            menu = menu
                .separator()
                .menu(crate::platform::REVEAL_LABEL, Box::new(RevealInFinder))
                .menu("Reveal in Sidebar", Box::new(RevealInSidebar))
                .separator()
                .menu("Copy Path", Box::new(CopyPath))
                .menu("Copy Relative Path", Box::new(CopyRelativePath));
        }
        menu.show(position, window, cx);
    }

    fn focus_active(&self, window: &mut Window, cx: &mut App) {
        match self.active_tab() {
            Some(tab) => window.focus(&tab.content_focus(cx), cx),
            None => window.focus(&self.focus_handle, cx),
        }
    }

    fn update_title(&self, window: &mut Window, cx: &App) {
        let folder = self
            .root
            .as_ref()
            .and_then(|r| r.file_name())
            .map(|n| n.to_string_lossy().into_owned());
        let file = self
            .active_tab()
            .map(|t| t.editor.read(cx).title().to_string());
        let title = match (file, folder) {
            (Some(file), Some(folder)) => format!("{file} — {folder}"),
            (Some(file), None) => file,
            (None, Some(folder)) => folder,
            (None, None) => "Me".into(),
        };
        window.set_window_title(&title);
    }

    // ---------------------------------------------------------------------
    // Go to definition and symbols
    // ---------------------------------------------------------------------

    /// The folder's symbol index, started (in the background) if needed.
    fn ensure_index(&mut self) -> Option<Arc<SymbolIndex>> {
        let root = self.root.clone()?;
        let index = self
            .symbol_index
            .get_or_insert_with(|| SymbolIndex::new(root))
            .clone();
        if !index.is_ready() {
            index.refresh(|| {});
        }
        Some(index)
    }

    /// Jumps to the definition of the identifier at `pos` in `editor`: from
    /// the file's own symbols and the folder's index.
    fn go_to_definition(
        &mut self,
        editor: Entity<EditorView>,
        pos: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((word_range, word)) = editor.read(cx).identifier_at(pos) else {
            return;
        };
        let Some((request, name)) = self.lsp_locations(&editor, pos, Query::Definition, cx) else {
            self.find_definition(editor, pos, true, window, cx);
            return;
        };
        self.pending_definition = Some(cx.spawn_in(window, async move |this, cx| {
            let targets = wait_for_server(&this, request, name, cx)
                .await
                .and_then(Result::ok);
            this.update_in(cx, |this, window, cx| {
                this.pending_definition = None;
                let targets = match targets {
                    Some(targets) if !targets.is_empty() => targets,
                    // No server answer: the built-in index.
                    _ => return this.find_definition(editor, pos, true, window, cx),
                };
                // Already on the definition: show where it's used instead.
                let (line, col) = editor.read(cx).state.doc.line_col(word_range.start);
                let path = editor.read(cx).path().map(Path::to_path_buf);
                if let [only] = targets.as_slice()
                    && Some(&only.path) == path.as_ref()
                    && only.line == line
                    && only.col == col
                {
                    return this.find_references_at(editor, pos, window, cx);
                }
                this.show_targets(
                    &format!("Definitions of {word}"),
                    &word,
                    targets,
                    false,
                    window,
                    cx,
                );
            })
            .ok();
        }));
    }

    /// A language server's answer about the identifier at `pos`, if a
    /// server for the file is running, with the server's name.
    fn lsp_locations(
        &self,
        editor: &Entity<EditorView>,
        pos: usize,
        query: Query,
        cx: &mut Context<Self>,
    ) -> Option<(Task<anyhow::Result<Vec<Target>>>, SharedString)> {
        let lsp = self.lsp.clone()?;
        let name = lsp.read(cx).info(editor)?.name;
        let request = lsp.update(cx, |lsp, cx| lsp.locations(editor, pos, query, cx))?;
        Some((request, name))
    }

    /// Jumps to the one target, or offers a picker.
    fn show_targets(
        &mut self,
        placeholder: &str,
        word: &str,
        targets: Vec<Target>,
        always_pick: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let locations: Vec<Location> = targets
            .iter()
            .map(|t| Location {
                path: t.path.clone(),
                name: Arc::from(word),
                kind: SymbolKind::Other,
                line: t.line,
                col: t.col,
            })
            .collect();
        if locations.len() == 1 && !always_pick {
            let target = locations.into_iter().next().unwrap();
            self.jump_to(target, window, cx);
            return;
        }
        let is_dark = theme::palette(cx).is_dark;
        let items = targets
            .iter()
            .map(|t| {
                let file = t
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                PickerItem {
                    label: if t.line_text.is_empty() {
                        word.to_string().into()
                    } else {
                        t.line_text.clone().into()
                    },
                    detail: format!("{}:{}", self.relative_path(&t.path), t.line + 1).into(),
                    icon: Some(crate::file_icons::for_name(&file, is_dark)),
                    badge: None,
                    depth: 0,
                }
            })
            .collect();
        self.show_picker(placeholder, items, locations, 0, window, cx);
    }

    fn find_definition(
        &mut self,
        editor: Entity<EditorView>,
        pos: usize,
        wait_for_index: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((_, word)) = editor.read(cx).identifier_at(pos) else {
            return;
        };
        let path = editor.read(cx).path().map(Path::to_path_buf);
        let index = self.ensure_index();
        let mut candidates: Vec<Location> = Vec::new();
        // This file first, from its live (maybe unsaved) text.
        let own = editor.update(cx, |e, _| e.symbols());
        let at_byte = editor.read(cx).state.doc.rope().char_to_byte(pos);
        let mut on_definition = false;
        for s in own
            .iter()
            .filter(|s| s.name == word && s.kind != SymbolKind::Impl)
        {
            if s.name_range.contains(&at_byte) || s.name_range.end == at_byte {
                on_definition = true;
                continue;
            }
            if let Some(path) = &path {
                candidates.push(Location {
                    path: path.clone(),
                    name: Arc::from(s.name.as_str()),
                    kind: s.kind,
                    line: s.line,
                    col: s.col,
                });
            }
        }
        if let Some(index) = &index {
            candidates.extend(
                index
                    .lookup(&word)
                    .into_iter()
                    .filter(|l| Some(&l.path) != path.as_ref() && l.kind != SymbolKind::Impl),
            );
        }
        symbols::rank(&mut candidates, path.as_deref());
        match candidates.len() {
            0 if wait_for_index && index.as_ref().is_some_and(|i| !i.is_ready()) => {
                let index = index.unwrap();
                self.flash_message(format!("Indexing symbols… looking for {word}"), cx);
                self.pending_definition = Some(cx.spawn_in(window, async move |this, cx| {
                    while index.is_indexing() {
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(50))
                            .await;
                    }
                    this.update_in(cx, |this, window, cx| {
                        this.pending_definition = None;
                        this.flash = None;
                        this.find_definition(editor, pos, false, window, cx);
                    })
                    .ok();
                }));
            }
            0 if on_definition => {}
            0 => self.flash_message(format!("No definition found for {word}"), cx),
            1 => {
                let target = candidates.remove(0);
                self.jump_to(target, window, cx);
            }
            _ => {
                let title = format!("Definitions of {word}");
                self.show_locations(&title, candidates, window, cx);
            }
        }
    }

    /// The location the cursor is at, for going back to it.
    fn current_location(&self, cx: &App) -> Option<Location> {
        let editor = self.active_editor()?;
        let e = editor.read(cx);
        let path = e.path()?.to_path_buf();
        let (line, col) = e.state.doc.line_col(e.state.selections().primary().head);
        Some(Location {
            path,
            name: Arc::from(""),
            kind: SymbolKind::Other,
            line,
            col,
        })
    }

    /// Opens `target` at its line and column, remembering where we were.
    fn jump_to(&mut self, target: Location, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(here) = self.current_location(cx) {
            self.nav_back.push(here);
            self.nav_back.truncate(100);
        }
        self.nav_forward.clear();
        self.open_location(&target, window, cx);
    }

    fn open_location(&mut self, target: &Location, window: &mut Window, cx: &mut Context<Self>) {
        self.open_path(
            target.path.clone(),
            OpenOptions {
                pin: true,
                line: Some(target.line as u32 + 1),
                column: Some(target.col as u32 + 1),
                ..Default::default()
            },
            window,
            cx,
        );
    }

    fn navigate(&mut self, back: bool, window: &mut Window, cx: &mut Context<Self>) {
        let stack = if back {
            &mut self.nav_back
        } else {
            &mut self.nav_forward
        };
        let Some(target) = stack.pop() else { return };
        if let Some(here) = self.current_location(cx) {
            if back {
                self.nav_forward.push(here);
            } else {
                self.nav_back.push(here);
            }
        }
        self.open_location(&target, window, cx);
    }

    fn show_picker(
        &mut self,
        placeholder: &str,
        items: Vec<PickerItem>,
        targets: Vec<Location>,
        initial: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.finder_open = false;
        self.palette_open = false;
        if !self.picker_open {
            self.picker_return = window.focused(cx);
        }
        self.picker_targets = targets;
        self.picker_problems = false;
        self.picker_open = true;
        self.picker
            .update(cx, |p, cx| p.show(placeholder, items, initial, window, cx));
        cx.notify();
    }

    fn on_picker_event(
        &mut self,
        _: &Entity<Picker>,
        event: &PickerEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.picker_open = false;
        match self.picker_return.take() {
            Some(handle) => window.focus(&handle, cx),
            None => self.focus_active(window, cx),
        }
        if let PickerEvent::Picked(ix) = event
            && let Some(target) = self.picker_targets.get(*ix).cloned()
        {
            self.jump_to(target, window, cx);
            if self.picker_problems {
                self.show_problem_at_cursor(cx);
            }
        }
        cx.notify();
    }

    /// A picker over places in the project (definitions, project symbols).
    fn show_locations(
        &mut self,
        placeholder: &str,
        locations: Vec<Location>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let items = locations
            .iter()
            .map(|l| PickerItem {
                label: SharedString::from(l.name.to_string()),
                detail: format!("{}:{}", self.relative_path(&l.path), l.line + 1).into(),
                icon: None,
                badge: Some(Badge::symbol(l.kind, theme::palette(cx))),
                depth: 0,
            })
            .collect();
        self.show_picker(placeholder, items, locations, 0, window, cx);
    }

    /// Cmd+R: the symbols in this file, nested, starting at the cursor's.
    fn show_file_symbols(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.active_editor() else {
            return;
        };
        let Some(path) = editor.read(cx).path().map(Path::to_path_buf) else {
            return;
        };
        let symbols = editor.update(cx, |e, _| e.symbols());
        if symbols.is_empty() {
            self.flash_message("No symbols in this file".to_string(), cx);
            return;
        }
        let at = {
            let e = editor.read(cx);
            e.state
                .doc
                .rope()
                .char_to_byte(e.state.selections().primary().head)
        };
        let mut items = Vec::new();
        let mut targets = Vec::new();
        let mut initial = 0;
        for (ix, s) in symbols.iter().enumerate() {
            let depth = symbols[..ix]
                .iter()
                .filter(|outer| {
                    outer.range.start <= s.range.start && s.range.end <= outer.range.end
                })
                .count();
            if s.range.start <= at && at < s.range.end {
                initial = ix;
            }
            items.push(PickerItem {
                label: symbol_label(s).into(),
                detail: format!("{} · line {}", s.kind.label(), s.line + 1).into(),
                icon: None,
                badge: Some(Badge::symbol(s.kind, theme::palette(cx))),
                depth,
            });
            targets.push(Location {
                path: path.clone(),
                name: Arc::from(s.name.as_str()),
                kind: s.kind,
                line: s.line,
                col: s.col,
            });
        }
        self.show_picker("Go to symbol in file…", items, targets, initial, window, cx);
    }

    /// Cmd+Shift+R: every definition in the folder.
    fn show_project_symbols(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.ensure_index() else {
            self.show_file_symbols(window, cx);
            return;
        };
        if !index.is_ready() {
            self.flash_message("Indexing symbols… try again in a moment".to_string(), cx);
            return;
        }
        let locations = index.all(50_000);
        self.show_locations("Go to symbol in project…", locations, window, cx);
    }

    /// Shift+F12: where the identifier at the cursor is used.
    fn find_references(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.active_editor() {
            Some(editor) => {
                let head = editor.read(cx).state.selections().primary().head;
                self.find_references_at(editor, head, window, cx);
            }
            None => self.show_search(window, cx),
        }
    }

    /// References from the language server, or else the identifier as a
    /// whole word in the project search.
    fn find_references_at(
        &mut self,
        editor: Entity<EditorView>,
        pos: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((_, word)) = editor.read(cx).identifier_at(pos) else {
            self.show_search(window, cx);
            return;
        };
        let Some((request, name)) = self.lsp_locations(&editor, pos, Query::References, cx) else {
            self.search_word(word, window, cx);
            return;
        };
        self.pending_definition = Some(cx.spawn_in(window, async move |this, cx| {
            let targets = wait_for_server(&this, request, name, cx)
                .await
                .and_then(Result::ok);
            this.update_in(cx, |this, window, cx| {
                this.pending_definition = None;
                match targets {
                    Some(targets) if !targets.is_empty() => {
                        let title = format!("References to {word}");
                        this.show_targets(&title, &word, targets, true, window, cx);
                    }
                    _ => this.search_word(word, window, cx),
                }
            })
            .ok();
        }));
    }

    /// The project search for `word`, whole word and case sensitive.
    fn search_word(&mut self, word: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(search) = self.search_panel.clone() else {
            return;
        };
        self.sidebar_visible = true;
        self.sidebar_mode = SidebarMode::Search;
        search.update(cx, |panel, cx| panel.show_word(word, window, cx));
        cx.notify();
    }

    /// F12: go to the definition of the identifier at the cursor.
    fn go_to_definition_at_cursor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(editor) = self.active_editor() {
            let head = editor.read(cx).state.selections().primary().head;
            self.go_to_definition(editor, head, window, cx);
        }
    }

    /// Asks the language server what to show in `editor`'s hover.
    fn request_hover(
        &mut self,
        editor: Entity<EditorView>,
        pos: usize,
        id: u64,
        cx: &mut Context<Self>,
    ) {
        let Some(lsp) = self.lsp.clone() else { return };
        let Some(request) = lsp.update(cx, |lsp, cx| lsp.hover(&editor, pos, cx)) else {
            return;
        };
        cx.spawn(async move |_, cx| {
            let info = request.await.ok().flatten();
            editor.update(cx, |editor, cx| editor.set_hover_info(id, info, cx));
        })
        .detach();
    }

    // ---------------------------------------------------------------------
    // Rename
    // ---------------------------------------------------------------------

    /// F2: rename the symbol at the cursor everywhere (the language server
    /// works out where), or, without a server, every occurrence of the word
    /// in this file with a cursor each.
    fn rename_symbol(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.active_editor() else {
            return;
        };
        let pos = editor.read(cx).state.selections().primary().head;
        let Some((_, word)) = editor.read(cx).identifier_at(pos) else {
            self.flash_message("Nothing to rename here".to_string(), cx);
            return;
        };
        let prepared = self.lsp.clone().and_then(|lsp| {
            let name = lsp.read(cx).info(&editor)?.name;
            let task = lsp.update(cx, |lsp, cx| lsp.prepare_rename(&editor, pos, cx))?;
            Some((task, name))
        });
        let Some((prepared, name)) = prepared else {
            editor.update(cx, |editor, cx| editor.select_all_occurrences(cx));
            self.flash_message(
                format!("No language server: editing every {word} in this file"),
                cx,
            );
            return;
        };
        self.pending_definition = Some(cx.spawn_in(window, async move |this, cx| {
            let prepared = wait_for_server(&this, prepared, name.clone(), cx).await;
            this.update_in(cx, |this, window, cx| {
                this.pending_definition = None;
                match prepared {
                    Some(Ok(Some((range, old)))) => {
                        this.open_rename(editor, range, old, window, cx)
                    }
                    // The server doesn't rename this kind of thing (Ruby LSP
                    // only renames constants): edit it in this file.
                    Some(Ok(None)) => {
                        editor.update(cx, |editor, cx| editor.select_all_occurrences(cx));
                        this.flash_message(
                            format!(
                                "{name} can't rename {word}: editing every {word} in this file"
                            ),
                            cx,
                        );
                    }
                    Some(Err(err)) => this.flash_message(format!("Can't rename {word}: {err}"), cx),
                    None => this.flash_message(format!("Can't rename {word}"), cx),
                }
            })
            .ok();
        }));
    }

    /// The new-name box, under the name.
    fn open_rename(
        &mut self,
        editor: Entity<EditorView>,
        range: std::ops::Range<usize>,
        old: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let anchor = {
            let e = editor.read(cx);
            e.layout.as_ref().and_then(|layout| {
                let p = layout.point_for_pos(range.start, &e.state)?;
                Some(gpui::point(p.x - px(8.), p.y + layout.line_height + px(2.)))
            })
        };
        self.rename_target = Some(RenameTarget {
            editor,
            pos: range.start,
            old: old.clone(),
            anchor,
        });
        self.rename
            .update(cx, |rename, cx| rename.show(&old, window, cx));
        cx.notify();
    }

    fn on_rename_event(
        &mut self,
        _: &Entity<RenamePrompt>,
        event: &RenameEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(RenameTarget {
            editor, pos, old, ..
        }) = self.rename_target.take()
        else {
            return;
        };
        cx.notify();
        if !matches!(event, RenameEvent::Blur) {
            window.focus(&editor.focus_handle(cx), cx);
        }
        let RenameEvent::Rename(new) = event else {
            return;
        };
        if new.is_empty() || *new == old {
            return;
        }
        let new = new.clone();
        let Some(lsp) = self.lsp.clone() else { return };
        let Some(request) = lsp.update(cx, |lsp, cx| lsp.rename(&editor, pos, new.clone(), cx))
        else {
            self.flash_message(format!("Can't rename {old}"), cx);
            return;
        };
        self.flash_message(format!("Renaming {old} to {new}…"), cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = request.await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(rename) if rename.files.is_empty() => {
                    this.flash_message(format!("Nothing to rename for {old}"), cx)
                }
                Ok(rename) => {
                    let count = this.apply_file_edits(rename.files, rename.encoding, window, cx);
                    let files = if count == 1 { "file" } else { "files" };
                    this.flash_message(format!("Renamed {old} to {new} in {count} {files}"), cx);
                }
                Err(err) => this.flash_message(format!("Rename failed: {err}"), cx),
            })
            .ok();
        })
        .detach();
    }

    /// Applies a rename: open files get one undoable edit (and are saved if
    /// they had no other changes, like VS Code), closed ones are edited on
    /// disk. Returns how many files changed.
    fn apply_file_edits(
        &mut self,
        files: Vec<FileEdit>,
        encoding: me_lsp::Encoding,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> usize {
        let mut changed = 0;
        let mut written = Vec::new();
        for file in files {
            let open = self
                .tabs
                .iter()
                .find(|t| t.editor.read(cx).path() == Some(file.path.as_path()))
                .map(|t| t.editor.clone());
            match open {
                Some(editor) => {
                    let was_clean = !editor.read(cx).is_dirty();
                    editor.update(cx, |editor, cx| {
                        let edits =
                            crate::lsp::char_edits(editor.state.doc.rope(), &file.edits, encoding);
                        editor.apply_edits(edits, cx);
                        if was_clean {
                            editor.save(window, cx).detach();
                        }
                    });
                    changed += 1;
                }
                None => match edit_file(&file, encoding) {
                    Ok(()) => {
                        changed += 1;
                        written.push(file.path);
                    }
                    Err(err) => self.flash_message(
                        format!("Could not change {}: {err}", self.relative_path(&file.path)),
                        cx,
                    ),
                },
            }
        }
        if let Some(lsp) = &self.lsp {
            lsp.read(cx).files_changed(&written);
        }
        if let Some(index) = self.symbol_index.clone() {
            cx.background_executor()
                .spawn(async move {
                    for path in written.iter().filter(|p| symbols::indexable(p)) {
                        if let Ok(text) = std::fs::read_to_string(path) {
                            index.update(path, &text);
                        }
                    }
                })
                .detach();
        }
        changed
    }

    // ---------------------------------------------------------------------
    // Problems
    // ---------------------------------------------------------------------

    /// Cmd+Shift+M: every problem in the folder.
    fn show_problems(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let problems = self
            .lsp
            .as_ref()
            .map(|lsp| lsp.read(cx).problems(cx))
            .unwrap_or_default();
        if problems.is_empty() {
            self.flash_message("No problems".to_string(), cx);
            return;
        }
        let palette = theme::palette(cx).clone();
        // Errors first, then by file.
        let mut problems = problems;
        problems.sort_by_key(|p| p.severity);
        let items = problems
            .iter()
            .map(|p| {
                let message: String = p
                    .message
                    .lines()
                    .next()
                    .unwrap_or("")
                    .chars()
                    .take(160)
                    .collect();
                PickerItem {
                    label: message.into(),
                    detail: format!("{}:{}", self.relative_path(&p.path), p.line + 1).into(),
                    icon: None,
                    badge: Some(crate::editor::hover::severity_badge(p.severity, &palette)),
                    depth: 0,
                }
            })
            .collect();
        let targets = problems
            .iter()
            .map(|p| Location {
                path: p.path.clone(),
                name: Arc::from(""),
                kind: SymbolKind::Other,
                line: p.line,
                col: p.col,
            })
            .collect();
        self.show_picker("Problems", items, targets, 0, window, cx);
        self.picker_problems = true;
    }

    /// F8 / Shift+F8: the next (previous) problem in the folder, after
    /// (before) the cursor, wrapping around.
    fn next_problem(&mut self, back: bool, window: &mut Window, cx: &mut Context<Self>) {
        let problems: Vec<_> = self
            .lsp
            .as_ref()
            .map(|lsp| lsp.read(cx).problems(cx))
            .unwrap_or_default()
            .into_iter()
            .filter(|p| p.severity <= Severity::Warning)
            .collect();
        if problems.is_empty() {
            self.flash_message("No problems".to_string(), cx);
            return;
        }
        let ix = match self.current_location(cx) {
            Some(here) => {
                let here = (&here.path, here.line, here.col);
                if back {
                    problems
                        .iter()
                        .rposition(|p| (&p.path, p.line, p.col) < here)
                        .unwrap_or(problems.len() - 1)
                } else {
                    problems
                        .iter()
                        .position(|p| (&p.path, p.line, p.col) > here)
                        .unwrap_or(0)
                }
            }
            None => 0,
        };
        let problem = &problems[ix];
        let target = Location {
            path: problem.path.clone(),
            name: Arc::from(""),
            kind: SymbolKind::Other,
            line: problem.line,
            col: problem.col,
        };
        self.jump_to(target, window, cx);
        self.show_problem_at_cursor(cx);
    }

    /// The hover for the problem the cursor was just put on.
    fn show_problem_at_cursor(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = self.active_editor() {
            editor.update(cx, |editor, cx| {
                let head = editor.state.selections().primary().head;
                editor.show_hover_at(head, true, cx);
            });
        }
    }

    fn flash_message(&mut self, message: String, cx: &mut Context<Self>) {
        self.flash = Some(message.into());
        self.flash_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(4))
                .await;
            this.update(cx, |this, cx| {
                this.flash = None;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    // ---------------------------------------------------------------------
    // Sessions
    // ---------------------------------------------------------------------

    /// This window's session; buffers whose text changed since they were
    /// last written are included.
    fn snapshot(&mut self, open: bool, window: Option<&Window>, cx: &App) -> session::Snapshot {
        let mut tabs = Vec::new();
        let mut buffers = Vec::new();
        let mut images = Vec::new();
        let mut active = None;
        for (ix, tab) in self.tabs.iter().enumerate() {
            // `me -w` tabs belong to the process waiting on them.
            if app_state::is_waited(tab.id, cx) {
                continue;
            }
            let editor = tab.editor.read(cx);
            let unsaved = match tab.raster() {
                Some(image) => image.read(cx).unsaved_pixels().map(|(revision, pixels)| {
                    let name = format!("buffer-{}.png", tab.id);
                    if self.written_revisions.insert(tab.id, revision) != Some(revision) {
                        images.push((name.clone(), pixels));
                    }
                    name
                }),
                None => editor.unsaved_text().map(|rope| {
                    let name = format!("buffer-{}.txt", tab.id);
                    let revision = editor.revision();
                    if self.written_revisions.insert(tab.id, revision) != Some(revision) {
                        buffers.push((name.clone(), rope));
                    }
                    name
                }),
            };
            if editor.path().is_none() && unsaved.is_none() {
                continue;
            }
            if Some(ix) == self.active {
                active = Some(tabs.len());
            }
            let (cursor, scroll) = editor.view_state();
            tabs.push(session::TabSession {
                path: editor.path().map(Path::to_path_buf),
                unsaved,
                pinned: tab.pinned,
                show_preview: tab.show_preview,
                cursor,
                scroll,
            });
        }
        if let Some(window) = window
            && let gpui::WindowBounds::Windowed(b) = window.window_bounds()
        {
            self.last_bounds = Some([
                f32::from(b.origin.x),
                f32::from(b.origin.y),
                f32::from(b.size.width),
                f32::from(b.size.height),
            ]);
        }
        session::Snapshot {
            session: session::WindowSession {
                root: self.root.clone(),
                open,
                tabs,
                active,
                sidebar_visible: self.sidebar_visible,
                sidebar_width: self.sidebar_width,
                bounds: self.last_bounds,
            },
            buffers,
            images,
        }
    }

    /// Writes this window's session. `open` says whether the window should
    /// come back on the next launch. A window without a folder and nothing
    /// worth keeping has its session removed instead.
    pub fn save_session(
        &mut self,
        open: bool,
        sync: bool,
        window: Option<&Window>,
        cx: &mut Context<Self>,
    ) {
        let snapshot = self.snapshot(open, window, cx);
        let key = self.session_key.clone();
        if self.root.is_none() && snapshot.session.tabs.is_empty() {
            self.save_task = None;
            session::delete(&key);
            return;
        }
        let write = move || {
            if let Err(err) = session::write(&key, &snapshot) {
                eprintln!("me: could not save the session: {err}");
            }
        };
        if sync {
            self.save_task = None;
            write();
        } else {
            self.save_task = Some(cx.background_executor().spawn(async move { write() }));
        }
    }

    /// Saves the session a few seconds after the last edit.
    fn schedule_session_save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_task = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(SESSION_SAVE_DELAY).await;
            this.update_in(cx, |this, window, cx| {
                this.save_session(true, false, Some(window), cx)
            })
            .ok();
        }));
    }

    /// Reopens a saved session's tabs, with their unsaved text, cursors and
    /// scroll positions.
    pub fn restore_session(
        &mut self,
        saved: session::WindowSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sidebar_visible = saved.sidebar_visible;
        self.sidebar_width = saved.sidebar_width.clamp(MIN_SIDEBAR, MAX_SIDEBAR);
        let mut active_id = None;
        for (ix, tab) in saved.tabs.into_iter().enumerate() {
            let buffer = tab
                .unsaved
                .as_deref()
                .and_then(|name| session::read_buffer(&self.session_key, name));
            // A raster image's unsaved pixels are a PNG.
            let raster = tab
                .path
                .as_deref()
                .is_some_and(|p| is_image(p) && !is_svg(p));
            let (text, pixels) = match buffer {
                Some(png) if raster => (None, Some(png)),
                buffer => (buffer.and_then(|b| String::from_utf8(b).ok()), None),
            };
            let id = match &tab.path {
                // A file deleted since, with nothing unsaved: let it go.
                Some(path) if !path.exists() && text.is_none() && pixels.is_none() => continue,
                Some(path) => self.open_path(
                    path.clone(),
                    OpenOptions {
                        pin: true,
                        keep_focus: true,
                        ..Default::default()
                    },
                    window,
                    cx,
                ),
                None if text.is_some() => self.open_text("", window, cx),
                None => continue,
            };
            let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) else {
                continue;
            };
            t.pinned = tab.pinned;
            // Raster images have nothing but the preview.
            t.show_preview = !t.has_source || (tab.show_preview && t.preview.is_some());
            if let (Some(png), Some(image)) = (pixels, t.raster()) {
                image.update(cx, |image, cx| image.restore_unsaved(png, cx));
            }
            t.editor.update(cx, |e, cx| {
                e.restore(
                    crate::editor::Restore {
                        text,
                        cursor: tab.cursor,
                        scroll: tab.scroll,
                    },
                    cx,
                )
            });
            if saved.active == Some(ix) {
                active_id = Some(id);
            }
        }
        let active = active_id
            .and_then(|id| self.tabs.iter().position(|t| t.id == id))
            .or(if self.tabs.is_empty() { None } else { Some(0) });
        if let Some(ix) = active {
            self.activate(ix, window, cx);
        }
        cx.notify();
    }

    // ---------------------------------------------------------------------
    // Opening and closing
    // ---------------------------------------------------------------------

    /// Opens (or focuses) a file and returns its tab id.
    pub fn open_path(
        &mut self,
        path: PathBuf,
        options: OpenOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> u64 {
        if let Some(ix) = self
            .tabs
            .iter()
            .position(|t| t.editor.read(cx).path() == Some(path.as_path()))
        {
            if options.pin {
                self.tabs[ix].pinned = true;
            }
            self.activate_tab(ix, !options.keep_focus, window, cx);
            if let Some(line) = options.line {
                self.tabs[ix]
                    .editor
                    .update(cx, |e, cx| e.go_to(line, options.column, cx));
            }
            return self.tabs[ix].id;
        }

        let tab = if is_image(&path) && !is_svg(&path) {
            // A picture to paint on; the bytes never go into an editor.
            let editor = cx.new(|cx| EditorView::placeholder(path.clone(), window, cx));
            let image = cx.new(|cx| ImagePreview::raster(path.clone(), cx));
            let mut tab = self.make_tab(
                editor,
                options.pin,
                Some(Preview::Image(image.clone())),
                window,
                cx,
            );
            tab.has_source = false;
            let id = tab.id;
            tab._subscriptions.push(cx.subscribe_in(
                &image,
                window,
                move |this, image, event: &ImageEvent, window, cx| match event {
                    ImageEvent::Edited => {
                        // Painting pins a preview tab, like typing.
                        if image.read(cx).is_dirty()
                            && let Some(tab) = this.tabs.iter_mut().find(|t| t.id == id)
                        {
                            tab.pinned = true;
                        }
                        this.schedule_session_save(window, cx);
                        cx.notify();
                    }
                    ImageEvent::Saved => {
                        if let Some(tree) = &this.file_tree {
                            tree.update(cx, |tree, cx| tree.refresh(cx));
                        }
                        this.schedule_session_save(window, cx);
                        cx.notify();
                    }
                    ImageEvent::Moved(path) => {
                        if let Some(tab) = this.tabs.iter().find(|t| t.id == id) {
                            tab.editor.update(cx, |e, cx| e.set_path(path.clone(), cx));
                        }
                    }
                    ImageEvent::Message(message) => this.flash_message(message.to_string(), cx),
                },
            ));
            tab
        } else {
            let editor = cx.new(|cx| EditorView::open(path.clone(), window, cx));
            if let Some(line) = options.line {
                editor.update(cx, |e, cx| e.go_to(line, options.column, cx));
            }
            let (preview, subscription) = if is_svg(&path) {
                let editor = editor.clone();
                let image = cx.new(|cx| ImagePreview::svg(path.clone(), editor, cx));
                (Some(Preview::Image(image)), None)
            } else if is_markdown(&path) && options.line.is_none() {
                let (preview, subscription) = self.markdown_preview(&editor, window, cx);
                (Some(preview), Some(subscription))
            } else {
                (None, None)
            };
            let mut tab = self.make_tab(editor, options.pin, preview, window, cx);
            tab._subscriptions.extend(subscription);
            tab
        };
        let id = tab.id;

        // Reuse an unpinned, unmodified preview tab.
        let reuse = self
            .active
            .filter(|&ix| !options.pin && !self.tabs[ix].pinned && !self.tabs[ix].is_dirty(cx));
        let ix = match reuse {
            Some(ix) => {
                let old = std::mem::replace(&mut self.tabs[ix], tab);
                app_state::tab_closed(old.id, cx);
                ix
            }
            None => {
                let ix = self.active.map_or(self.tabs.len(), |a| a + 1);
                self.tabs.insert(ix, tab);
                ix
            }
        };
        if symbols::indexable(&path) {
            self.ensure_index();
        }
        self.recent.retain(|p| p != &path);
        self.recent.insert(0, path);
        self.recent.truncate(50);
        self.activate_tab(ix, !options.keep_focus, window, cx);
        id
    }

    /// Opens a new tab with `text` (stdin, or a new untitled file).
    pub fn open_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) -> u64 {
        let editor = cx.new(|cx| EditorView::with_text(text, window, cx));
        let tab = self.make_tab(editor, true, None, window, cx);
        let id = tab.id;
        let ix = self.active.map_or(self.tabs.len(), |a| a + 1);
        self.tabs.insert(ix, tab);
        self.activate(ix, window, cx);
        id
    }

    /// A rendered view of `editor`'s markdown; its local links open here.
    fn markdown_preview(
        &mut self,
        editor: &Entity<EditorView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Preview, Subscription) {
        let preview = cx.new(|cx| MarkdownPreview::new(editor.clone(), window, cx));
        let subscription = cx.subscribe_in(
            &preview,
            window,
            |this, _, event: &MarkdownEvent, window, cx| match event {
                MarkdownEvent::OpenPath(path) => {
                    this.open_path(
                        path.clone(),
                        OpenOptions {
                            pin: true,
                            ..Default::default()
                        },
                        window,
                        cx,
                    );
                }
            },
        );
        (Preview::Markdown(preview), subscription)
    }

    fn make_tab(
        &mut self,
        editor: Entity<EditorView>,
        pinned: bool,
        preview: Option<Preview>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Tab {
        let id = app_state::next_tab_id(cx);
        if let Some(lsp) = &self.lsp {
            lsp.update(cx, |lsp, cx| lsp.track(&editor, cx));
        }
        let subscriptions = vec![cx.subscribe_in(
            &editor,
            window,
            move |this, editor, event: &EditorEvent, window, cx| match event {
                EditorEvent::Changed | EditorEvent::DirtyChanged => {
                    if let Some(tab) = this.tabs.iter_mut().find(|t| t.id == id)
                        && !tab.pinned
                        && matches!(event, EditorEvent::DirtyChanged)
                    {
                        tab.pinned = true;
                    }
                    this.schedule_session_save(window, cx);
                    cx.notify();
                }
                EditorEvent::Saved => {
                    if let Some(tree) = &this.file_tree {
                        tree.update(cx, |tree, cx| tree.refresh(cx));
                    }
                    if let (Some(index), Some(path)) = (
                        this.symbol_index.clone(),
                        editor.read(cx).path().map(Path::to_path_buf),
                    ) && symbols::indexable(&path)
                    {
                        let text = editor.read(cx).text();
                        cx.background_executor()
                            .spawn(async move { index.update(&path, &text) })
                            .detach();
                    }
                    this.update_title(window, cx);
                    cx.notify();
                }
                EditorEvent::Loaded => cx.notify(),
                EditorEvent::GoToDefinition(pos) => {
                    this.go_to_definition(editor.clone(), *pos, window, cx)
                }
                EditorEvent::Hover { pos, id } => this.request_hover(editor.clone(), *pos, *id, cx),
                EditorEvent::OpenFile { path, line } => {
                    let target = Location {
                        path: path.clone(),
                        name: Arc::from(""),
                        kind: SymbolKind::Other,
                        line: line.map_or(0, |l| l.saturating_sub(1) as usize),
                        col: 0,
                    };
                    this.jump_to(target, window, cx);
                }
            },
        )];
        Tab {
            id,
            editor,
            show_preview: preview.is_some(),
            preview,
            has_source: true,
            pinned,
            _subscriptions: subscriptions,
        }
    }

    pub fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.activate_tab(ix, true, window, cx);
    }

    fn activate_tab(
        &mut self,
        ix: usize,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if ix >= self.tabs.len() {
            return;
        }
        self.active = Some(ix);
        let tab = &self.tabs[ix];
        if focus {
            window.focus(&tab.content_focus(cx), cx);
        }
        let path = tab.editor.read(cx).path().map(Path::to_path_buf);
        if let (Some(tree), Some(path)) = (&self.file_tree, path) {
            tree.update(cx, |tree, cx| tree.select_quietly(Some(&path), cx));
        }
        self.sync_find_highlights(cx);
        self.update_title(window, cx);
        cx.notify();
    }

    /// Closes a tab, asking first if it has unsaved changes.
    pub fn close_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(ix) else { return };
        if !tab.is_dirty(cx) {
            self.remove_tab(ix, window, cx);
            return;
        }
        let id = tab.id;
        let title = tab.editor.read(cx).title();
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Do you want to save the changes you made to {title}?"),
            Some("Your changes will be lost if you don't save them."),
            &["Save", "Don't Save", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            let Ok(answer) = answer.await else { return };
            if answer == 2 {
                return;
            }
            if answer == 0 {
                let save = this.update_in(cx, |this, window, cx| {
                    let tab = this.tabs.iter().find(|t| t.id == id)?;
                    Some(tab.save(window, cx))
                });
                let Ok(Some(save)) = save else { return };
                let saved = save.await.is_ok();
                let dirty = this.read_with(cx, |this, cx| {
                    this.tabs.iter().any(|t| t.id == id && t.is_dirty(cx))
                });
                if !saved || dirty.unwrap_or(true) {
                    return;
                }
            }
            this.update_in(cx, |this, window, cx| {
                if let Some(ix) = this.tabs.iter().position(|t| t.id == id) {
                    this.remove_tab(ix, window, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn remove_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.tabs.remove(ix);
        if let Some(path) = tab.editor.read(cx).path() {
            self.closed.push(path.to_path_buf());
        }
        app_state::tab_closed(tab.id, cx);
        self.active = if self.tabs.is_empty() {
            None
        } else {
            Some(match self.active {
                Some(a) if a > ix => a - 1,
                Some(a) if a == ix => ix.min(self.tabs.len() - 1),
                Some(a) => a,
                None => 0,
            })
        };
        match self.active {
            Some(a) => self.activate(a, window, cx),
            None => {
                window.focus(&self.focus_handle, cx);
                self.update_title(window, cx);
            }
        }
        cx.notify();
    }

    fn toggle_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.active else { return };
        if !self.tabs[ix].has_source {
            return;
        }
        if self.tabs[ix].preview.is_none() {
            // Any file can be read as markdown.
            let editor = self.tabs[ix].editor.clone();
            let (preview, subscription) = self.markdown_preview(&editor, window, cx);
            let tab = &mut self.tabs[ix];
            tab.preview = Some(preview);
            tab._subscriptions.push(subscription);
            tab.show_preview = true;
        } else {
            let tab = &mut self.tabs[ix];
            tab.show_preview = !tab.show_preview;
        }
        let focus = self.tabs[ix].content_focus(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    // ---------------------------------------------------------------------
    // Find
    // ---------------------------------------------------------------------

    fn open_find_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab().filter(|t| t.has_source) else {
            return;
        };
        if tab.showing_preview() {
            // Find works on the source.
            self.toggle_preview(window, cx);
        }
        let seed = self
            .active_editor()
            .and_then(|e| e.read(cx).selected_text());
        self.find_bar_open = true;
        self.find_bar
            .update(cx, |bar, cx| bar.show(seed, window, cx));
        self.sync_find_highlights(cx);
        cx.notify();
    }

    fn sync_find_highlights(&mut self, cx: &mut Context<Self>) {
        let query = if self.find_bar_open {
            self.find_bar.read(cx).query(cx)
        } else {
            None
        };
        for (ix, tab) in self.tabs.iter().enumerate() {
            let q = if Some(ix) == self.active {
                query.as_ref()
            } else {
                None
            };
            tab.editor.update(cx, |e, cx| e.set_search(q, cx));
        }
    }

    fn on_find_bar_event(
        &mut self,
        _: &Entity<FindBar>,
        event: &FindBarEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.active_editor() else {
            return;
        };
        match event {
            FindBarEvent::QueryChanged(query) => {
                editor.update(cx, |e, cx| {
                    e.set_search(query.as_ref(), cx);
                    if let Some(q) = query {
                        // Incremental: jump to the first match from the cursor.
                        let start = e.state.selections().primary().start();
                        e.state.set_cursor(start);
                        e.find(q, false, cx);
                    }
                });
            }
            FindBarEvent::Next(q) => {
                editor.update(cx, |e, cx| e.find(q, false, cx));
            }
            FindBarEvent::Previous(q) => {
                editor.update(cx, |e, cx| e.find(q, true, cx));
            }
            FindBarEvent::SelectAll(q) => {
                editor.update(cx, |e, cx| e.select_all_matches(q, cx));
                self.close_find_bar(window, cx);
            }
            FindBarEvent::Replace(q, with) => {
                editor.update(cx, |e, cx| e.replace_next(q, with, cx));
            }
            FindBarEvent::ReplaceAll(q, with) => {
                editor.update(cx, |e, cx| e.replace_all(q, with, cx));
            }
            FindBarEvent::Close => self.close_find_bar(window, cx),
        }
    }

    fn close_find_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.find_bar_open = false;
        self.sync_find_highlights(cx);
        self.focus_active(window, cx);
        cx.notify();
    }

    fn find_step(&mut self, backwards: bool, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.find_bar.read(cx).query(cx);
        match (query, self.active_editor()) {
            (Some(q), Some(editor)) => {
                editor.update(cx, |e, cx| e.find(&q, backwards, cx));
            }
            _ => self.open_find_bar(window, cx),
        }
    }

    fn on_search_event(
        &mut self,
        _: &Entity<SearchPanel>,
        event: &SearchPanelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            SearchPanelEvent::Open { path, line, query } => {
                self.open_path(
                    path.clone(),
                    OpenOptions {
                        pin: true,
                        line: Some(*line),
                        ..Default::default()
                    },
                    window,
                    cx,
                );
                if let Some(editor) = self.active_editor() {
                    let q = query.clone();
                    editor.update(cx, |e, cx| {
                        let line_start = e.state.doc.line_start(*line as usize - 1);
                        e.state.set_cursor(line_start);
                        e.find(&q, false, cx);
                    });
                }
            }
            SearchPanelEvent::ReplaceAll {
                query,
                replacement,
                files,
            } => {
                let mut on_disk = Vec::new();
                for path in files {
                    match self
                        .tabs
                        .iter()
                        .find(|t| t.editor.read(cx).path() == Some(path.as_path()))
                    {
                        Some(tab) => {
                            tab.editor
                                .update(cx, |e, cx| e.replace_all(query, replacement, cx));
                        }
                        None => on_disk.push(path.clone()),
                    }
                }
                for path in on_disk {
                    if let Err(err) = me_project::replace_in_file(&path, query, replacement, None) {
                        eprintln!("replace in {}: {err}", path.display());
                    }
                }
                if let Some(search) = &self.search_panel {
                    search.update(cx, |s, cx| s.rerun(cx));
                }
            }
            SearchPanelEvent::Dismiss => {
                self.sidebar_mode = SidebarMode::Files;
                self.focus_active(window, cx);
                cx.notify();
            }
        }
    }

    // ---------------------------------------------------------------------
    // Rendering
    // ---------------------------------------------------------------------

    fn render_sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::palette(cx).clone();
        let folder: SharedString = self
            .root
            .as_ref()
            .and_then(|r| r.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
            .into();
        let content: AnyElement = match (self.sidebar_mode, &self.file_tree, &self.search_panel) {
            (SidebarMode::Search, _, Some(search)) => search.clone().into_any_element(),
            (_, Some(tree), _) => tree.clone().into_any_element(),
            _ => div().into_any_element(),
        };
        let files_active = self.sidebar_mode == SidebarMode::Files;
        div()
            .id("sidebar")
            .w(px(self.sidebar_width))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .bg(palette.sidebar_bg)
            .border_r_1()
            .border_color(palette.border)
            .child(
                // Title-bar area: traffic lights live here on macOS.
                div()
                    .id("sidebar-titlebar")
                    .h(px(TITLE_BAR_HEIGHT))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_end()
                    .px(px(10.))
                    .gap(px(2.))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, _| this.should_move_window = true),
                    )
                    .on_mouse_move(cx.listener(|this, _: &MouseMoveEvent, window, _| {
                        if this.should_move_window {
                            this.should_move_window = false;
                            window.start_window_move();
                        }
                    }))
                    .on_click(|event, window, _| {
                        if event.click_count() == 2 {
                            window.titlebar_double_click();
                        }
                    })
                    .child(sidebar_button(
                        "files",
                        "icons/folder.svg",
                        files_active,
                        &palette,
                        cx.listener(|this, _, _, cx| {
                            this.sidebar_mode = SidebarMode::Files;
                            cx.notify();
                        }),
                    ))
                    .child(sidebar_button(
                        "search",
                        "icons/search.svg",
                        !files_active,
                        &palette,
                        cx.listener(|this, _, window, cx| {
                            this.show_search(window, cx);
                        }),
                    )),
            )
            .when(files_active && !folder.is_empty(), |el| {
                el.child(
                    div()
                        .px(px(16.))
                        .pb(px(4.))
                        .text_size(px(12.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(palette.text_tertiary)
                        .child(folder),
                )
            })
            .child(div().flex_1().min_h(px(0.)).child(content))
    }

    fn show_go_to_line(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab().filter(|t| t.has_source) else {
            return;
        };
        if tab.showing_preview() {
            self.toggle_preview(window, cx);
        }
        let Some(editor) = self.active_editor() else {
            return;
        };
        let (current, total) = {
            let e = editor.read(cx);
            let head = e.state.selections().primary().head;
            (e.state.doc.char_to_line(head) + 1, e.state.doc.len_lines())
        };
        self.go_to_line_open = true;
        self.go_to_line
            .update(cx, |g, cx| g.show(current, total, window, cx));
        cx.notify();
    }

    fn toggle_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette_open {
            self.close_palette(window, cx);
            return;
        }
        self.finder_open = false;
        self.go_to_line_open = false;
        let target = if self.picker_open {
            self.picker_open = false;
            self.picker_return.take()
        } else {
            window.focused(cx)
        };
        self.palette
            .update(cx, |p, cx| p.reset(target.as_ref(), window, cx));
        self.palette_return = target;
        self.palette_open = true;
        cx.notify();
    }

    fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette_open = false;
        match self.palette_return.take() {
            Some(handle) => window.focus(&handle, cx),
            None => self.focus_active(window, cx),
        }
        cx.notify();
    }

    fn on_palette_event(
        &mut self,
        _: &Entity<CommandPalette>,
        event: &PaletteEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            PaletteEvent::Dismiss => self.close_palette(window, cx),
            PaletteEvent::Run(action) => {
                self.close_palette(window, cx);
                // Runs where focus was, exactly like the key binding.
                let action = action.boxed_clone();
                window.defer(cx, move |window, cx| window.dispatch_action(action, cx));
            }
        }
    }

    fn show_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(search) = self.search_panel.clone() else {
            return;
        };
        self.sidebar_visible = true;
        self.sidebar_mode = SidebarMode::Search;
        let seed = self
            .active_editor()
            .and_then(|e| e.read(cx).selected_text());
        search.update(cx, |panel, cx| panel.show(seed, window, cx));
        cx.notify();
    }

    fn render_tab_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::palette(cx).clone();
        let ui_font = gpui::font(theme::fonts(cx).ui.clone());
        let text_system = window.text_system().clone();
        let inset = cfg!(target_os = "macos") && !(self.sidebar_visible && self.root.is_some());
        let details = self.tab_details(cx);
        let tabs: Vec<AnyElement> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(ix, tab)| {
                let editor = tab.editor.read(cx);
                let active = Some(ix) == self.active;
                let dirty = tab.is_dirty(cx);
                let title = editor.title();
                let detail = details[ix].clone();
                let icon = editor
                    .path()
                    .is_some()
                    .then(|| crate::file_icons::for_name(&title, palette.is_dark));
                let id = tab.id;
                let close_color = palette.text_secondary;
                let drop_highlight = palette.accent.opacity(0.18);
                // Size the tab to its title: padding, icon, gaps and the
                // close button take 64px, plus room for the ellipsis logic.
                let run = gpui::TextRun {
                    len: title.len(),
                    font: ui_font.clone(),
                    color: palette.text,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                let mut text_width = text_system
                    .shape_line(title.clone(), px(TAB_TEXT_SIZE), &[run.clone()], None)
                    .width;
                if let Some(detail) = &detail {
                    let run = gpui::TextRun {
                        len: detail.len(),
                        ..run
                    };
                    text_width += px(6.)
                        + text_system
                            .shape_line(detail.clone(), px(TAB_TEXT_SIZE - 1.5), &[run], None)
                            .width;
                }
                let width = (text_width + px(78.)).clamp(px(96.), px(300.));
                div()
                    .id(("tab", id as usize))
                    .group("tab")
                    .h_full()
                    .w(width)
                    .flex_none()
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .border_r_1()
                    .border_color(palette.border)
                    .text_size(px(TAB_TEXT_SIZE))
                    .when(active, |el| {
                        el.bg(palette.tab_active_bg).text_color(palette.text)
                    })
                    .when(!active, |el| {
                        el.text_color(palette.text_secondary)
                            .hover(|s| s.bg(palette.hover))
                    })
                    .when(!tab.pinned, |el| el.italic())
                    .when_some(icon.clone(), |el, icon| {
                        el.child(
                            svg()
                                .path(icon.path)
                                .size(px(22.))
                                .mx(px(-3.))
                                .flex_none()
                                .text_color(icon.color),
                        )
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .items_baseline()
                            .gap(px(6.))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(div().flex_none().child(title.clone()))
                            // Tabs with the same name show where they are.
                            .when_some(detail, |el, detail| {
                                el.child(
                                    div()
                                        .min_w(px(0.))
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .text_size(px(TAB_TEXT_SIZE - 1.5))
                                        .text_color(palette.text_tertiary)
                                        .child(detail),
                                )
                            }),
                    )
                    .child(
                        div()
                            .id(("close", id as usize))
                            .size(px(16.))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.))
                            .hover(|s| s.bg(palette.hover))
                            .when(dirty, |el| {
                                el.child(div().size(px(7.)).rounded_full().bg(palette.modified_dot))
                            })
                            .when(!dirty, |el| {
                                el.invisible()
                                    .group_hover("tab", |s| s.visible())
                                    .when(active, |el| el.visible())
                                    .child(
                                        svg()
                                            .path("icons/close.svg")
                                            .size(px(11.))
                                            .text_color(close_color),
                                    )
                            })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                if let Some(ix) = this.tabs.iter().position(|t| t.id == id) {
                                    this.close_tab(ix, window, cx);
                                }
                            })),
                    )
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        if let Some(ix) = this.tabs.iter().position(|t| t.id == id) {
                            if event.click_count() >= 2 {
                                this.tabs[ix].pinned = true;
                            }
                            this.activate(ix, window, cx);
                        }
                    }))
                    .on_mouse_down(
                        MouseButton::Middle,
                        cx.listener(move |this, _, window, cx| {
                            if let Some(ix) = this.tabs.iter().position(|t| t.id == id) {
                                this.close_tab(ix, window, cx);
                            }
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                            this.show_tab_menu(id, event.position, window, cx)
                        }),
                    )
                    .on_drag(
                        DraggedTab {
                            id,
                            title: title.clone(),
                            icon: icon.clone(),
                        },
                        |tab, _, _, cx| cx.new(|_| tab.clone()),
                    )
                    .drag_over::<DraggedTab>(move |style, dragged, _, _| {
                        if dragged.id == id {
                            style
                        } else {
                            style.bg(drop_highlight)
                        }
                    })
                    .on_drop(cx.listener(move |this, dragged: &DraggedTab, _, cx| {
                        this.move_tab(dragged.id, Some(id), cx)
                    }))
                    .into_any_element()
            })
            .collect();

        let preview_toggle = self
            .active_tab()
            .filter(|t| t.has_source)
            .and_then(|t| t.preview.as_ref().map(|_| t.show_preview));
        div()
            .id("tab-bar")
            .h(px(TITLE_BAR_HEIGHT))
            .flex_none()
            .flex()
            .items_stretch()
            .bg(palette.tab_bar_bg)
            .border_b_1()
            .border_color(palette.border)
            .when(inset, |el| el.pl(px(78.)))
            .child(
                div()
                    .id("tabs")
                    .flex()
                    .flex_1()
                    .min_w(px(0.))
                    .overflow_x_scroll()
                    .children(tabs)
                    .child(
                        // Empty space after the tabs drags the window.
                        div()
                            .id("tab-bar-drag")
                            .flex_1()
                            .min_w(px(40.))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, _, _| this.should_move_window = true),
                            )
                            .on_mouse_move(cx.listener(|this, _: &MouseMoveEvent, window, _| {
                                if this.should_move_window {
                                    this.should_move_window = false;
                                    window.start_window_move();
                                }
                            }))
                            .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                                // Double-clicking after the last tab adds a new
                                // file there.
                                if event.click_count() == 2 {
                                    let id = this.open_text("", window, cx);
                                    this.move_tab(id, None, cx);
                                }
                            }))
                            .on_drop(cx.listener(|this, dragged: &DraggedTab, _, cx| {
                                this.move_tab(dragged.id, None, cx)
                            })),
                    ),
            )
            .when_some(preview_toggle, |el, showing| {
                el.child(
                    div()
                        .id("preview-toggle")
                        .flex_none()
                        .flex()
                        .items_center()
                        .px(px(10.))
                        .gap(px(5.))
                        .text_size(px(13.))
                        .text_color(palette.text_secondary)
                        .hover(|s| s.text_color(palette.text))
                        .child(
                            svg()
                                .path(if showing {
                                    "icons/eye.svg"
                                } else {
                                    "icons/file-text.svg"
                                })
                                .size(px(14.))
                                .text_color(palette.text_secondary),
                        )
                        .child(if showing { "Preview" } else { "Source" })
                        .on_click(
                            cx.listener(|this, _, window, cx| this.toggle_preview(window, cx)),
                        ),
                )
            })
    }

    /// For tabs whose names clash, the shortest end of their folder path
    /// that tells them apart ("text" for crates/text/Cargo.toml).
    fn tab_details(&self, cx: &App) -> Vec<Option<SharedString>> {
        let paths: Vec<Option<PathBuf>> = self
            .tabs
            .iter()
            .map(|t| t.editor.read(cx).path().map(Path::to_path_buf))
            .collect();
        let name = |p: &PathBuf| p.file_name().map(|n| n.to_os_string());
        paths
            .iter()
            .map(|path| {
                let path = path.as_ref()?;
                let twins: Vec<&PathBuf> = paths
                    .iter()
                    .flatten()
                    .filter(|other| *other != path && name(other) == name(path))
                    .collect();
                if twins.is_empty() {
                    return None;
                }
                let dirs = |p: &Path| -> Vec<String> {
                    let parent = p.parent().unwrap_or(p);
                    let parent = self
                        .root
                        .as_ref()
                        .and_then(|r| parent.strip_prefix(r).ok())
                        .unwrap_or(parent);
                    parent
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy().into_owned())
                        .collect()
                };
                let mine = dirs(path);
                let others: Vec<Vec<String>> = twins.iter().map(|t| dirs(t)).collect();
                for take in 1..=mine.len().max(1) {
                    let suffix = &mine[mine.len().saturating_sub(take)..];
                    let clash = others
                        .iter()
                        .any(|o| o.len() >= suffix.len() && o[o.len() - suffix.len()..] == *suffix);
                    if !clash || take == mine.len() {
                        let text = suffix.join("/");
                        return Some(if text.is_empty() {
                            "./".into()
                        } else {
                            text.into()
                        });
                    }
                }
                None
            })
            .collect()
    }

    /// The path bar under the tabs: the file's folders (each opens a menu of
    /// what's in it) and the file.
    fn render_breadcrumbs(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let tab = self.active_tab()?;
        let path = tab.editor.read(cx).path()?.to_path_buf();
        let palette = theme::palette(cx).clone();
        // Relative to the window's folder when inside it, else from the top.
        let (mut dir, relative) = match self
            .root
            .as_ref()
            .and_then(|root| Some((root.clone(), path.strip_prefix(root).ok()?.to_path_buf())))
        {
            Some(inside) => inside,
            None => {
                let top: PathBuf = path.ancestors().last().unwrap_or(Path::new("/")).into();
                let relative = path.strip_prefix(&top).unwrap_or(&path).to_path_buf();
                (top, relative)
            }
        };
        let names: Vec<String> = relative
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        let chevron = || {
            svg()
                .path("icons/chevron-right.svg")
                .size(px(12.))
                .flex_none()
                .text_color(palette.text_tertiary)
        };
        let mut children: Vec<AnyElement> = Vec::new();
        for (ix, name) in names.iter().enumerate() {
            let last = ix + 1 == names.len();
            if last {
                let icon = crate::file_icons::for_name(name, palette.is_dark);
                children.push(
                    div()
                        .id("crumb-file")
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(2.))
                        .px(px(4.))
                        .rounded(px(4.))
                        .text_color(palette.text)
                        .hover(|s| s.bg(palette.hover))
                        .child(
                            svg()
                                .path(icon.path)
                                .size(px(20.))
                                .flex_none()
                                .text_color(icon.color),
                        )
                        .child(div().flex_none().child(name.clone()))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                                let path = this.active_path(cx);
                                if let Some(dir) = path.as_ref().and_then(|p| p.parent()) {
                                    folder_menu(dir, path.as_deref()).show(
                                        event.position,
                                        window,
                                        cx,
                                    );
                                }
                            }),
                        )
                        .into_any_element(),
                );
            } else {
                dir = dir.join(name);
                let menu_dir = dir.clone();
                let current = path.clone();
                children.push(
                    div()
                        .id(("crumb", ix))
                        .flex_none()
                        .px(px(4.))
                        .rounded(px(4.))
                        .hover(|s| s.bg(palette.hover).text_color(palette.text))
                        .child(name.clone())
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |_, event: &MouseDownEvent, window, cx| {
                                folder_menu(&menu_dir, Some(&current)).show(
                                    event.position,
                                    window,
                                    cx,
                                );
                            }),
                        )
                        .into_any_element(),
                );
                children.push(chevron().into_any_element());
            }
        }
        // Then the symbols around the cursor: impl › fn.
        let editor = tab.editor.clone();
        let symbols = editor.update(cx, |e, _| e.symbols());
        let at = {
            let e = editor.read(cx);
            e.state
                .doc
                .rope()
                .char_to_byte(e.state.selections().primary().head)
        };
        for (ix, symbol) in me_syntax::enclosing(&symbols, at).into_iter().enumerate() {
            children.push(chevron().into_any_element());
            children.push(
                div()
                    .id(("crumb-symbol", ix))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .px(px(4.))
                    .rounded(px(4.))
                    .hover(|s| s.bg(palette.hover).text_color(palette.text))
                    .child(Badge::symbol(symbol.kind, &palette).element())
                    .child(div().flex_none().child(symbol_label(symbol)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, window, cx| {
                            this.show_file_symbols(window, cx)
                        }),
                    )
                    .into_any_element(),
            );
        }
        Some(
            div()
                .id("breadcrumbs")
                .h(px(BREADCRUMBS_HEIGHT))
                .flex_none()
                .flex()
                .items_center()
                .gap(px(1.))
                .px(px(10.))
                .overflow_x_hidden()
                .whitespace_nowrap()
                .bg(palette.editor_bg)
                .text_size(px(12.5))
                .text_color(palette.text_secondary)
                .children(children)
                .into_any_element(),
        )
    }

    fn render_status_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::palette(cx).clone();
        let mut left: Vec<AnyElement> = Vec::new();
        let mut right: Vec<AnyElement> = Vec::new();
        let label = |text: SharedString| div().child(text).into_any_element();
        if let Some(build) = crate::updater::ready_build(cx) {
            right.push(
                div()
                    .id("status-update")
                    .h(px(20.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .rounded(px(4.))
                    .text_color(palette.accent)
                    .hover(|s| s.bg(palette.hover))
                    .child(SharedString::from(format!(
                        "Restart to Update (build {build})"
                    )))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| crate::updater::restart(cx))
                    .into_any_element(),
            );
        }
        if let Some(tab) = self.active_tab() {
            let editor = tab.editor.read(cx);
            let source_key = if cfg!(target_os = "macos") {
                "⇧⌘V"
            } else {
                "Ctrl+Shift+V"
            };
            if let (Some(preview), true) = (&tab.preview, tab.show_preview) {
                left.push(label(match preview {
                    Preview::Markdown(_) => {
                        format!("Markdown preview · {source_key} for source").into()
                    }
                    Preview::Image(image) if tab.has_source => {
                        format!("{} · {source_key} for source", image.read(cx).info).into()
                    }
                    Preview::Image(image) => image.read(cx).status(),
                }));
            } else {
                let sels = editor.state.selections();
                let primary = sels.primary();
                let (line, col) = editor.state.doc.line_col(primary.head);
                left.push(status_item(
                    "status-position",
                    format!("Ln {}, Col {}", line + 1, col + 1).into(),
                    &palette,
                    cx.listener(|this, _: &MouseDownEvent, window, cx| {
                        this.show_go_to_line(window, cx)
                    }),
                ));
                if sels.len() > 1 {
                    left.push(label(format!("{} selections", sels.len()).into()));
                } else if !primary.is_empty() {
                    left.push(label(
                        format!("{} selected", primary.end() - primary.start()).into(),
                    ));
                }
            }
            if let Some(notice) = &editor.notice
                && tab.has_source
            {
                left.push(label(notice.clone()));
            }
            let language = editor.language_name();
            if !tab.has_source {
                return div()
                    .h(px(STATUS_BAR_HEIGHT))
                    .flex_none()
                    .flex()
                    .items_center()
                    .px(px(14.))
                    .border_t_1()
                    .border_color(palette.border)
                    .bg(palette.tab_bar_bg)
                    .text_size(px(12.5))
                    .text_color(palette.text_secondary)
                    .children(left);
            }
            if let Some(info) = self.lsp.as_ref().and_then(|l| l.read(cx).info(&tab.editor)) {
                let stopped = matches!(info.status, Status::Stopped(_));
                let text: SharedString = match &info.status {
                    Status::Starting => format!("{} starting…", info.name).into(),
                    Status::Busy(work) => {
                        let work: String = work.chars().take(48).collect();
                        format!("{}: {work}", info.name).into()
                    }
                    Status::Ready => info.name.clone(),
                    Status::Stopped(_) => format!("{} stopped", info.name).into(),
                };
                let detail = match &info.status {
                    Status::Stopped(detail) if !detail.is_empty() => Some(detail.clone()),
                    _ => None,
                };
                let name = info.name.clone();
                right.push(status_item(
                    "status-lsp",
                    text,
                    &palette,
                    cx.listener(move |_, event: &MouseDownEvent, window, cx| {
                        server_menu(&name, stopped, detail.as_deref()).show(
                            event.position,
                            window,
                            cx,
                        )
                    }),
                ));
            }
            right.push(status_item(
                "status-language",
                language.unwrap_or("Plain Text").into(),
                &palette,
                cx.listener(move |_, event: &MouseDownEvent, window, cx| {
                    language_menu(language).show(event.position, window, cx)
                }),
            ));
            right.push(label(
                match editor.state.doc.line_ending() {
                    me_text::LineEnding::Lf => "LF",
                    me_text::LineEnding::CrLf => "CRLF",
                }
                .into(),
            ));
            let (hard_tabs, width) = (editor.state.hard_tabs, editor.state.tab_width);
            right.push(status_item(
                "status-indent",
                if hard_tabs {
                    format!("Tab Size: {width}").into()
                } else {
                    format!("Spaces: {width}").into()
                },
                &palette,
                cx.listener(move |_, event: &MouseDownEvent, window, cx| {
                    indentation_menu(hard_tabs, width).show(event.position, window, cx)
                }),
            ));
        }
        if let Some(lsp) = &self.lsp {
            let lsp = lsp.read(cx);
            let (errors, warnings) = lsp.problem_counts();
            let has_server = self
                .active_editor()
                .is_some_and(|editor| lsp.info(&editor).is_some());
            if errors + warnings > 0 || has_server {
                left.push(problems_item(errors, warnings, &palette, cx));
            }
        }
        if let Some(flash) = &self.flash {
            left.push(
                div()
                    .text_color(palette.text)
                    .child(flash.clone())
                    .into_any_element(),
            );
        }
        div()
            .h(px(STATUS_BAR_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .px(px(8.))
            .border_t_1()
            .border_color(palette.border)
            .bg(palette.tab_bar_bg)
            .text_size(px(12.5))
            .text_color(palette.text_secondary)
            .child(div().flex().items_center().gap(px(8.)).children(left))
            .child(div().flex().items_center().gap(px(8.)).children(right))
    }

    fn render_empty(&self, cx: &App) -> impl IntoElement {
        let palette = theme::palette(cx);
        let hint = |keys: &'static str, label: &'static str| {
            div()
                .flex()
                .gap(px(10.))
                .child(
                    div()
                        .w(px(110.))
                        .whitespace_nowrap()
                        .text_align(gpui::TextAlign::Right)
                        .text_color(palette.text_secondary)
                        .child(keys),
                )
                .child(div().text_color(palette.text_tertiary).child(label))
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(14.))
            .bg(palette.editor_bg)
            .child(
                svg()
                    .path("brand/logo-mark.svg")
                    .size(px(72.))
                    .text_color(palette.text_tertiary.opacity(0.5)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .text_size(px(14.))
                    .child(hint(
                        if cfg!(target_os = "macos") {
                            "⌘T"
                        } else {
                            "Ctrl+T"
                        },
                        "Go to file",
                    ))
                    .child(hint(
                        if cfg!(target_os = "macos") {
                            "⇧⌘F"
                        } else {
                            "Ctrl+Shift+F"
                        },
                        "Find in project",
                    ))
                    .child(hint(
                        if cfg!(target_os = "macos") {
                            "⌘O"
                        } else {
                            "Ctrl+O"
                        },
                        "Open file or folder",
                    ))
                    .child(hint(
                        if cfg!(target_os = "macos") {
                            "⌘B"
                        } else {
                            "Ctrl+B"
                        },
                        "Toggle sidebar",
                    )),
            )
    }
}

/// A clickable status bar entry. It reacts on mouse down, so a menu opens
/// where the pointer is, like native status bar items.
fn status_item(
    id: &'static str,
    text: SharedString,
    palette: &theme::Palette,
    on_press: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .h(px(20.))
        .px(px(6.))
        .flex()
        .items_center()
        .rounded(px(4.))
        .hover(|s| s.bg(palette.hover).text_color(palette.text))
        .child(text)
        .on_mouse_down(MouseButton::Left, on_press)
        .into_any_element()
}

/// Error and warning counts; click for the problems list.
fn problems_item(
    errors: usize,
    warnings: usize,
    palette: &theme::Palette,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let count = |icon: &'static str, n: usize, color: gpui::Hsla| {
        div()
            .flex()
            .items_center()
            .gap(px(3.))
            .child(svg().path(icon).size(px(13.)).text_color(if n > 0 {
                color
            } else {
                palette.text_secondary
            }))
            .child(n.to_string())
    };
    div()
        .id("status-problems")
        .h(px(20.))
        .px(px(6.))
        .flex()
        .items_center()
        .gap(px(8.))
        .rounded(px(4.))
        .hover(|s| s.bg(palette.hover).text_color(palette.text))
        .child(count("icons/circle-x.svg", errors, palette.error))
        .child(count("icons/triangle-alert.svg", warnings, palette.warning))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _, window, cx| this.show_problems(window, cx)),
        )
        .into_any_element()
}

/// What's in `dir`, for a path bar segment: files open, folders show their
/// own contents one level down.
fn folder_menu(dir: &Path, current: Option<&Path>) -> gpui_component::native_menu::NativeMenu {
    use gpui_component::{Icon, native_menu::NativeMenu};
    const MAX_ENTRIES: usize = 300;
    const MAX_SUBMENUS: usize = 40;
    let file_item = |menu: NativeMenu, entry: &me_project::DirEntry| {
        // Menu icons take the text color; only the shape matters.
        let icon = crate::file_icons::for_name(&entry.name, false);
        let label = if Some(entry.path.as_path()) == current {
            format!("{}  ✓", entry.name)
        } else {
            entry.name.clone()
        };
        menu.menu_with_icon(
            label,
            Icon::empty().path(icon.path),
            Box::new(OpenFile {
                path: entry.path.clone(),
            }),
        )
    };
    let folder_item = |menu: NativeMenu, entry: &me_project::DirEntry| {
        menu.menu_with_icon(
            entry.name.clone(),
            Icon::empty().path("icons/folder.svg"),
            Box::new(RevealPath {
                path: entry.path.clone(),
            }),
        )
    };
    let entries = me_project::list_dir(dir).unwrap_or_default();
    let mut menu = NativeMenu::new();
    let mut submenus = 0;
    for entry in entries.iter().take(MAX_ENTRIES) {
        if !entry.is_dir {
            menu = file_item(menu, entry);
        } else if submenus < MAX_SUBMENUS {
            submenus += 1;
            let mut sub = NativeMenu::new();
            for child in me_project::list_dir(&entry.path)
                .unwrap_or_default()
                .iter()
                .take(MAX_ENTRIES)
            {
                sub = if child.is_dir {
                    folder_item(sub, child)
                } else {
                    file_item(sub, child)
                };
            }
            if sub.is_empty() {
                menu = folder_item(menu, entry);
            } else {
                menu = menu.submenu(entry.name.clone(), sub);
            }
        } else {
            menu = folder_item(menu, entry);
        }
    }
    if entries.len() > MAX_ENTRIES {
        menu = menu.separator().menu(
            format!("{} more in the sidebar…", entries.len() - MAX_ENTRIES),
            Box::new(RevealPath {
                path: dir.to_path_buf(),
            }),
        );
    }
    menu
}

/// The language server's status bar menu.
fn server_menu(
    name: &str,
    stopped: bool,
    detail: Option<&str>,
) -> gpui_component::native_menu::NativeMenu {
    let mut menu = gpui_component::native_menu::NativeMenu::new();
    if let Some(detail) = detail {
        let detail: String = detail.chars().take(80).collect();
        menu = menu
            .menu_with_disabled(detail, true, Box::new(RestartLanguageServer))
            .separator();
    }
    menu = menu.menu(
        if stopped {
            format!("Start {name}")
        } else {
            format!("Restart {name}")
        },
        Box::new(RestartLanguageServer),
    );
    if !stopped {
        menu = menu.menu(format!("Stop {name}"), Box::new(StopLanguageServer));
    }
    menu
}

fn language_menu(current: Option<&'static str>) -> gpui_component::native_menu::NativeMenu {
    let mut menu = gpui_component::native_menu::NativeMenu::new().menu_with_check(
        "Plain Text",
        current.is_none(),
        Box::new(SetLanguage { name: None }),
    );
    menu = menu.separator();
    for language in me_syntax::LANGUAGES {
        menu = menu.menu_with_check(
            language.name,
            current == Some(language.name),
            Box::new(SetLanguage {
                name: Some(language.name.into()),
            }),
        );
    }
    menu
}

fn indentation_menu(hard_tabs: bool, width: usize) -> gpui_component::native_menu::NativeMenu {
    let mut menu = gpui_component::native_menu::NativeMenu::new()
        .menu_with_check(
            "Indent Using Spaces",
            !hard_tabs,
            Box::new(SetIndentation {
                hard_tabs: false,
                width,
            }),
        )
        .menu_with_check(
            "Indent Using Tabs",
            hard_tabs,
            Box::new(SetIndentation {
                hard_tabs: true,
                width,
            }),
        )
        .separator();
    for w in [2, 3, 4, 8] {
        menu = menu.menu_with_check(
            format!("Width: {w}"),
            w == width,
            Box::new(SetIndentation {
                hard_tabs,
                width: w,
            }),
        );
    }
    menu
}

fn sidebar_button(
    id: &'static str,
    icon: &'static str,
    active: bool,
    palette: &theme::Palette,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .size(px(26.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .when(active, |el| el.bg(palette.hover))
        .hover(|s| s.bg(palette.hover))
        .child(svg().path(icon).size(px(15.)).text_color(if active {
            palette.text
        } else {
            palette.text_secondary
        }))
        .on_click(on_click)
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::trace_first_frame();
        let palette = theme::palette(cx).clone();
        let show_sidebar = self.sidebar_visible && self.root.is_some();
        let content: AnyElement = match self.active_tab() {
            Some(tab) => match (&tab.preview, tab.show_preview) {
                (Some(preview), true) => preview.element(),
                _ => tab.editor.clone().into_any_element(),
            },
            None => self.render_empty(cx).into_any_element(),
        };
        let finder = self.finder.clone().filter(|_| self.finder_open);
        let command_palette = self.palette_open.then(|| self.palette.clone());
        let picker = self.picker_open.then(|| self.picker.clone());
        let rename = self
            .rename_target
            .as_ref()
            .map(|target| (self.rename.clone(), target.anchor));
        let mut context = KeyContext::new_with_defaults();
        context.add("Workspace");

        div()
            .id("workspace")
            .key_context(context)
            .track_focus(&self.focus_handle)
            .size_full()
            .relative()
            .flex()
            .text_color(palette.text)
            .font_family(theme::fonts(cx).ui.clone())
            .on_action(cx.listener(|_, _: &CloseWindow, window, _| window.remove_window()))
            .on_action(cx.listener(|this, action: &OpenFile, window, cx| {
                this.open_path(
                    action.path.clone(),
                    OpenOptions {
                        pin: true,
                        ..Default::default()
                    },
                    window,
                    cx,
                );
            }))
            .on_action(cx.listener(|this, action: &RevealPath, _, cx| {
                if let Some(tree) = this.file_tree.clone() {
                    this.sidebar_visible = true;
                    this.sidebar_mode = SidebarMode::Files;
                    tree.update(cx, |t, cx| t.reveal(&action.path, cx));
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleCommandPalette, window, cx| {
                this.toggle_palette(window, cx)
            }))
            .on_action(cx.listener(|this, action: &SetLanguage, _, cx| {
                // Reached when the editor isn't focused (e.g. in the preview).
                if let Some(editor) = this.active_editor() {
                    let language = action
                        .name
                        .as_ref()
                        .and_then(|n| me_syntax::language_by_name(n));
                    editor.update(cx, |e, cx| e.set_language(language, cx));
                }
            }))
            .on_action(cx.listener(|this, action: &SetIndentation, _, cx| {
                if let Some(editor) = this.active_editor() {
                    editor.update(cx, |e, cx| {
                        e.set_indentation(action.hard_tabs, action.width, cx)
                    });
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleFileFinder, window, cx| {
                this.palette_open = false;
                this.picker_open = false;
                if this.finder_open {
                    this.finder_open = false;
                    this.focus_active(window, cx);
                } else if let Some(finder) = this.finder.clone() {
                    this.finder_open = true;
                    let recent = this.recent.clone();
                    finder.update(cx, |f, cx| f.reset(recent, window, cx));
                }
                cx.notify();
            }))
            .on_action(
                cx.listener(|this, _: &GoToLine, window, cx| this.show_go_to_line(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ToggleProjectSearch, window, cx| {
                    this.show_search(window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &Find, window, cx| this.open_find_bar(window, cx)))
            .on_action(
                cx.listener(|this, _: &FindNext, window, cx| this.find_step(false, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &FindPrevious, window, cx| this.find_step(true, window, cx)),
            )
            .on_action(cx.listener(|this, _: &UseSelectionForFind, window, cx| {
                if let Some(text) = this
                    .active_editor()
                    .and_then(|e| e.read(cx).selected_text())
                {
                    this.find_bar
                        .update(cx, |bar, cx| bar.set_text(&text, window, cx));
                    this.sync_find_highlights(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| {
                this.sidebar_visible = !this.sidebar_visible;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &RevealInSidebar, _, cx| {
                let path = this
                    .active_editor()
                    .and_then(|e| e.read(cx).path().map(Path::to_path_buf));
                if let (Some(tree), Some(path)) = (this.file_tree.clone(), path) {
                    this.sidebar_visible = true;
                    this.sidebar_mode = SidebarMode::Files;
                    tree.update(cx, |t, cx| t.reveal(&path, cx));
                }
            }))
            .on_action(
                cx.listener(|this, _: &TogglePreview, window, cx| this.toggle_preview(window, cx)),
            )
            .on_action(cx.listener(|this, _: &RevealInFinder, _, cx| {
                if let Some(path) = this.active_path(cx) {
                    cx.reveal_path(&path);
                }
            }))
            .on_action(cx.listener(|this, _: &CopyPath, _, cx| {
                if let Some(path) = this.active_path(cx) {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                        path.to_string_lossy().into_owned(),
                    ));
                }
            }))
            .on_action(cx.listener(|this, _: &CopyRelativePath, _, cx| {
                if let Some(path) = this.active_path(cx) {
                    let relative = this.relative_path(&path);
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(relative));
                }
            }))
            .on_action(cx.listener(|this, _: &CloseOtherTabs, window, cx| {
                let active = this.active;
                this.close_tabs_where(|ix| Some(ix) != active, window, cx);
            }))
            .on_action(cx.listener(|this, _: &CloseTabsToTheRight, window, cx| {
                let active = this.active.unwrap_or(usize::MAX);
                this.close_tabs_where(|ix| ix > active, window, cx);
            }))
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                if this.finder_open {
                    this.finder_open = false;
                    cx.notify();
                } else if let Some(ix) = this.active {
                    this.close_tab(ix, window, cx);
                } else {
                    window.remove_window();
                }
            }))
            .on_action(cx.listener(|this, _: &ReopenClosedTab, window, cx| {
                if let Some(path) = this.closed.pop() {
                    this.open_path(
                        path,
                        OpenOptions {
                            pin: true,
                            ..Default::default()
                        },
                        window,
                        cx,
                    );
                }
            }))
            .on_action(cx.listener(|this, _: &NextTab, window, cx| {
                if let Some(a) = this.active.filter(|_| !this.tabs.is_empty()) {
                    this.activate((a + 1) % this.tabs.len(), window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &PreviousTab, window, cx| {
                if let Some(a) = this.active.filter(|_| !this.tabs.is_empty()) {
                    this.activate((a + this.tabs.len() - 1) % this.tabs.len(), window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ActivateTab1, w, cx| this.activate(0, w, cx)))
            .on_action(cx.listener(|this, _: &ActivateTab2, w, cx| this.activate(1, w, cx)))
            .on_action(cx.listener(|this, _: &ActivateTab3, w, cx| this.activate(2, w, cx)))
            .on_action(cx.listener(|this, _: &ActivateTab4, w, cx| this.activate(3, w, cx)))
            .on_action(cx.listener(|this, _: &ActivateTab5, w, cx| this.activate(4, w, cx)))
            .on_action(cx.listener(|this, _: &ActivateTab6, w, cx| this.activate(5, w, cx)))
            .on_action(cx.listener(|this, _: &ActivateTab7, w, cx| this.activate(6, w, cx)))
            .on_action(cx.listener(|this, _: &ActivateTab8, w, cx| this.activate(7, w, cx)))
            .on_action(cx.listener(|this, _: &ActivateTab9, w, cx| {
                let last = this.tabs.len().saturating_sub(1);
                this.activate(last, w, cx)
            }))
            .on_action(cx.listener(|this, _: &NewFile, window, cx| {
                this.open_text("", window, cx);
            }))
            .on_action(cx.listener(|this, _: &GoToDefinition, window, cx| {
                this.go_to_definition_at_cursor(window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &FindReferences, window, cx| {
                    this.find_references(window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &GoToSymbol, window, cx| {
                if this.picker_open {
                    this.on_picker_event(&this.picker.clone(), &PickerEvent::Dismiss, window, cx);
                } else {
                    this.show_file_symbols(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &GoToProjectSymbol, window, cx| {
                this.show_project_symbols(window, cx)
            }))
            .on_action(cx.listener(|this, _: &RestartLanguageServer, _, cx| {
                if let (Some(lsp), Some(editor)) = (this.lsp.clone(), this.active_editor()) {
                    lsp.update(cx, |lsp, cx| lsp.restart(&editor, cx));
                }
            }))
            .on_action(
                cx.listener(|this, _: &RenameSymbol, window, cx| this.rename_symbol(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ShowProblems, window, cx| this.show_problems(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &NextProblem, window, cx| {
                    this.next_problem(false, window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &PreviousProblem, window, cx| {
                this.next_problem(true, window, cx)
            }))
            .on_action(cx.listener(|this, _: &StopLanguageServer, _, cx| {
                if let (Some(lsp), Some(editor)) = (this.lsp.clone(), this.active_editor()) {
                    lsp.update(cx, |lsp, cx| lsp.stop(&editor, cx));
                }
            }))
            .on_action(
                cx.listener(|this, _: &NavigateBack, window, cx| this.navigate(true, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &NavigateForward, window, cx| {
                    this.navigate(false, window, cx)
                }),
            )
            .on_mouse_down(
                MouseButton::Navigate(gpui::NavigationDirection::Back),
                cx.listener(|this, _, window, cx| this.navigate(true, window, cx)),
            )
            .on_mouse_down(
                MouseButton::Navigate(gpui::NavigationDirection::Forward),
                cx.listener(|this, _, window, cx| this.navigate(false, window, cx)),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                if this.resizing_sidebar {
                    if event.pressed_button == Some(MouseButton::Left) {
                        this.sidebar_width =
                            f32::from(event.position.x).clamp(MIN_SIDEBAR, MAX_SIDEBAR);
                    } else {
                        this.resizing_sidebar = false;
                    }
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.should_move_window = false;
                    if this.resizing_sidebar {
                        this.resizing_sidebar = false;
                        cx.notify();
                    }
                }),
            )
            .when(show_sidebar, |el| {
                el.child(self.render_sidebar(cx)).child(
                    // Resize handle straddling the sidebar border.
                    div()
                        .id("sidebar-resize")
                        .absolute()
                        .left(px(self.sidebar_width - 3.))
                        .top_0()
                        .h_full()
                        .w(px(6.))
                        .cursor(CursorStyle::ResizeLeftRight)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.resizing_sidebar = true;
                                cx.stop_propagation();
                            }),
                        ),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .h_full()
                    .flex()
                    .flex_col()
                    .bg(palette.editor_bg)
                    .child(self.render_tab_bar(window, cx))
                    .children(self.render_breadcrumbs(cx))
                    .when(self.find_bar_open, |el| el.child(self.find_bar.clone()))
                    .child(div().flex_1().min_h(px(0.)).child(content))
                    .child(self.render_status_bar(cx)),
            )
            .when(self.go_to_line_open, |el| {
                el.child(
                    div()
                        .absolute()
                        .top(px(TITLE_BAR_HEIGHT + 8.))
                        .left_0()
                        .right_0()
                        .flex()
                        .justify_center()
                        .child(self.go_to_line.clone()),
                )
            })
            .when_some(command_palette, |el, command_palette| {
                el.child(
                    div()
                        .absolute()
                        .top(px(TITLE_BAR_HEIGHT + 8.))
                        .left_0()
                        .right_0()
                        .flex()
                        .justify_center()
                        .child(command_palette),
                )
            })
            .when_some(finder, |el, finder| {
                el.child(
                    div()
                        .absolute()
                        .top(px(TITLE_BAR_HEIGHT + 8.))
                        .left_0()
                        .right_0()
                        .flex()
                        .justify_center()
                        .child(finder),
                )
            })
            .when_some(picker, |el, picker| {
                el.child(
                    div()
                        .absolute()
                        .top(px(TITLE_BAR_HEIGHT + 8.))
                        .left_0()
                        .right_0()
                        .flex()
                        .justify_center()
                        .child(picker),
                )
            })
            .when_some(rename, |el, (rename, anchor)| match anchor {
                Some(anchor) => el.child(deferred(
                    anchored()
                        .position(anchor)
                        .snap_to_window_with_margin(px(8.))
                        .child(rename),
                )),
                None => el.child(
                    div()
                        .absolute()
                        .top(px(TITLE_BAR_HEIGHT + 8.))
                        .left_0()
                        .right_0()
                        .flex()
                        .justify_center()
                        .child(rename),
                ),
            })
    }
}

/// Applies a rename's edits to a file that isn't open.
fn edit_file(file: &FileEdit, encoding: me_lsp::Encoding) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(&file.path)?;
    let mut rope = me_text::Rope::from_str(&text);
    let changes = me_text::ChangeSet::new(
        crate::lsp::char_edits(&rope, &file.edits, encoding)
            .into_iter()
            .map(|(range, text)| me_text::Change::new(range.start, range.end, text))
            .collect(),
    );
    changes.apply(&mut rope);
    let mut bytes = Vec::with_capacity(rope.len_bytes());
    rope.write_to(&mut bytes)?;
    crate::editor::write_atomically(&file.path, &bytes)?;
    Ok(())
}

/// How a symbol reads in lists: impl blocks as `impl Name`.
fn symbol_label(symbol: &me_syntax::Symbol) -> String {
    match symbol.kind {
        SymbolKind::Impl => format!("impl {}", symbol.name),
        _ => symbol.name.clone(),
    }
}

/// Waits for a language server's answer: says so in the status bar when
/// it's slow, and gives up after [`LSP_TIMEOUT`] (`None`; the caller falls
/// back).
async fn wait_for_server<T: Unpin + 'static>(
    this: &gpui::WeakEntity<Workspace>,
    request: Task<anyhow::Result<T>>,
    name: SharedString,
    cx: &mut gpui::AsyncWindowContext,
) -> Option<anyhow::Result<T>> {
    use futures::future::{Either, select};
    let slow = cx
        .background_executor()
        .timer(std::time::Duration::from_millis(400));
    let request = match select(request, slow).await {
        Either::Left((result, _)) => return Some(result),
        Either::Right((_, request)) => request,
    };
    this.update(cx, |this, cx| {
        this.flash_message(format!("Asking {name}…"), cx)
    })
    .ok();
    let timeout = cx.background_executor().timer(LSP_TIMEOUT);
    let result = match select(request, timeout).await {
        Either::Left((result, _)) => Some(result),
        Either::Right(_) => None,
    };
    this.update(cx, |this, cx| {
        this.flash = None;
        cx.notify();
    })
    .ok();
    result
}

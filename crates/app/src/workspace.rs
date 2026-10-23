//! One window: sidebar (files or project search), tabs, the active editor or
//! markdown preview, the find bar, the Cmd+T finder and a status bar.

use std::path::{Path, PathBuf};

use gpui::{
    AnyElement, App, AppContext, ClickEvent, Context, CursorStyle, Entity, FocusHandle, Focusable,
    InteractiveElement, IntoElement, KeyContext, MouseButton, MouseMoveEvent, ParentElement,
    PromptLevel, Render, SharedString, StatefulInteractiveElement, Styled, Subscription, Window,
    div, prelude::FluentBuilder, px, svg,
};

use crate::actions::*;
use crate::app_state;
use crate::editor::{EditorEvent, EditorView};
use crate::file_tree::{FileTree, FileTreeEvent};
use crate::find_bar::{FindBar, FindBarEvent};
use crate::finder::{FinderEvent, FinderView};
use crate::goto_line::{GoToLineEvent, GoToLinePrompt};
use crate::markdown::{MarkdownEvent, MarkdownPreview, is_markdown};
use crate::search_panel::{SearchPanel, SearchPanelEvent};
use crate::theme;

/// Height of the title/tab bar. On macOS the traffic lights sit in it.
pub const TITLE_BAR_HEIGHT: f32 = if cfg!(target_os = "macos") { 38. } else { 34. };
const STATUS_BAR_HEIGHT: f32 = 24.;
const MIN_SIDEBAR: f32 = 160.;
const MAX_SIDEBAR: f32 = 600.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SidebarMode {
    Files,
    Search,
}

pub struct Tab {
    pub id: u64,
    pub editor: Entity<EditorView>,
    pub preview: Option<Entity<MarkdownPreview>>,
    pub show_preview: bool,
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
}

#[derive(Clone, Debug, Default)]
pub struct OpenOptions {
    pub pin: bool,
    pub line: Option<u32>,
    pub column: Option<u32>,
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
    tabs: Vec<Tab>,
    active: Option<usize>,
    closed: Vec<PathBuf>,
    recent: Vec<PathBuf>,
    should_move_window: bool,
    /// The user already agreed to discard unsaved changes on close.
    pub close_confirmed: bool,
    _subscriptions: Vec<Subscription>,
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
        let mut subscriptions = vec![
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
        subscriptions.push(cx.observe_window_appearance(window, |_, window, cx| {
            theme::apply(window.appearance(), cx);
        }));
        subscriptions.push(cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.on_window_activated(cx);
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
            tabs: Vec::new(),
            active: None,
            closed: Vec::new(),
            recent: Vec::new(),
            should_move_window: false,
            close_confirmed: false,
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
                            ..Default::default()
                        },
                        window,
                        cx,
                    );
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
        for tab in &self.tabs {
            tab.editor
                .update(cx, |editor, cx| editor.reload_if_changed(cx));
        }
    }

    pub fn tab_ids(&self) -> Vec<u64> {
        self.tabs.iter().map(|t| t.id).collect()
    }

    pub fn dirty_count(&self, cx: &App) -> usize {
        self.tabs
            .iter()
            .filter(|t| t.editor.read(cx).is_dirty())
            .count()
    }

    fn active_tab(&self) -> Option<&Tab> {
        self.active.and_then(|ix| self.tabs.get(ix))
    }

    pub fn active_editor(&self) -> Option<Entity<EditorView>> {
        self.active_tab().map(|t| t.editor.clone())
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
            (None, None) => "Editor".into(),
        };
        window.set_window_title(&title);
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
            self.activate(ix, window, cx);
            if let Some(line) = options.line {
                self.tabs[ix]
                    .editor
                    .update(cx, |e, cx| e.go_to(line, options.column, cx));
            }
            return self.tabs[ix].id;
        }

        let editor = cx.new(|cx| EditorView::open(path.clone(), window, cx));
        if let Some(line) = options.line {
            editor.update(cx, |e, cx| e.go_to(line, options.column, cx));
        }
        let tab = self.make_tab(editor, options.pin, is_markdown(&path), window, cx);
        let id = tab.id;

        // Reuse an unpinned, unmodified preview tab.
        let reuse = self.active.filter(|&ix| {
            !options.pin && !self.tabs[ix].pinned && !self.tabs[ix].editor.read(cx).is_dirty()
        });
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
        self.recent.retain(|p| p != &path);
        self.recent.insert(0, path);
        self.recent.truncate(50);
        self.activate(ix, window, cx);
        id
    }

    /// Opens a new tab with `text` (stdin, or a new untitled file).
    pub fn open_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) -> u64 {
        let editor = cx.new(|cx| EditorView::with_text(text, window, cx));
        let tab = self.make_tab(editor, true, false, window, cx);
        let id = tab.id;
        let ix = self.active.map_or(self.tabs.len(), |a| a + 1);
        self.tabs.insert(ix, tab);
        self.activate(ix, window, cx);
        id
    }

    fn make_tab(
        &mut self,
        editor: Entity<EditorView>,
        pinned: bool,
        markdown: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Tab {
        let id = app_state::next_tab_id(cx);
        let mut subscriptions = vec![cx.subscribe_in(
            &editor,
            window,
            move |this, _, event: &EditorEvent, window, cx| match event {
                EditorEvent::Changed | EditorEvent::DirtyChanged => {
                    if let Some(tab) = this.tabs.iter_mut().find(|t| t.id == id)
                        && !tab.pinned
                        && matches!(event, EditorEvent::DirtyChanged)
                    {
                        tab.pinned = true;
                    }
                    cx.notify();
                }
                EditorEvent::Saved => {
                    if let Some(tree) = &this.file_tree {
                        tree.update(cx, |tree, cx| tree.refresh(cx));
                    }
                    this.update_title(window, cx);
                    cx.notify();
                }
                EditorEvent::Loaded => cx.notify(),
            },
        )];
        let preview = markdown.then(|| {
            let preview = cx.new(|cx| MarkdownPreview::new(editor.clone(), window, cx));
            subscriptions.push(cx.subscribe_in(
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
            ));
            preview
        });
        Tab {
            id,
            editor,
            show_preview: preview.is_some(),
            preview,
            pinned,
            _subscriptions: subscriptions,
        }
    }

    pub fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        self.active = Some(ix);
        let tab = &self.tabs[ix];
        window.focus(&tab.content_focus(cx), cx);
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
        if !tab.editor.read(cx).is_dirty() {
            self.remove_tab(ix, window, cx);
            return;
        }
        let id = tab.id;
        let editor = tab.editor.clone();
        let title = editor.read(cx).title();
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
                let save = this.update_in(cx, |_, window, cx| {
                    editor.update(cx, |e, cx| e.save(window, cx))
                });
                let Ok(save) = save else { return };
                if save.await.is_err() || editor.read_with(cx, |e, _| e.is_dirty()) {
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
        let tab = &mut self.tabs[ix];
        if tab.preview.is_none() {
            let editor = tab.editor.clone();
            let preview = cx.new(|cx| MarkdownPreview::new(editor, window, cx));
            let tab = &mut self.tabs[ix];
            tab.preview = Some(preview);
            tab.show_preview = true;
        } else {
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
        let Some(tab) = self.active_tab() else { return };
        if tab.show_preview && tab.preview.is_some() {
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
                        column: None,
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
                    if let Err(err) =
                        editor_project::replace_in_file(&path, query, replacement, None)
                    {
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
                        .text_size(px(11.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(palette.text_tertiary)
                        .child(folder),
                )
            })
            .child(div().flex_1().min_h(px(0.)).child(content))
    }

    fn show_go_to_line(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab() else { return };
        if tab.show_preview && tab.preview.is_some() {
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

    fn render_tab_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::palette(cx).clone();
        let inset = cfg!(target_os = "macos") && !(self.sidebar_visible && self.root.is_some());
        let tabs: Vec<AnyElement> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(ix, tab)| {
                let editor = tab.editor.read(cx);
                let active = Some(ix) == self.active;
                let dirty = editor.is_dirty();
                let title = editor.title();
                let id = tab.id;
                let close_color = palette.text_secondary;
                div()
                    .id(("tab", id as usize))
                    .group("tab")
                    .h_full()
                    .min_w(px(110.))
                    .max_w(px(220.))
                    .flex_shrink(1.)
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .border_r_1()
                    .border_color(palette.border)
                    .text_size(px(12.5))
                    .when(active, |el| {
                        el.bg(palette.tab_active_bg).text_color(palette.text)
                    })
                    .when(!active, |el| {
                        el.text_color(palette.text_secondary)
                            .hover(|s| s.bg(palette.hover))
                    })
                    .when(!tab.pinned, |el| el.italic())
                    .child(
                        div()
                            .flex_1()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(title),
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
                    .into_any_element()
            })
            .collect();

        let preview_toggle = self
            .active_tab()
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
                                if event.click_count() == 2 {
                                    if cfg!(target_os = "macos") {
                                        window.titlebar_double_click();
                                    } else {
                                        this.open_text("", window, cx);
                                    }
                                }
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
                        .text_size(px(12.))
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

    fn render_status_bar(&self, cx: &App) -> impl IntoElement {
        let palette = theme::palette(cx);
        let mut left: Vec<SharedString> = Vec::new();
        let mut right: Vec<SharedString> = Vec::new();
        if let Some(tab) = self.active_tab() {
            let editor = tab.editor.read(cx);
            if tab.show_preview && tab.preview.is_some() {
                left.push(
                    if cfg!(target_os = "macos") {
                        "Markdown preview · ⇧⌘V for source"
                    } else {
                        "Markdown preview · Ctrl+Shift+V for source"
                    }
                    .into(),
                );
            } else {
                let sels = editor.state.selections();
                let primary = sels.primary();
                let (line, col) = editor.state.doc.line_col(primary.head);
                left.push(format!("Ln {}, Col {}", line + 1, col + 1).into());
                if sels.len() > 1 {
                    left.push(format!("{} selections", sels.len()).into());
                } else if !primary.is_empty() {
                    left.push(format!("{} selected", primary.end() - primary.start()).into());
                }
            }
            if let Some(notice) = &editor.notice {
                left.push(notice.clone());
            }
            right.push(editor.language_name().unwrap_or("Plain Text").into());
            right.push(
                match editor.state.doc.line_ending() {
                    editor_text::LineEnding::Lf => "LF",
                    editor_text::LineEnding::CrLf => "CRLF",
                }
                .into(),
            );
            right.push(if editor.state.hard_tabs {
                "Tabs".into()
            } else {
                format!("Spaces: {}", editor.state.tab_width).into()
            });
        }
        div()
            .h(px(STATUS_BAR_HEIGHT))
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .px(px(12.))
            .border_t_1()
            .border_color(palette.border)
            .bg(palette.tab_bar_bg)
            .text_size(px(11.5))
            .text_color(palette.text_secondary)
            .child(div().flex().gap(px(14.)).children(left))
            .child(div().flex().gap(px(14.)).children(right))
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
                    .text_size(px(13.))
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::trace_first_frame();
        let palette = theme::palette(cx).clone();
        let show_sidebar = self.sidebar_visible && self.root.is_some();
        let content: AnyElement = match self.active_tab() {
            Some(tab) => match (&tab.preview, tab.show_preview) {
                (Some(preview), true) => preview.clone().into_any_element(),
                _ => tab.editor.clone().into_any_element(),
            },
            None => self.render_empty(cx).into_any_element(),
        };
        let finder = self.finder.clone().filter(|_| self.finder_open);
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
            .on_action(cx.listener(|this, _: &ToggleFileFinder, window, cx| {
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
                    .child(self.render_tab_bar(cx))
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
    }
}

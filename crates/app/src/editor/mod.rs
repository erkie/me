//! The code editor view: owns an [`EditorState`], handles keyboard, mouse
//! and IME input, loads and saves the file, and keeps the syntax tree in
//! sync in the background. Painting lives in [`element`].

pub mod element;

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use gpui::{
    App, Bounds, ClipboardItem, Context, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyContext, ParentElement, Pixels, Point, Render, SharedString, Styled, Task,
    UTF16Selection, Window, div, px,
};
use me_syntax::{DEFAULT_MAX_FILE_SIZE, Syntax, language_for_path};
use me_text::{
    ClipboardText, Document, EditKind, EditorState, Matcher, SearchQuery, Selection, SelectionSet,
};
use serde::{Deserialize, Serialize};

use crate::actions::*;
use element::{EditorElement, LayoutSnapshot};

/// Files above this size are read on a background thread.
const SYNC_LOAD_LIMIT: u64 = 2 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditorEvent {
    Changed,
    DirtyChanged,
    Saved,
    Loaded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Autoscroll {
    /// Scroll just enough to show the primary cursor.
    Fit,
    /// Put the primary cursor in the middle of the screen.
    Center,
}

/// Scroll offset. Vertical is f64: in a multi-million-line file the offset
/// is tens of millions of pixels, beyond what f32 represents exactly.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScrollPosition {
    pub x: Pixels,
    pub y: f64,
}

#[derive(Clone, Debug)]
enum Drag {
    /// Plain or shift/cmd-click drag. `origin` is the range the first click
    /// selected (a char, word or line depending on click count).
    Select {
        origin: Range<usize>,
        granularity: Granularity,
        others: Vec<Selection>,
    },
    /// Option-drag column selection from a (line, visual column) corner.
    Column {
        anchor: (usize, usize),
        others: Vec<Selection>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Granularity {
    Char,
    Word,
    Line,
}

#[derive(Serialize, Deserialize)]
struct ClipboardMeta {
    per_selection: Vec<String>,
    whole_lines: bool,
}

pub struct EditorView {
    focus_handle: FocusHandle,
    pub state: EditorState,
    path: Option<PathBuf>,
    syntax: Option<Syntax>,
    parse_task: Option<Task<()>>,
    load_task: Option<Task<()>>,
    pub(crate) scroll: ScrollPosition,
    pub(crate) autoscroll: Option<Autoscroll>,
    pub(crate) layout: Option<LayoutSnapshot>,
    drag: Option<Drag>,
    marked: Option<Vec<Range<usize>>>,
    pub(crate) cursor_visible: bool,
    blink_epoch: usize,
    blink_task: Option<Task<()>>,
    pub(crate) scrolled_at: Option<std::time::Instant>,
    pub(crate) search: Option<Arc<Matcher>>,
    pub(crate) active_match: Option<Range<usize>>,
    read_only: bool,
    pub(crate) notice: Option<SharedString>,
    was_dirty: bool,
    loading: bool,
    disk_mtime: Option<SystemTime>,
    _subscriptions: Vec<gpui::Subscription>,
}

impl EventEmitter<EditorEvent> for EditorView {}

impl Focusable for EditorView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EditorView {
    pub fn empty(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        let subscriptions = vec![
            cx.on_focus(&focus_handle, window, |this, _, cx| this.restart_blink(cx)),
            cx.on_blur(&focus_handle, window, |this, _, cx| {
                this.blink_task = None;
                this.cursor_visible = false;
                this.drag = None;
                cx.notify();
            }),
        ];
        Self {
            focus_handle,
            state: EditorState::new(Document::new()),
            path: None,
            syntax: None,
            parse_task: None,
            load_task: None,
            scroll: ScrollPosition::default(),
            autoscroll: None,
            layout: None,
            drag: None,
            marked: None,
            cursor_visible: true,
            blink_epoch: 0,
            blink_task: None,
            scrolled_at: None,
            search: None,
            active_match: None,
            read_only: false,
            notice: None,
            was_dirty: false,
            loading: false,
            disk_mtime: None,
            _subscriptions: subscriptions,
        }
    }

    pub fn with_text(text: &str, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self::empty(window, cx);
        this.state = EditorState::from_str(text);
        this
    }

    /// Opens `path`. Small files load synchronously so the first frame
    /// already shows them; big ones load in the background.
    pub fn open(path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self::empty(window, cx);
        this.path = Some(path.clone());
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if size <= SYNC_LOAD_LIMIT {
            let loaded = load_file(&path);
            this.finish_load(loaded, cx);
        } else {
            this.loading = true;
            this.notice = Some(format!("Loading {}…", human_size(size)).into());
            let task_path = path.clone();
            this.load_task = Some(cx.spawn(async move |this, cx| {
                let loaded = cx
                    .background_executor()
                    .spawn(async move { load_file(&task_path) })
                    .await;
                this.update(cx, |this, cx| {
                    this.finish_load(loaded, cx);
                    cx.emit(EditorEvent::Loaded);
                    cx.notify();
                })
                .ok();
            }));
        }
        this
    }

    fn finish_load(&mut self, loaded: Loaded, cx: &mut Context<Self>) {
        crate::trace_phase("document loaded");
        self.loading = false;
        self.notice = loaded.notice.map(SharedString::from);
        self.read_only = loaded.read_only;
        self.disk_mtime = loaded.mtime;
        self.state = EditorState::new(loaded.doc);
        self.scroll = ScrollPosition::default();
        if let Some(path) = &self.path
            && self.state.doc.len_bytes() <= DEFAULT_MAX_FILE_SIZE
            && let Some(language) = language_for_path(path).and_then(|def| def.load())
        {
            let mut syntax = Syntax::new(language);
            // Parse small files right away so the first frame is colored.
            if self.state.doc.len_bytes() < 256 * 1024 {
                syntax.parse(self.state.doc.rope(), Duration::from_millis(50));
                self.syntax = Some(syntax);
            } else {
                self.syntax = Some(syntax);
                self.schedule_parse(cx);
            }
        }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn title(&self) -> SharedString {
        match &self.path {
            Some(path) => path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string())
                .into(),
            None => "Untitled".into(),
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.state.doc.is_dirty()
    }

    pub fn is_loading(&self) -> bool {
        self.loading
    }

    pub fn language_name(&self) -> Option<&'static str> {
        self.syntax.as_ref().map(|s| s.language().def.name)
    }

    pub(crate) fn syntax(&self) -> Option<&Syntax> {
        self.syntax.as_ref()
    }

    /// Moves the cursor to a 1-based line and column and centers it.
    pub fn go_to(&mut self, line: u32, column: Option<u32>, cx: &mut Context<Self>) {
        let line = (line.max(1) - 1) as usize;
        let col = column.map(|c| c.max(1) - 1).unwrap_or(0) as usize;
        let pos = self.state.doc.pos_at(line, col);
        self.state.set_cursor(pos);
        self.autoscroll = Some(Autoscroll::Center);
        cx.notify();
    }

    pub fn set_search(&mut self, query: Option<&SearchQuery>, cx: &mut Context<Self>) {
        self.search = query.and_then(|q| q.compile().ok()).map(Arc::new);
        cx.notify();
    }

    pub fn find(&mut self, query: &SearchQuery, backwards: bool, cx: &mut Context<Self>) -> bool {
        let found = self.state.find(query, backwards);
        self.active_match = found.clone();
        self.autoscroll = Some(Autoscroll::Center);
        cx.notify();
        found.is_some()
    }

    pub fn select_all_matches(&mut self, query: &SearchQuery, cx: &mut Context<Self>) {
        if self.state.select_all_matches(query) {
            self.did_move(cx);
        }
    }

    pub fn replace_next(&mut self, query: &SearchQuery, replacement: &str, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
        self.state.replace_next(query, replacement);
        self.active_match = Some(self.state.selections().primary().range());
        self.did_change(cx);
    }

    pub fn replace_all(
        &mut self,
        query: &SearchQuery,
        replacement: &str,
        cx: &mut Context<Self>,
    ) -> usize {
        if self.read_only {
            return 0;
        }
        let n = self.state.replace_all(query, replacement);
        self.did_change(cx);
        n
    }

    /// The primary selection's text, if it is on one line.
    pub fn selected_text(&self) -> Option<String> {
        let sel = self.state.selections().primary();
        if sel.is_empty() {
            let r = self.state.word_range_at(sel.head);
            return (!r.is_empty()).then(|| self.state.doc.text(r));
        }
        let text = self.state.doc.text(sel.range());
        (!text.contains('\n')).then_some(text)
    }

    pub fn text(&self) -> String {
        self.state.text()
    }

    pub fn revision(&self) -> u64 {
        self.state.doc.revision()
    }

    // ---------------------------------------------------------------------
    // Change plumbing
    // ---------------------------------------------------------------------

    fn did_change(&mut self, cx: &mut Context<Self>) {
        let edits = self.state.doc.take_edits();
        if !edits.is_empty() {
            if let Some(syntax) = &mut self.syntax {
                syntax.edit(&edits);
            }
            self.schedule_parse(cx);
            cx.emit(EditorEvent::Changed);
        }
        let dirty = self.state.doc.is_dirty();
        if dirty != self.was_dirty {
            self.was_dirty = dirty;
            cx.emit(EditorEvent::DirtyChanged);
        }
        self.marked = None;
        self.did_move(cx);
    }

    fn did_move(&mut self, cx: &mut Context<Self>) {
        self.autoscroll = Some(Autoscroll::Fit);
        self.restart_blink(cx);
        cx.notify();
    }

    fn schedule_parse(&mut self, cx: &mut Context<Self>) {
        let Some(syntax) = &self.syntax else { return };
        let language = syntax.language().clone();
        let old_tree = syntax.tree().cloned();
        let rope = self.state.doc.rope().clone();
        let revision = self.state.doc.revision();
        self.parse_task = Some(cx.spawn(async move |this, cx| {
            let tree = cx
                .background_executor()
                .spawn(async move {
                    me_syntax::parse(&language, &rope, old_tree.as_ref(), Duration::from_secs(3))
                })
                .await;
            this.update(cx, |this, cx| {
                if this.state.doc.revision() == revision
                    && let (Some(tree), Some(syntax)) = (tree, &mut this.syntax)
                {
                    syntax.set_tree(tree);
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    fn restart_blink(&mut self, cx: &mut Context<Self>) {
        self.cursor_visible = true;
        self.blink_epoch += 1;
        let epoch = self.blink_epoch;
        self.blink_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(530))
                    .await;
                let keep_going = this
                    .update(cx, |this, cx| {
                        if this.blink_epoch != epoch {
                            return false;
                        }
                        this.cursor_visible = !this.cursor_visible;
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !keep_going {
                    break;
                }
            }
        }));
    }

    fn motion(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut EditorState)) {
        f(&mut self.state);
        self.active_match = None;
        self.did_move(cx);
    }

    fn edit(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut EditorState)) {
        if self.read_only || self.loading {
            return;
        }
        f(&mut self.state);
        self.active_match = None;
        self.did_change(cx);
    }

    fn page_lines(&self) -> isize {
        self.layout
            .as_ref()
            .map(|l| ((l.bounds.size.height / l.line_height) as isize - 2).max(1))
            .unwrap_or(30)
    }

    fn page(&mut self, down: bool, extend: bool, cx: &mut Context<Self>) {
        let lines = self.page_lines();
        let delta = if down { lines } else { -lines };
        if let Some(layout) = &self.layout {
            let line_height = f32::from(layout.line_height) as f64;
            self.scroll.y = (self.scroll.y + line_height * delta as f64).max(0.);
        }
        self.motion(cx, |s| s.move_vertically(delta, extend));
    }

    // ---------------------------------------------------------------------
    // Clipboard
    // ---------------------------------------------------------------------

    fn copy(&mut self, cx: &mut Context<Self>) -> ClipboardText {
        let clip = self.state.copy();
        let meta = ClipboardMeta {
            per_selection: clip.per_selection.clone(),
            whole_lines: clip.whole_lines,
        };
        cx.write_to_clipboard(ClipboardItem::new_string_with_json_metadata(
            clip.text.clone(),
            meta,
        ));
        clip
    }

    fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        let Some(text) = item.text() else { return };
        let meta: Option<ClipboardMeta> = item.entries().iter().find_map(|e| match e {
            gpui::ClipboardEntry::String(s) => s.metadata_json(),
            _ => None,
        });
        let clip = match meta {
            Some(meta)
                if meta.per_selection.concat().len() + meta.per_selection.len() >= text.len() =>
            {
                ClipboardText {
                    text,
                    per_selection: meta.per_selection,
                    whole_lines: meta.whole_lines,
                }
            }
            _ => ClipboardText {
                text: text.clone(),
                per_selection: vec![text],
                whole_lines: false,
            },
        };
        self.edit(cx, |s| s.paste(&clip));
    }

    // ---------------------------------------------------------------------
    // Saving
    // ---------------------------------------------------------------------

    pub fn save(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<()>> {
        match self.path.clone() {
            Some(path) => {
                let result = self.save_to(&path, cx);
                Task::ready(result)
            }
            None => self.save_as(window, cx),
        }
    }

    pub fn save_as(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<()>> {
        let dir = self
            .path
            .as_ref()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/"));
        let suggested = self
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned());
        let rx = cx.prompt_for_new_path(&dir, suggested.as_deref());
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = rx.await else {
                return Ok(());
            };
            this.update(cx, |this, cx| {
                this.save_to(&path, cx)?;
                this.path = Some(path.clone());
                if this.syntax.is_none()
                    && let Some(language) = language_for_path(&path).and_then(|d| d.load())
                {
                    this.syntax = Some(Syntax::new(language));
                    this.schedule_parse(cx);
                }
                cx.notify();
                anyhow::Ok(())
            })?
        })
    }

    fn save_to(&mut self, path: &Path, cx: &mut Context<Self>) -> anyhow::Result<()> {
        let mut bytes = Vec::with_capacity(self.state.doc.len_bytes());
        self.state.doc.write_to(&mut bytes)?;
        write_atomically(path, &bytes)?;
        self.disk_mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        self.state.doc.mark_saved();
        self.was_dirty = false;
        cx.emit(EditorEvent::DirtyChanged);
        cx.emit(EditorEvent::Saved);
        cx.notify();
        Ok(())
    }

    /// Reloads from disk if the file changed there and has no unsaved edits.
    pub fn reload_if_changed(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        if mtime.is_none() || mtime == self.disk_mtime || self.is_dirty() {
            return;
        }
        let loaded = load_file(&path);
        self.disk_mtime = loaded.mtime;
        let text = loaded.doc.rope().to_string();
        let mut sels = self.state.selections().clone();
        self.state.doc.replace_all(&text, &mut sels);
        self.state.set_selections(sels);
        self.state.doc.mark_saved();
        self.was_dirty = false;
        let edits = self.state.doc.take_edits();
        if let Some(syntax) = &mut self.syntax {
            syntax.edit(&edits);
        }
        self.schedule_parse(cx);
        cx.emit(EditorEvent::Changed);
        cx.notify();
    }

    // ---------------------------------------------------------------------
    // Mouse
    // ---------------------------------------------------------------------

    pub(crate) fn mouse_down(
        &mut self,
        position: Point<Pixels>,
        click_count: usize,
        modifiers: gpui::Modifiers,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        let Some(layout) = &self.layout else { return };
        let hit = layout.hit(position, &self.state);
        // Option-drag selects a column; Cmd-click (Ctrl-click elsewhere) adds
        // a cursor, and combined with Option adds a column.
        let add = modifiers.secondary();
        if modifiers.alt {
            let others = if add {
                self.state.selections().iter().copied().collect()
            } else {
                Vec::new()
            };
            self.drag = Some(Drag::Column {
                anchor: (hit.line, hit.visual_col),
                others: others.clone(),
            });
            self.apply_column(
                others,
                (hit.line, hit.visual_col),
                (hit.line, hit.visual_col),
            );
            self.did_move(cx);
            return;
        }

        let granularity = match click_count {
            2 => Granularity::Word,
            n if n >= 3 => Granularity::Line,
            _ => Granularity::Char,
        };
        let origin = match granularity {
            Granularity::Char => hit.pos..hit.pos,
            Granularity::Word => self.state.word_range_at(hit.pos),
            Granularity::Line => self.state.line_range_at(hit.pos),
        };

        if modifiers.shift && granularity == Granularity::Char {
            let primary = self.state.selections().primary();
            let mut sels: Vec<Selection> = self.state.selections().iter().copied().collect();
            let ix = self.state.selections().primary_index();
            sels[ix] = primary.extend_to(hit.pos);
            self.state.set_selections(SelectionSet::new(sels, ix));
            self.drag = Some(Drag::Select {
                origin: primary.anchor..primary.anchor,
                granularity,
                others: Vec::new(),
            });
            self.did_move(cx);
            return;
        }

        let others = if add {
            if click_count == 1 {
                self.state.toggle_cursor(hit.pos);
                self.did_move(cx);
                let others = self
                    .state
                    .selections()
                    .iter()
                    .copied()
                    .filter(|s| s.head != hit.pos)
                    .collect();
                self.drag = Some(Drag::Select {
                    origin,
                    granularity,
                    others,
                });
                return;
            }
            self.state.selections().iter().copied().collect()
        } else {
            Vec::new()
        };
        self.drag = Some(Drag::Select {
            origin: origin.clone(),
            granularity,
            others: others.clone(),
        });
        self.apply_drag_select(others, origin.clone(), origin, granularity);
        self.did_move(cx);
    }

    pub(crate) fn mouse_drag(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let (Some(drag), Some(layout)) = (self.drag.clone(), &self.layout) else {
            return;
        };
        let hit = layout.hit(position, &self.state);
        match drag {
            Drag::Select {
                origin,
                granularity,
                others,
            } => {
                let target = match granularity {
                    Granularity::Char => hit.pos..hit.pos,
                    Granularity::Word => self.state.word_range_at(hit.pos),
                    Granularity::Line => self.state.line_range_at(hit.pos),
                };
                self.apply_drag_select(others, origin, target, granularity);
            }
            Drag::Column { anchor, others } => {
                self.apply_column(others, anchor, (hit.line, hit.visual_col));
            }
        }
        self.did_move(cx);
    }

    pub(crate) fn mouse_up(&mut self, _cx: &mut Context<Self>) {
        self.drag = None;
    }

    pub(crate) fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    fn apply_drag_select(
        &mut self,
        others: Vec<Selection>,
        origin: Range<usize>,
        target: Range<usize>,
        _g: Granularity,
    ) {
        let sel = if target.start < origin.start {
            Selection::new(origin.end, target.start)
        } else {
            Selection::new(origin.start, target.end.max(origin.end))
        };
        let mut sels = others;
        sels.push(sel);
        let primary = sels.len() - 1;
        self.state.set_selections(SelectionSet::new(sels, primary));
    }

    fn apply_column(
        &mut self,
        others: Vec<Selection>,
        anchor: (usize, usize),
        head: (usize, usize),
    ) {
        self.state.select_column(anchor, head);
        if !others.is_empty() {
            let mut sels = others;
            let column: Vec<Selection> = self.state.selections().iter().copied().collect();
            sels.extend(column);
            let primary = sels.len() - 1;
            self.state.set_selections(SelectionSet::new(sels, primary));
        }
    }

    pub(crate) fn scroll_by(&mut self, delta: Point<Pixels>, cx: &mut Context<Self>) {
        self.scroll.x = (self.scroll.x - delta.x).max(px(0.));
        self.scroll.y = (self.scroll.y - f32::from(delta.y) as f64).max(0.);
        self.autoscroll = None;
        cx.notify();
    }

    // ---------------------------------------------------------------------
    // IME helpers (UTF-16 offsets <-> chars)
    // ---------------------------------------------------------------------

    fn utf16_to_char(&self, offset: usize) -> usize {
        let rope = self.state.doc.rope();
        rope.utf16_cu_to_char(offset.min(rope.len_utf16_cu()))
    }

    fn char_to_utf16(&self, offset: usize) -> usize {
        let rope = self.state.doc.rope();
        rope.char_to_utf16_cu(offset.min(rope.len_chars()))
    }

    /// Replaces either the marked text or every selection with `text`, and
    /// returns the inserted ranges.
    fn ime_replace(&mut self, range_utf16: Option<Range<usize>>, text: &str) -> Vec<Range<usize>> {
        let ranges: Vec<Range<usize>> = if let Some(marked) = self.marked.take() {
            marked
        } else if let Some(r) = range_utf16 {
            // An explicit range from the IME applies relative to the primary
            // selection; map it to every cursor by offset.
            let start = self.utf16_to_char(r.start);
            let end = self.utf16_to_char(r.end);
            let primary = self.state.selections().primary();
            let (before, after) = (
                primary.start().saturating_sub(start),
                end.saturating_sub(primary.end()),
            );
            self.state
                .selections()
                .iter()
                .map(|s| {
                    s.start().saturating_sub(before)
                        ..(s.end() + after).min(self.state.doc.len_chars())
                })
                .collect()
        } else {
            self.state.selections().iter().map(|s| s.range()).collect()
        };
        self.state.replace_ranges(&ranges, text, EditKind::Insert)
    }
}

impl gpui::EntityInputHandler for EditorView {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let start = self.utf16_to_char(range_utf16.start);
        let end = self.utf16_to_char(range_utf16.end);
        adjusted_range.replace(self.char_to_utf16(start)..self.char_to_utf16(end));
        Some(self.state.doc.text(start..end))
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let sel = self.state.selections().primary();
        Some(UTF16Selection {
            range: self.char_to_utf16(sel.start())..self.char_to_utf16(sel.end()),
            reversed: sel.is_reversed(),
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        let marked = self.marked.as_ref()?;
        let primary = self
            .state
            .selections()
            .primary_index()
            .min(marked.len().saturating_sub(1));
        let r = marked.get(primary)?;
        Some(self.char_to_utf16(r.start)..self.char_to_utf16(r.end))
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only || self.loading {
            return;
        }
        self.ime_replace(range_utf16, text);
        self.marked = None;
        self.active_match = None;
        self.did_change(cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only || self.loading {
            return;
        }
        let inserted = self.ime_replace(range_utf16, text);
        // Place each cursor inside its marked text as the IME asks.
        if let Some(sel) = new_selected_range_utf16 {
            let sels: Vec<Selection> = inserted
                .iter()
                .map(|r| {
                    let base = self.char_to_utf16(r.start);
                    let a = self.utf16_to_char(base + sel.start).min(r.end);
                    let b = self.utf16_to_char(base + sel.end).min(r.end);
                    Selection::new(a, b)
                })
                .collect();
            if !sels.is_empty() {
                let primary = self.state.selections().primary_index().min(sels.len() - 1);
                self.state.set_selections(SelectionSet::new(sels, primary));
            }
        }
        let edits = self.state.doc.take_edits();
        if let Some(syntax) = &mut self.syntax {
            syntax.edit(&edits);
        }
        self.schedule_parse(cx);
        self.marked = if text.is_empty() {
            None
        } else {
            Some(inserted)
        };
        self.did_move(cx);
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let layout = self.layout.as_ref()?;
        let start = self.utf16_to_char(range_utf16.start);
        let origin = layout.point_for_pos(start, &self.state)?;
        Some(Bounds::new(
            origin,
            gpui::size(layout.char_width, layout.line_height),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let layout = self.layout.as_ref()?;
        let hit = layout.hit(point, &self.state);
        Some(self.char_to_utf16(hit.pos))
    }
}

impl Render for EditorView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut context = KeyContext::new_with_defaults();
        context.add("Editor");
        if self.state.selections().len() > 1 {
            context.add("multi_cursor");
        }
        div()
            .id("editor")
            .key_context(context)
            .track_focus(&self.focus_handle)
            .size_full()
            .overflow_hidden()
            .cursor(gpui::CursorStyle::IBeam)
            // Movement
            .on_action(
                cx.listener(|this, _: &MoveLeft, _, cx| this.motion(cx, |s| s.move_left(false))),
            )
            .on_action(
                cx.listener(|this, _: &MoveRight, _, cx| this.motion(cx, |s| s.move_right(false))),
            )
            .on_action(cx.listener(|this, _: &MoveUp, _, cx| this.motion(cx, |s| s.move_up(false))))
            .on_action(
                cx.listener(|this, _: &MoveDown, _, cx| this.motion(cx, |s| s.move_down(false))),
            )
            .on_action(cx.listener(|this, _: &MoveWordLeft, _, cx| {
                this.motion(cx, |s| s.move_word_left(false))
            }))
            .on_action(cx.listener(|this, _: &MoveWordRight, _, cx| {
                this.motion(cx, |s| s.move_word_right(false))
            }))
            .on_action(cx.listener(|this, _: &MoveLineStart, _, cx| {
                this.motion(cx, |s| s.move_line_start(false))
            }))
            .on_action(cx.listener(|this, _: &MoveLineEnd, _, cx| {
                this.motion(cx, |s| s.move_line_end(false))
            }))
            .on_action(cx.listener(|this, _: &MoveDocStart, _, cx| {
                this.motion(cx, |s| s.move_doc_start(false))
            }))
            .on_action(
                cx.listener(|this, _: &MoveDocEnd, _, cx| {
                    this.motion(cx, |s| s.move_doc_end(false))
                }),
            )
            .on_action(cx.listener(|this, _: &PageUp, _, cx| this.page(false, false, cx)))
            .on_action(cx.listener(|this, _: &PageDown, _, cx| this.page(true, false, cx)))
            .on_action(
                cx.listener(|this, _: &SelectLeft, _, cx| this.motion(cx, |s| s.move_left(true))),
            )
            .on_action(
                cx.listener(|this, _: &SelectRight, _, cx| this.motion(cx, |s| s.move_right(true))),
            )
            .on_action(
                cx.listener(|this, _: &SelectUp, _, cx| this.motion(cx, |s| s.move_up(true))),
            )
            .on_action(
                cx.listener(|this, _: &SelectDown, _, cx| this.motion(cx, |s| s.move_down(true))),
            )
            .on_action(cx.listener(|this, _: &SelectWordLeft, _, cx| {
                this.motion(cx, |s| s.move_word_left(true))
            }))
            .on_action(cx.listener(|this, _: &SelectWordRight, _, cx| {
                this.motion(cx, |s| s.move_word_right(true))
            }))
            .on_action(cx.listener(|this, _: &SelectLineStart, _, cx| {
                this.motion(cx, |s| s.move_line_start(true))
            }))
            .on_action(cx.listener(|this, _: &SelectLineEnd, _, cx| {
                this.motion(cx, |s| s.move_line_end(true))
            }))
            .on_action(cx.listener(|this, _: &SelectDocStart, _, cx| {
                this.motion(cx, |s| s.move_doc_start(true))
            }))
            .on_action(cx.listener(|this, _: &SelectDocEnd, _, cx| {
                this.motion(cx, |s| s.move_doc_end(true))
            }))
            .on_action(cx.listener(|this, _: &SelectPageUp, _, cx| this.page(false, true, cx)))
            .on_action(cx.listener(|this, _: &SelectPageDown, _, cx| this.page(true, true, cx)))
            // Editing
            .on_action(cx.listener(|this, _: &Backspace, _, cx| this.edit(cx, |s| s.backspace())))
            .on_action(cx.listener(|this, _: &Delete, _, cx| this.edit(cx, |s| s.delete_forward())))
            .on_action(cx.listener(|this, _: &DeleteWordLeft, _, cx| {
                this.edit(cx, |s| s.delete_word_left())
            }))
            .on_action(cx.listener(|this, _: &DeleteWordRight, _, cx| {
                this.edit(cx, |s| s.delete_word_right())
            }))
            .on_action(cx.listener(|this, _: &DeleteToLineStart, _, cx| {
                this.edit(cx, |s| s.delete_to_line_start())
            }))
            .on_action(cx.listener(|this, _: &DeleteToLineEnd, _, cx| {
                this.edit(cx, |s| s.delete_to_line_end())
            }))
            .on_action(cx.listener(|this, _: &Newline, _, cx| this.edit(cx, |s| s.newline())))
            .on_action(cx.listener(|this, _: &Tab, _, cx| this.edit(cx, |s| s.tab())))
            .on_action(cx.listener(|this, _: &Outdent, _, cx| this.edit(cx, |s| s.outdent_lines())))
            .on_action(
                cx.listener(|this, _: &IndentLines, _, cx| this.edit(cx, |s| s.indent_lines())),
            )
            .on_action(
                cx.listener(|this, _: &OutdentLines, _, cx| this.edit(cx, |s| s.outdent_lines())),
            )
            .on_action(cx.listener(|this, _: &ToggleComment, _, cx| {
                let token = this
                    .syntax
                    .as_ref()
                    .and_then(|s| s.language().def.line_comment)
                    .unwrap_or("#");
                this.edit(cx, |s| s.toggle_line_comment(token))
            }))
            .on_action(
                cx.listener(|this, _: &DuplicateLine, _, cx| this.edit(cx, |s| s.duplicate())),
            )
            .on_action(
                cx.listener(|this, _: &MoveLineUp, _, cx| this.edit(cx, |s| s.move_lines(false))),
            )
            .on_action(
                cx.listener(|this, _: &MoveLineDown, _, cx| this.edit(cx, |s| s.move_lines(true))),
            )
            .on_action(
                cx.listener(|this, _: &DeleteLine, _, cx| this.edit(cx, |s| s.delete_lines())),
            )
            .on_action(cx.listener(|this, _: &Undo, _, cx| {
                this.edit(cx, |s| {
                    s.undo();
                })
            }))
            .on_action(cx.listener(|this, _: &Redo, _, cx| {
                this.edit(cx, |s| {
                    s.redo();
                })
            }))
            .on_action(cx.listener(|this, _: &Copy, _, cx| {
                this.copy(cx);
            }))
            .on_action(cx.listener(|this, _: &Cut, _, cx| {
                if this.read_only {
                    this.copy(cx);
                    return;
                }
                let clip = this.state.cut();
                let meta = ClipboardMeta {
                    per_selection: clip.per_selection.clone(),
                    whole_lines: clip.whole_lines,
                };
                cx.write_to_clipboard(ClipboardItem::new_string_with_json_metadata(
                    clip.text, meta,
                ));
                this.did_change(cx);
            }))
            .on_action(cx.listener(|this, _: &Paste, _, cx| this.paste(cx)))
            .on_action(
                cx.listener(|this, _: &SelectAll, _, cx| this.motion(cx, |s| s.select_all())),
            )
            // Multiple selections
            .on_action(
                cx.listener(|this, _: &SelectLine, _, cx| this.motion(cx, |s| s.select_line())),
            )
            .on_action(cx.listener(|this, _: &SplitIntoLines, _, cx| {
                this.motion(cx, |s| s.split_into_lines())
            }))
            .on_action(cx.listener(|this, _: &AddNextOccurrence, _, cx| {
                this.motion(cx, |s| {
                    s.add_next_occurrence();
                })
            }))
            .on_action(cx.listener(|this, _: &SkipOccurrence, _, cx| {
                this.motion(cx, |s| {
                    s.skip_occurrence();
                })
            }))
            .on_action(cx.listener(|this, _: &SelectAllOccurrences, _, cx| {
                this.motion(cx, |s| {
                    s.select_all_occurrences();
                })
            }))
            .on_action(cx.listener(|this, _: &AddCursorAbove, _, cx| {
                this.motion(cx, |s| {
                    s.add_cursor_vertically(false);
                })
            }))
            .on_action(cx.listener(|this, _: &AddCursorBelow, _, cx| {
                this.motion(cx, |s| {
                    s.add_cursor_vertically(true);
                })
            }))
            .on_action(cx.listener(|this, _: &Cancel, _, cx| {
                if this.state.selections().len() > 1
                    || !this.state.selections().primary().is_empty()
                {
                    this.motion(cx, |s| s.collapse_selections());
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &Save, window, cx| {
                this.save(window, cx).detach();
            }))
            .on_action(cx.listener(|this, _: &SaveAs, window, cx| {
                this.save_as(window, cx).detach();
            }))
            .child(EditorElement::new(cx.entity()))
    }
}

struct Loaded {
    doc: Document,
    read_only: bool,
    notice: Option<String>,
    mtime: Option<SystemTime>,
}

fn load_file(path: &Path) -> Loaded {
    let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            // A new file: opens empty and is created on save.
            return Loaded {
                doc: Document::new(),
                read_only: false,
                notice: None,
                mtime: None,
            };
        }
        Err(err) => {
            return Loaded {
                doc: Document::new(),
                read_only: true,
                notice: Some(format!("Could not open: {err}")),
                mtime,
            };
        }
    };
    if bytes[..bytes.len().min(8192)].contains(&0) {
        return Loaded {
            doc: Document::new(),
            read_only: true,
            notice: Some(format!("Binary file ({})", human_size(bytes.len() as u64))),
            mtime,
        };
    }
    match String::from_utf8(bytes) {
        Ok(text) => Loaded {
            doc: Document::from_str(&text),
            read_only: false,
            notice: None,
            mtime,
        },
        Err(err) => {
            let text = String::from_utf8_lossy(err.as_bytes()).into_owned();
            Loaded {
                doc: Document::from_str(&text),
                read_only: true,
                notice: Some("Not valid UTF-8: opened read-only".into()),
                mtime,
            }
        }
    }
}

fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let permissions = std::fs::metadata(path).map(|m| m.permissions()).ok();
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    if let Some(permissions) = permissions {
        std::fs::set_permissions(tmp.path(), permissions)?;
    }
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

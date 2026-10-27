//! CSV and TSV files as a table. The source lives in an [`EditorView`] so
//! flipping to source (Cmd+Shift+V) shows the same, live text.
//!
//! Like the editor, only the visible cells are shaped and painted, and
//! records are indexed in the background, so a file of millions of rows
//! scrolls as smoothly as a small one.

mod parse;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use encoding_rs::Encoding;
use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, DispatchPhase, Element, ElementId, Entity,
    FocusHandle, Focusable, FontWeight, GlobalElementId, Hitbox, HitboxBehavior,
    InspectorElementId, InteractiveElement, IntoElement, LayoutId, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, PaintQuad, ParentElement, Pixels, Point, Render,
    ScrollWheelEvent, ShapedLine, SharedString, Style, Styled, Subscription, Task, TextAlign,
    TextRun, Window, div, fill, font, point, px, relative, size,
};
use me_text::Rope;
use unicode_width::UnicodeWidthChar;

use crate::actions::{
    Copy, CopyPath, CopyRelativePath, MoveDocEnd, MoveDocStart, MoveDown, MoveLeft, MoveLineEnd,
    MoveLineStart, MoveRight, MoveUp, Outdent, PageDown, PageUp, RevealInFinder, Tab,
    TogglePreview,
};
use crate::editor::{EditorEvent, EditorView};
use crate::{settings, theme};
pub use parse::is_table;
use parse::{Dialect, Index};

/// Re-index at most this often while the source changes.
const REFRESH_DELAY: Duration = Duration::from_millis(120);
/// Indexed before the first frame; the rest is indexed in the background.
const FIRST_INDEX_BYTES: usize = 512 * 1024;
/// Column widths, in characters, before the user drags them.
const MIN_COLUMN_CHARS: usize = 3;
const MAX_COLUMN_CHARS: usize = 40;
const CELL_PAD: f32 = 8.;
/// How close to a header edge a drag resizes the column.
const RESIZE_GRAB: f32 = 4.;
const SCROLLBAR_VISIBLE_FOR: Duration = Duration::from_millis(1200);

/// A choice from the table's menus. `None` means "detect it".
#[derive(Clone, Debug, PartialEq)]
pub enum TableOption {
    Delimiter(Option<u8>),
    Quote(Option<Option<u8>>),
    Header(Option<bool>),
    Encoding(Option<&'static str>),
}

/// Change how the active table is read (from its menus).
#[derive(Clone, Debug, PartialEq, gpui::Action)]
#[action(namespace = table, no_json)]
pub struct SetTableOption {
    pub option: TableOption,
}

gpui::actions!(table, [CopyRow]);

/// The encodings offered in the menu, by their WHATWG names.
const ENCODINGS: &[&str] = &[
    "UTF-8",
    "UTF-16LE",
    "UTF-16BE",
    "windows-1252",
    "ISO-8859-2",
    "windows-1251",
    "KOI8-R",
    "Shift_JIS",
    "EUC-KR",
    "gbk",
    "Big5",
];

/// What the user forced; anything left `None` is detected.
#[derive(Clone, Default)]
struct Overrides {
    delimiter: Option<u8>,
    quote: Option<Option<u8>>,
    header: Option<bool>,
    encoding: Option<&'static Encoding>,
}

/// The text the table is read from.
struct Source {
    rope: Rope,
    encoding: &'static Encoding,
    /// The editor's own text (live), or a copy decoded from disk because the
    /// file isn't UTF-8.
    from_editor: bool,
    revision: u64,
}

/// Per column: auto width (in characters), whether it holds numbers.
#[derive(Default)]
struct Columns {
    chars: Vec<usize>,
    numeric: Vec<bool>,
}

/// The geometry of the last frame, for clicks and keyboard paging.
#[derive(Clone, Default)]
struct Layout {
    body: Bounds<Pixels>,
    header: Bounds<Pixels>,
    row_height: Pixels,
    /// Left edge of each column (and the end of the last), unscrolled.
    xs: Vec<Pixels>,
}

pub struct CsvPreview {
    focus_handle: FocusHandle,
    editor: Entity<EditorView>,
    /// The separator the extension suggests.
    preferred: u8,
    overrides: Overrides,
    guessed: Option<parse::Guess>,
    source: Option<Source>,
    index: Option<Arc<Index>>,
    columns: Columns,
    /// Bumped whenever the index or dialect changes; keys the row cache.
    generation: u64,
    cache: Option<(u64, usize, Vec<Vec<SharedString>>)>,
    /// The first record, kept apart so the header doesn't evict the rows.
    header: Option<(u64, Vec<SharedString>)>,
    error: Option<SharedString>,
    cancel: Arc<AtomicBool>,
    index_task: Option<Task<()>>,
    load_task: Option<Task<()>>,
    refresh: Option<Task<()>>,
    scroll_x: Pixels,
    /// In f64: a million rows down is past where f32 pixels are exact.
    scroll_y: f64,
    scrolled_at: Option<Instant>,
    /// Data row and column of the selected cell.
    selected: Option<(usize, usize)>,
    widths: Vec<Option<Pixels>>,
    /// Column being resized: its index, the mouse x and its width at the start.
    resizing: Option<(usize, Pixels, Pixels)>,
    layout: Layout,
    _subscription: Subscription,
}

impl Focusable for CsvPreview {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl CsvPreview {
    pub fn new(editor: Entity<EditorView>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&editor, |this, _, event: &EditorEvent, cx| match event {
            EditorEvent::Loaded => this.reload(cx),
            EditorEvent::Changed => this.schedule_refresh(cx),
            _ => {}
        });
        let preferred = editor
            .read(cx)
            .path()
            .and_then(parse::extension_delimiter)
            .unwrap_or(b',');
        let mut this = Self {
            focus_handle: cx.focus_handle(),
            editor,
            preferred,
            overrides: Overrides::default(),
            guessed: None,
            source: None,
            index: None,
            columns: Columns::default(),
            generation: 0,
            cache: None,
            header: None,
            error: None,
            cancel: Arc::new(AtomicBool::new(false)),
            index_task: None,
            load_task: None,
            refresh: None,
            scroll_x: px(0.),
            scroll_y: 0.,
            scrolled_at: None,
            selected: None,
            widths: Vec::new(),
            resizing: None,
            layout: Layout::default(),
            _subscription: subscription,
        };
        this.reload(cx);
        this
    }

    // ---------------------------------------------------------------------
    // Reading
    // ---------------------------------------------------------------------

    fn dialect(&self) -> Dialect {
        let guessed = self.guessed.as_ref().map(|g| g.dialect).unwrap_or_default();
        Dialect {
            delimiter: self.overrides.delimiter.unwrap_or(guessed.delimiter),
            quote: self.overrides.quote.unwrap_or(guessed.quote),
        }
    }

    fn has_header(&self) -> bool {
        self.overrides
            .header
            .unwrap_or_else(|| self.guessed.as_ref().is_some_and(|g| g.header))
    }

    /// Picks the source (the editor's text, or the file decoded from disk),
    /// guesses the dialect and indexes it.
    fn reload(&mut self, cx: &mut Context<Self>) {
        let editor = self.editor.read(cx);
        if editor.is_loading() {
            return;
        }
        let forced = self.overrides.encoding;
        let from_disk = editor.is_blank()
            || editor.is_lossy()
            || forced.is_some_and(|e| e != encoding_rs::UTF_8);
        if !from_disk {
            self.load_task = None;
            self.set_source(
                Source {
                    rope: editor.state.doc.rope().clone(),
                    encoding: encoding_rs::UTF_8,
                    from_editor: true,
                    revision: editor.revision(),
                },
                cx,
            );
            return;
        }
        let Some(path) = editor.path().map(|p| p.to_path_buf()) else {
            return;
        };
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move {
                    let bytes = std::fs::read(&path)?;
                    let encoding = forced.unwrap_or_else(|| parse::sniff_encoding(&bytes));
                    let text = parse::decode(&bytes, encoding);
                    anyhow::Ok((Rope::from_str(&text), encoding))
                })
                .await;
            this.update(cx, |this, cx| {
                this.load_task = None;
                match loaded {
                    Ok((rope, encoding)) => this.set_source(
                        Source {
                            rope,
                            encoding,
                            from_editor: false,
                            revision: 0,
                        },
                        cx,
                    ),
                    Err(err) => {
                        this.error = Some(format!("Could not read the file: {err}").into());
                        cx.notify();
                    }
                }
            })
            .ok();
        }));
    }

    fn set_source(&mut self, source: Source, cx: &mut Context<Self>) {
        self.guessed = Some(parse::guess(&source.rope, self.preferred));
        self.source = Some(source);
        self.error = None;
        self.reindex(true, cx);
    }

    fn schedule_refresh(&mut self, cx: &mut Context<Self>) {
        if self.refresh.is_some() || !self.source.as_ref().is_some_and(|s| s.from_editor) {
            return;
        }
        self.refresh = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(REFRESH_DELAY).await;
            this.update(cx, |this, cx| {
                this.refresh = None;
                let editor = this.editor.read(cx);
                let (rope, revision) = (editor.state.doc.rope().clone(), editor.revision());
                if let Some(source) = this.source.as_mut().filter(|s| s.revision != revision) {
                    source.rope = rope;
                    source.revision = revision;
                    this.reindex(false, cx);
                }
            })
            .ok();
        }));
    }

    /// Indexes the start of the source now and the rest in the background.
    /// `fresh` (a new file or dialect) also re-measures the columns.
    fn reindex(&mut self, fresh: bool, cx: &mut Context<Self>) {
        let Some(source) = &self.source else { return };
        self.cancel.store(true, Ordering::Relaxed);
        self.cancel = Arc::new(AtomicBool::new(false));
        let dialect = self.dialect();
        let rope = source.rope.clone();
        let Some(first) = Index::build(&rope, dialect, Some(FIRST_INDEX_BYTES), &self.cancel)
        else {
            return;
        };
        if fresh || self.columns.chars.len() < first.columns {
            self.measure(&first, &rope, dialect, fresh);
        }
        let complete = first.complete;
        self.index = Some(Arc::new(first));
        self.generation += 1;
        self.cache = None;
        self.clamp_selection();
        cx.notify();
        if complete {
            self.index_task = None;
            return;
        }
        let cancel = self.cancel.clone();
        self.index_task = Some(cx.spawn(async move |this, cx| {
            let task_rope = rope.clone();
            let index = cx
                .background_executor()
                .spawn(async move { Index::build(&task_rope, dialect, None, &cancel) })
                .await;
            let Some(index) = index else { return };
            this.update(cx, |this, cx| {
                this.index_task = None;
                // Widen the columns for what's further down.
                this.measure(&index, &rope, dialect, false);
                this.index = Some(Arc::new(index));
                this.generation += 1;
                this.cache = None;
                this.clamp_selection();
                cx.notify();
            })
            .ok();
        }));
    }

    /// Column widths and alignment, from records at the start, middle and
    /// end. Unless `fresh`, columns only get wider and keep dragged widths.
    fn measure(&mut self, index: &Index, rope: &Rope, dialect: Dialect, fresh: bool) {
        let mut records = index.records(rope, dialect, 0, parse::SAMPLE_RECORDS);
        if index.complete && index.records > parse::SAMPLE_RECORDS * 2 {
            let n = index.records;
            let half = parse::SAMPLE_RECORDS / 2;
            records.extend(index.records(rope, dialect, n / 2, half));
            records.extend(index.records(rope, dialect, n - half, half));
        }
        let columns = index.columns.max(1);
        let header = self.has_header();
        let mut chars = vec![MIN_COLUMN_CHARS; columns];
        let mut numbers = vec![0usize; columns];
        let mut filled = vec![0usize; columns];
        for (i, record) in records.iter().enumerate() {
            for (col, field) in record.iter().enumerate().take(columns) {
                let width = display_width(field).min(MAX_COLUMN_CHARS);
                chars[col] = chars[col].max(width);
                if i == 0 && header || field.trim().is_empty() {
                    continue;
                }
                filled[col] += 1;
                numbers[col] += parse::is_numeric(field) as usize;
            }
        }
        if !fresh {
            for (new, old) in chars.iter_mut().zip(&self.columns.chars) {
                *new = (*new).max(*old);
            }
        }
        self.columns = Columns {
            chars,
            numeric: numbers
                .iter()
                .zip(&filled)
                .map(|(n, f)| *f > 0 && n * 10 >= f * 9)
                .collect(),
        };
        if fresh {
            self.widths.clear();
        }
        self.widths.resize(columns, None);
    }

    pub fn set_option(&mut self, option: &TableOption, cx: &mut Context<Self>) {
        match option {
            TableOption::Delimiter(d) => self.overrides.delimiter = *d,
            TableOption::Quote(q) => self.overrides.quote = *q,
            TableOption::Header(h) => self.overrides.header = *h,
            TableOption::Encoding(name) => {
                self.overrides.encoding = name.and_then(|n| Encoding::for_label(n.as_bytes()));
                self.reload(cx);
                return;
            }
        }
        // Re-guess the header for the new separator unless it's forced.
        if let (Some(source), Some(guessed)) = (&self.source, self.guessed.as_mut()) {
            let dialect = Dialect {
                delimiter: self
                    .overrides
                    .delimiter
                    .unwrap_or(guessed.dialect.delimiter),
                quote: self.overrides.quote.unwrap_or(guessed.dialect.quote),
            };
            guessed.header = parse::guess_header(&source.rope, dialect);
        }
        self.reindex(true, cx);
    }

    // ---------------------------------------------------------------------
    // Rows
    // ---------------------------------------------------------------------

    /// Records, header row included.
    fn records(&self) -> usize {
        self.index.as_ref().map_or(0, |i| i.records)
    }

    /// Data rows: records without the header.
    fn rows(&self) -> usize {
        self.records().saturating_sub(self.has_header() as usize)
    }

    fn column_count(&self) -> usize {
        self.index.as_ref().map_or(0, |i| i.columns).max(1)
    }

    /// Records `first..first + count`, from the cache when it has them.
    fn records_range(&mut self, first: usize, count: usize) -> &[Vec<SharedString>] {
        let covered = matches!(&self.cache, Some((generation, start, rows))
            if *generation == self.generation
                && *start <= first
                && first + count <= start + rows.len());
        if !covered {
            let (Some(index), Some(source)) = (&self.index, &self.source) else {
                return &[];
            };
            // Read a screen's worth around the request so small scrolls hit.
            let start = first.saturating_sub(count);
            let rows = index
                .records(&source.rope, self.dialect(), start, count * 3)
                .into_iter()
                .map(|r| r.into_iter().map(SharedString::from).collect())
                .collect();
            self.cache = Some((self.generation, start, rows));
        }
        let (_, start, rows) = self.cache.as_ref().unwrap();
        let from = (first - start).min(rows.len());
        let to = (first - start + count).min(rows.len());
        &rows[from..to]
    }

    fn header_names(&mut self, columns: std::ops::Range<usize>) -> Vec<SharedString> {
        if !self.has_header() {
            return columns.map(|c| column_letter(c).into()).collect();
        }
        let header = match &self.header {
            Some((generation, header)) if *generation == self.generation => header.clone(),
            _ => {
                let header = self
                    .records_range(0, 1)
                    .first()
                    .cloned()
                    .unwrap_or_default();
                self.header = Some((self.generation, header.clone()));
                header
            }
        };
        columns
            .map(|c| header.get(c).cloned().unwrap_or_default())
            .collect()
    }

    fn cell(&mut self, row: usize, col: usize) -> Option<SharedString> {
        let record = row + self.has_header() as usize;
        self.records_range(record, 1).first()?.get(col).cloned()
    }

    // ---------------------------------------------------------------------
    // Geometry
    // ---------------------------------------------------------------------

    fn column_width(&self, col: usize, char_width: Pixels) -> Pixels {
        self.widths.get(col).copied().flatten().unwrap_or_else(|| {
            let chars = self
                .columns
                .chars
                .get(col)
                .copied()
                .unwrap_or(MIN_COLUMN_CHARS);
            char_width * chars as f32 + px(CELL_PAD * 2.)
        })
    }

    fn scroll_by(&mut self, delta: Point<Pixels>, cx: &mut Context<Self>) {
        self.scroll_x -= delta.x;
        self.scroll_y -= f32::from(delta.y) as f64;
        self.clamp_scroll();
        self.scrolled_at = Some(Instant::now());
        // Repaint once more after the overlay scrollbars should fade.
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SCROLLBAR_VISIBLE_FOR).await;
            this.update(cx, |_, cx| cx.notify()).ok();
        })
        .detach();
        cx.notify();
    }

    fn content_width(&self) -> Pixels {
        self.layout.xs.last().copied().unwrap_or_default()
    }

    fn content_height(&self) -> f64 {
        f32::from(self.layout.row_height) as f64 * self.rows() as f64
    }

    fn max_scroll_y(&self) -> f64 {
        (self.content_height() - f32::from(self.layout.body.size.height) as f64).max(0.)
    }

    fn clamp_scroll(&mut self) {
        let max_x = (self.content_width() - self.layout.body.size.width).max(px(0.));
        self.scroll_x = self.scroll_x.min(max_x).max(px(0.));
        self.scroll_y = self.scroll_y.min(self.max_scroll_y()).max(0.);
    }

    fn clamp_selection(&mut self) {
        let (rows, columns) = (self.rows(), self.column_count());
        if let Some((row, col)) = &mut self.selected {
            if rows == 0 {
                self.selected = None;
            } else {
                *row = (*row).min(rows - 1);
                *col = (*col).min(columns - 1);
            }
        }
    }

    /// The data row and column under a window position in the body.
    fn cell_at(&self, p: Point<Pixels>) -> Option<(usize, usize)> {
        let l = &self.layout;
        if !l.body.contains(&p) || l.row_height <= px(0.) {
            return None;
        }
        let y = f32::from(p.y - l.body.top()) as f64 + self.scroll_y;
        let x = p.x - l.body.left() + self.scroll_x;
        let row = (y / f32::from(l.row_height) as f64).floor() as usize;
        let col = l.xs.partition_point(|&left| left <= x).checked_sub(1)?;
        (row < self.rows() && col < self.column_count()).then_some((row, col))
    }

    /// The column whose right edge is under `p` in the header.
    fn edge_at(&self, p: Point<Pixels>) -> Option<usize> {
        let l = &self.layout;
        if !l.header.contains(&p) {
            return None;
        }
        let x = p.x - l.body.left() + self.scroll_x;
        l.xs.iter()
            .skip(1)
            .position(|&right| (right - x).abs() <= px(RESIZE_GRAB))
    }

    fn select(&mut self, row: usize, col: usize, cx: &mut Context<Self>) {
        let rows = self.rows();
        if rows == 0 {
            return;
        }
        let (row, col) = (row.min(rows - 1), col.min(self.column_count() - 1));
        self.selected = Some((row, col));
        // Scroll it into view.
        let l = &self.layout;
        if l.row_height > px(0.) {
            let row_height = f32::from(l.row_height) as f64;
            let height = f32::from(l.body.size.height) as f64;
            let top = row_height * row as f64;
            let bottom = top + row_height;
            if top < self.scroll_y {
                self.scroll_y = top;
            } else if bottom > self.scroll_y + height {
                self.scroll_y = bottom - height;
            }
            if let (Some(&left), Some(&right)) = (l.xs.get(col), l.xs.get(col + 1)) {
                if left < self.scroll_x {
                    self.scroll_x = left;
                } else if right > self.scroll_x + l.body.size.width {
                    self.scroll_x = (right - l.body.size.width).min(left);
                }
            }
            self.clamp_scroll();
        }
        cx.notify();
    }

    fn move_by(&mut self, rows: isize, cols: isize, cx: &mut Context<Self>) {
        let (row, col) = self.selected.unwrap_or((0, 0));
        let row = (row as isize + rows).max(0) as usize;
        let col = (col as isize + cols).max(0) as usize;
        self.select(row, col, cx);
    }

    fn page_rows(&self) -> isize {
        let l = &self.layout;
        if l.row_height <= px(0.) {
            return 20;
        }
        ((l.body.size.height / l.row_height).floor() as isize - 1).max(1)
    }

    fn copy_selection(&mut self, cx: &mut Context<Self>) {
        if let Some((row, col)) = self.selected
            && let Some(text) = self.cell(row, col)
        {
            cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
        }
    }

    /// The selected row, tab separated, to paste into a spreadsheet.
    fn copy_row(&mut self, cx: &mut Context<Self>) {
        let Some((row, _)) = self.selected else {
            return;
        };
        let record = row + self.has_header() as usize;
        if let Some(fields) = self.records_range(record, 1).first() {
            let line = fields
                .iter()
                .map(|f| f.as_ref())
                .collect::<Vec<_>>()
                .join("\t");
            cx.write_to_clipboard(ClipboardItem::new_string(line));
        }
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle, cx);
        if let Some(col) = self.edge_at(event.position) {
            if event.click_count == 2 {
                // Double-click an edge: back to the measured width.
                if let Some(w) = self.widths.get_mut(col) {
                    *w = None;
                }
                cx.notify();
                return;
            }
            let l = &self.layout;
            let width = l.xs[col + 1] - l.xs[col];
            self.resizing = Some((col, event.position.x, width));
            return;
        }
        if let Some((row, col)) = self.cell_at(event.position) {
            self.select(row, col, cx);
        }
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some((col, start_x, start_width)) = self.resizing else {
            return;
        };
        let width = (start_width + event.position.x - start_x).max(px(24.));
        if self.widths.len() <= col {
            self.widths.resize(col + 1, None);
        }
        self.widths[col] = Some(width);
        cx.notify();
    }

    /// Right click: copying, flipping to source, and how the file is read.
    fn context_menu(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        if let Some((row, col)) = self.cell_at(event.position) {
            self.selected = Some((row, col));
            cx.notify();
        }
        let nothing = self.selected.is_none();
        let mut menu = gpui_component::native_menu::NativeMenu::new()
            .menu_with_disabled("Copy Cell", nothing, Box::new(Copy))
            .menu_with_disabled("Copy Row", nothing, Box::new(CopyRow))
            .separator();
        menu = self.options_menu(menu);
        menu = menu
            .separator()
            .menu("Show Source", Box::new(TogglePreview));
        if self.editor.read(cx).path().is_some() {
            menu = menu
                .separator()
                .menu(crate::platform::REVEAL_LABEL, Box::new(RevealInFinder))
                .menu("Copy Path", Box::new(CopyPath))
                .menu("Copy Relative Path", Box::new(CopyRelativePath));
        }
        menu.show(event.position, window, cx);
    }

    /// Separator, quote, header and encoding, as submenus.
    pub fn options_menu(
        &self,
        menu: gpui_component::native_menu::NativeMenu,
    ) -> gpui_component::native_menu::NativeMenu {
        menu.submenu("Separator", self.delimiter_menu())
            .submenu("Quote", self.quote_menu())
            .submenu("Header Row", self.header_menu())
            .submenu("Encoding", self.encoding_menu())
    }

    pub fn delimiter_menu(&self) -> gpui_component::native_menu::NativeMenu {
        use gpui_component::native_menu::NativeMenu;
        let set = |d| {
            Box::new(SetTableOption {
                option: TableOption::Delimiter(d),
            })
        };
        let guessed = self.guessed.as_ref().map_or(b',', |g| g.dialect.delimiter);
        let mut menu = NativeMenu::new()
            .menu_with_check(
                format!("Detect ({})", parse::delimiter_name(guessed)),
                self.overrides.delimiter.is_none(),
                set(None),
            )
            .separator();
        for (d, name) in parse::DELIMITERS {
            menu = menu.menu_with_check(*name, self.overrides.delimiter == Some(*d), set(Some(*d)));
        }
        menu
    }

    fn quote_menu(&self) -> gpui_component::native_menu::NativeMenu {
        use gpui_component::native_menu::NativeMenu;
        let set = |q| {
            Box::new(SetTableOption {
                option: TableOption::Quote(q),
            })
        };
        let q = self.overrides.quote;
        NativeMenu::new()
            .menu_with_check("Detect", q.is_none(), set(None))
            .separator()
            .menu_with_check(
                "Double Quote \"",
                q == Some(Some(b'"')),
                set(Some(Some(b'"'))),
            )
            .menu_with_check(
                "Single Quote '",
                q == Some(Some(b'\'')),
                set(Some(Some(b'\''))),
            )
            .menu_with_check("None", q == Some(None), set(Some(None)))
    }

    pub fn header_menu(&self) -> gpui_component::native_menu::NativeMenu {
        use gpui_component::native_menu::NativeMenu;
        let set = |h| {
            Box::new(SetTableOption {
                option: TableOption::Header(h),
            })
        };
        let guessed = self.guessed.as_ref().is_some_and(|g| g.header);
        let h = self.overrides.header;
        NativeMenu::new()
            .menu_with_check(
                if guessed {
                    "Detect (First Row)"
                } else {
                    "Detect (None)"
                },
                h.is_none(),
                set(None),
            )
            .separator()
            .menu_with_check("First Row Is Header", h == Some(true), set(Some(true)))
            .menu_with_check("No Header Row", h == Some(false), set(Some(false)))
    }

    pub fn encoding_menu(&self) -> gpui_component::native_menu::NativeMenu {
        use gpui_component::native_menu::NativeMenu;
        let set = |e| {
            Box::new(SetTableOption {
                option: TableOption::Encoding(e),
            })
        };
        let forced = self.overrides.encoding.map(|e| e.name());
        let mut menu = NativeMenu::new()
            .menu_with_check(
                format!("Detect ({})", self.encoding_name()),
                forced.is_none(),
                set(None),
            )
            .separator();
        for name in ENCODINGS {
            let label = Encoding::for_label(name.as_bytes()).map_or(*name, |e| e.name());
            menu = menu.menu_with_check(*name, forced == Some(label), set(Some(*name)));
        }
        menu
    }

    // ---------------------------------------------------------------------
    // Status bar
    // ---------------------------------------------------------------------

    /// "12,345 rows × 8 columns", or what's going on instead.
    pub fn status(&self) -> SharedString {
        if let Some(error) = &self.error {
            return error.clone();
        }
        if self.index.is_none() {
            return "Reading…".into();
        }
        let rows = self.rows();
        let more = if self.index_task.is_some() { "+" } else { "" };
        format!(
            "{}{more} {} × {} {}",
            thousands(rows),
            if rows == 1 { "row" } else { "rows" },
            self.column_count(),
            if self.column_count() == 1 {
                "column"
            } else {
                "columns"
            },
        )
        .into()
    }

    pub fn delimiter_label(&self) -> SharedString {
        parse::delimiter_name(self.dialect().delimiter).into()
    }

    pub fn header_label(&self) -> SharedString {
        if self.has_header() {
            "Header Row".into()
        } else {
            "No Header".into()
        }
    }

    pub fn encoding_name(&self) -> &'static str {
        self.source
            .as_ref()
            .map(|s| s.encoding)
            .or(self.overrides.encoding)
            .unwrap_or(encoding_rs::UTF_8)
            .name()
    }
}

/// Columns without a header are lettered like a spreadsheet: A … Z, AA ….
fn column_letter(mut col: usize) -> String {
    let mut name = Vec::new();
    loop {
        name.push(b'A' + (col % 26) as u8);
        if col < 26 {
            break;
        }
        col = col / 26 - 1;
    }
    name.reverse();
    String::from_utf8(name).unwrap()
}

fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn display_width(text: &str) -> usize {
    text.chars().map(|c| c.width().unwrap_or(0).max(1)).sum()
}

/// `text` on one line, cut to `max` columns with an ellipsis.
fn fit(text: &str, max: usize) -> String {
    let mut out = String::new();
    let mut width = 0;
    let total = display_width(text);
    for c in text.chars() {
        let c = match c {
            '\n' => '↵',
            '\r' | '\t' => ' ',
            c if c.is_control() => '·',
            c => c,
        };
        let w = c.width().unwrap_or(0).max(1);
        if total > max && width + w > max.saturating_sub(1) {
            out.push('…');
            break;
        }
        if width + w > max {
            break;
        }
        width += w;
        out.push(c);
    }
    out
}

impl Render for CsvPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::palette(cx).clone();
        div()
            .id("csv-table")
            .key_context("CsvTable")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(palette.editor_bg)
            .on_action(cx.listener(|this, _: &MoveUp, _, cx| this.move_by(-1, 0, cx)))
            .on_action(cx.listener(|this, _: &MoveDown, _, cx| this.move_by(1, 0, cx)))
            .on_action(cx.listener(|this, _: &MoveLeft, _, cx| this.move_by(0, -1, cx)))
            .on_action(cx.listener(|this, _: &MoveRight, _, cx| this.move_by(0, 1, cx)))
            .on_action(cx.listener(|this, _: &Tab, _, cx| this.move_by(0, 1, cx)))
            .on_action(cx.listener(|this, _: &Outdent, _, cx| this.move_by(0, -1, cx)))
            .on_action(cx.listener(|this, _: &PageUp, _, cx| {
                let page = this.page_rows();
                this.move_by(-page, 0, cx)
            }))
            .on_action(cx.listener(|this, _: &PageDown, _, cx| {
                let page = this.page_rows();
                this.move_by(page, 0, cx)
            }))
            .on_action(cx.listener(|this, _: &MoveDocStart, _, cx| {
                let col = this.selected.map_or(0, |s| s.1);
                this.select(0, col, cx)
            }))
            .on_action(cx.listener(|this, _: &MoveDocEnd, _, cx| {
                let col = this.selected.map_or(0, |s| s.1);
                this.select(usize::MAX, col, cx)
            }))
            .on_action(cx.listener(|this, _: &MoveLineStart, _, cx| {
                let row = this.selected.map_or(0, |s| s.0);
                this.select(row, 0, cx)
            }))
            .on_action(cx.listener(|this, _: &MoveLineEnd, _, cx| {
                let row = this.selected.map_or(0, |s| s.0);
                this.select(row, usize::MAX, cx)
            }))
            .on_action(cx.listener(|this, _: &Copy, _, cx| this.copy_selection(cx)))
            .on_action(cx.listener(|this, _: &CopyRow, _, cx| this.copy_row(cx)))
            .on_action(cx.listener(|this, action: &SetTableOption, _, cx| {
                this.set_option(&action.option, cx)
            }))
            .child(TableElement { view: cx.entity() })
    }
}

// -------------------------------------------------------------------------
// Painting
// -------------------------------------------------------------------------

struct TableElement {
    view: Entity<CsvPreview>,
}

impl IntoElement for TableElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

struct Text {
    origin: Point<Pixels>,
    line: ShapedLine,
}

pub struct PrepaintState {
    hitbox: Hitbox,
    /// Header edges that resize their column.
    edges: Vec<Hitbox>,
    body: Bounds<Pixels>,
    row_height: Pixels,
    body_quads: Vec<PaintQuad>,
    body_text: Vec<Text>,
    header_quads: Vec<PaintQuad>,
    header_text: Vec<Text>,
    gutter_quads: Vec<PaintQuad>,
    gutter_text: Vec<Text>,
    overlay: Vec<PaintQuad>,
    notice: Option<Text>,
}

impl Element for TableElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> PrepaintState {
        let palette = theme::palette(cx).clone();
        let settings = settings::get(cx).clone();
        let mono = font(theme::fonts(cx).mono.clone());
        let bold = gpui::Font {
            weight: FontWeight::SEMIBOLD,
            ..mono.clone()
        };
        let ui = font(theme::fonts(cx).ui.clone());
        let font_size = px(settings.font_size);
        let row_height = px(settings.line_height() + 6.);
        let text_system = window.text_system().clone();
        let char_width = text_system
            .advance(text_system.resolve_font(&mono), font_size, 'm')
            .map(|s| s.width)
            .unwrap_or(font_size * 0.6);
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        let shape = |text: String, font: &gpui::Font, color, size| {
            let run = TextRun {
                len: text.len(),
                font: font.clone(),
                color,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            text_system.shape_line(SharedString::from(text), size, &[run], None)
        };
        let pad = px(CELL_PAD);
        // Text sits a little below the middle of its row, like the editor.
        let text_dy = (row_height - px(settings.line_height())) / 2.;

        self.view.update(cx, |view, _| {
            let mut state = PrepaintState {
                hitbox,
                edges: Vec::new(),
                body: bounds,
                row_height,
                body_quads: Vec::new(),
                body_text: Vec::new(),
                header_quads: Vec::new(),
                header_text: Vec::new(),
                gutter_quads: Vec::new(),
                gutter_text: Vec::new(),
                overlay: Vec::new(),
                notice: None,
            };
            let notice = if view.index.is_none() || view.error.is_some() {
                Some(view.status())
            } else if view.records() == 0 {
                Some("Empty file".into())
            } else {
                None
            };
            if let Some(text) = notice {
                let line = shape(text.to_string(), &ui, palette.text_secondary, px(13.));
                let origin = point(
                    bounds.left() + (bounds.size.width - line.width) / 2.,
                    bounds.top() + bounds.size.height * 0.4,
                );
                state.notice = Some(Text { origin, line });
                view.layout = Layout::default();
                return state;
            }

            let rows = view.rows();
            let columns = view.column_count();
            let digits = rows.max(1).to_string().len().max(2);
            let gutter = char_width * digits as f32 + pad * 2.;
            let header = Bounds::new(
                point(bounds.left() + gutter, bounds.top()),
                size((bounds.size.width - gutter).max(px(0.)), row_height),
            );
            let body = Bounds::new(
                point(bounds.left() + gutter, bounds.top() + row_height),
                size(
                    (bounds.size.width - gutter).max(px(0.)),
                    (bounds.size.height - row_height).max(px(0.)),
                ),
            );
            let mut xs = Vec::with_capacity(columns + 1);
            let mut x = px(0.);
            for col in 0..columns {
                xs.push(x);
                x += view.column_width(col, char_width);
            }
            xs.push(x);
            view.layout = Layout {
                body,
                header,
                row_height,
                xs: xs.clone(),
            };
            view.clamp_scroll();
            let scroll_x = view.scroll_x;
            let scroll_y = view.scroll_y;
            state.body = body;

            // What's visible.
            // Rows are placed relative to the first visible one, so their
            // spacing stays exact however far down it is.
            let rh = f32::from(row_height) as f64;
            let first_row = ((scroll_y / rh).floor() as usize).min(rows);
            let first_top = body.top() - px((scroll_y - first_row as f64 * rh) as f32);
            let last_row =
                (((scroll_y + f32::from(body.size.height) as f64) / rh).ceil() as usize).min(rows);
            let first_col = xs.partition_point(|&l| l <= scroll_x).saturating_sub(1);
            let last_col = xs
                .partition_point(|&l| l < scroll_x + body.size.width)
                .min(columns);
            let col_x = |col: usize| body.left() + xs[col] - scroll_x;
            let row_y = |row: usize| first_top + row_height * (row - first_row) as f32;

            // Body: stripes, selection, cells.
            let selected = view.selected;
            for row in first_row..last_row {
                let y = row_y(row);
                if row % 2 == 1 {
                    state.body_quads.push(fill(
                        Bounds::new(point(body.left(), y), size(body.size.width, row_height)),
                        palette.current_line,
                    ));
                }
            }
            if let Some((row, col)) = selected
                && (first_row..last_row).contains(&row)
            {
                let y = row_y(row);
                state.body_quads.push(fill(
                    Bounds::new(point(body.left(), y), size(body.size.width, row_height)),
                    palette.row_selected_inactive,
                ));
                let cell = Bounds::new(
                    point(col_x(col), y),
                    size(xs[col + 1] - xs[col], row_height),
                );
                state.body_quads.push(fill(cell, palette.selection));
            }
            let header_offset = view.has_header() as usize;
            let records: Vec<Vec<SharedString>> = view
                .records_range(first_row + header_offset, last_row - first_row)
                .to_vec();
            for (i, record) in records.iter().enumerate() {
                let y = row_y(first_row + i) + text_dy;
                for col in first_col..last_col {
                    let Some(value) = record.get(col).filter(|v| !v.is_empty()) else {
                        continue;
                    };
                    let width = xs[col + 1] - xs[col] - pad * 2.;
                    let max = (width / char_width).floor().max(0.) as usize;
                    let line = shape(fit(value, max), &mono, palette.text, font_size);
                    let numeric = view.columns.numeric.get(col).copied().unwrap_or(false);
                    let x = if numeric {
                        col_x(col + 1) - pad - line.width
                    } else {
                        col_x(col) + pad
                    };
                    state.body_text.push(Text {
                        origin: point(x, y),
                        line,
                    });
                }
            }
            // Column rules down the body.
            for col in (first_col + 1)..=last_col {
                state.body_quads.push(fill(
                    Bounds::new(
                        point(col_x(col), body.top()),
                        size(px(1.), body.size.height),
                    ),
                    palette.border.opacity(0.6),
                ));
            }

            // Header.
            state.header_quads.push(fill(
                Bounds::new(bounds.origin, size(bounds.size.width, row_height)),
                palette.code_bg,
            ));
            state.header_quads.push(fill(
                Bounds::new(
                    point(bounds.left(), header.bottom() - px(1.)),
                    size(bounds.size.width, px(1.)),
                ),
                palette.border,
            ));
            let names = view.header_names(first_col..last_col);
            let has_header = view.has_header();
            for (name, col) in names.iter().zip(first_col..last_col) {
                let width = xs[col + 1] - xs[col] - pad * 2.;
                let max = (width / char_width).floor().max(0.) as usize;
                let color = if has_header {
                    palette.text
                } else {
                    palette.text_tertiary
                };
                let line = shape(fit(name, max), &bold, color, font_size);
                let numeric = view.columns.numeric.get(col).copied().unwrap_or(false);
                let x = if numeric {
                    col_x(col + 1) - pad - line.width
                } else {
                    col_x(col) + pad
                };
                state.header_text.push(Text {
                    origin: point(x, header.top() + text_dy),
                    line,
                });
                let edge = col_x(col + 1);
                state.header_quads.push(fill(
                    Bounds::new(point(edge, header.top()), size(px(1.), row_height)),
                    palette.border,
                ));
                state.edges.push(window.insert_hitbox(
                    Bounds::new(
                        point(edge - px(RESIZE_GRAB), header.top()),
                        size(px(RESIZE_GRAB * 2.), row_height),
                    ),
                    HitboxBehavior::Normal,
                ));
            }

            // Gutter: row numbers.
            state.gutter_quads.push(fill(
                Bounds::new(
                    point(bounds.left(), body.top()),
                    size(gutter, body.size.height),
                ),
                palette.editor_bg,
            ));
            state.gutter_quads.push(fill(
                Bounds::new(
                    point(body.left() - px(1.), bounds.top()),
                    size(px(1.), bounds.size.height),
                ),
                palette.border,
            ));
            for row in first_row..last_row {
                let active = selected.is_some_and(|(r, _)| r == row);
                let color = if active {
                    palette.gutter_active_text
                } else {
                    palette.gutter_text
                };
                let line = shape((row + 1).to_string(), &mono, color, font_size);
                let x = body.left() - pad - line.width;
                state.gutter_text.push(Text {
                    origin: point(x, row_y(row) + text_dy),
                    line,
                });
            }

            // Overlay scrollbars while scrolling, like the editor.
            if view
                .scrolled_at
                .is_some_and(|t| t.elapsed() < SCROLLBAR_VISIBLE_FOR)
            {
                let content_height = view.content_height();
                let content_width = view.content_width();
                let thumb_color = palette.text_tertiary.opacity(0.6);
                let max_y = view.max_scroll_y();
                if max_y > 0. {
                    let track = body.size.height - px(4.);
                    let visible = f32::from(body.size.height) as f64 / content_height;
                    let thumb = (track * visible as f32).max(px(24.));
                    let ratio = (scroll_y / max_y) as f32;
                    let y = body.top() + px(2.) + (track - thumb) * ratio;
                    state.overlay.push(
                        fill(
                            Bounds::new(point(bounds.right() - px(9.), y), size(px(6.), thumb)),
                            thumb_color,
                        )
                        .corner_radii(px(3.)),
                    );
                }
                if content_width > body.size.width {
                    let track = body.size.width - px(14.);
                    let thumb = (track * (body.size.width / content_width)).max(px(24.));
                    let ratio = scroll_x / (content_width - body.size.width);
                    let x = body.left() + px(2.) + (track - thumb) * ratio;
                    state.overlay.push(
                        fill(
                            Bounds::new(point(x, bounds.bottom() - px(9.)), size(thumb, px(6.))),
                            thumb_color,
                        )
                        .corner_radii(px(3.)),
                    );
                }
            }
            state
        })
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        state: &mut PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let palette = theme::palette(cx).clone();
        window.paint_quad(fill(bounds, palette.editor_bg));
        let row_height = state.row_height;
        let paint_text = |texts: &[Text], window: &mut Window, cx: &mut App| {
            for text in texts {
                let _ = text
                    .line
                    .paint(text.origin, row_height, TextAlign::Left, None, window, cx);
            }
        };
        if let Some(notice) = &state.notice {
            paint_text(std::slice::from_ref(notice), window, cx);
            return;
        }
        let resizing = self.view.read(cx).resizing.is_some();
        if resizing || state.edges.iter().any(|e| e.is_hovered(window)) {
            window.set_cursor_style(CursorStyle::ResizeLeftRight, &state.hitbox);
        }
        self.register_mouse_listeners(&state.hitbox, row_height, window);

        let body = state.body;
        window.with_content_mask(Some(gpui::ContentMask { bounds: body }), |window| {
            for quad in std::mem::take(&mut state.body_quads) {
                window.paint_quad(quad);
            }
            paint_text(&state.body_text, window, cx);
        });
        let header = Bounds::new(
            point(body.left(), bounds.top()),
            size(body.size.width, row_height),
        );
        for quad in std::mem::take(&mut state.header_quads) {
            window.paint_quad(quad);
        }
        window.with_content_mask(Some(gpui::ContentMask { bounds: header }), |window| {
            paint_text(&state.header_text, window, cx);
        });
        for quad in std::mem::take(&mut state.gutter_quads) {
            window.paint_quad(quad);
        }
        let gutter = Bounds::new(
            point(bounds.left(), body.top()),
            size(body.left() - bounds.left(), body.size.height),
        );
        window.with_content_mask(Some(gpui::ContentMask { bounds: gutter }), |window| {
            paint_text(&state.gutter_text, window, cx);
        });
        for quad in std::mem::take(&mut state.overlay) {
            window.paint_quad(quad);
        }
    }
}

impl TableElement {
    fn register_mouse_listeners(&self, hitbox: &Hitbox, row_height: Pixels, window: &mut Window) {
        let view = self.view.clone();
        let hb = hitbox.clone();
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble || !hb.is_hovered(window) {
                return;
            }
            // Control-click is a right click on macOS.
            let secondary = event.button == MouseButton::Right
                || (cfg!(target_os = "macos")
                    && event.button == MouseButton::Left
                    && event.modifiers.control);
            if secondary {
                view.update(cx, |view, cx| view.context_menu(event, window, cx));
                cx.stop_propagation();
            } else if event.button == MouseButton::Left {
                view.update(cx, |view, cx| view.mouse_down(event, window, cx));
                cx.stop_propagation();
            }
        });
        let view = self.view.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _window, cx| {
            if phase == DispatchPhase::Bubble && event.pressed_button == Some(MouseButton::Left) {
                view.update(cx, |view, cx| view.mouse_move(event, cx));
            }
        });
        let view = self.view.clone();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, _window, cx| {
            if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
                view.update(cx, |view, cx| {
                    if view.resizing.take().is_some() {
                        cx.notify();
                    }
                });
            }
        });
        let view = self.view.clone();
        let hb = hitbox.clone();
        window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble || !hb.is_hovered(window) {
                return;
            }
            let mut delta = event.delta.pixel_delta(row_height);
            // A plain mouse wheel scrolls sideways with Shift held.
            if event.modifiers.shift && delta.x == px(0.) {
                delta = point(delta.y, px(0.));
            }
            view.update(cx, |view, cx| view.scroll_by(delta, cx));
            cx.stop_propagation();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_columns() {
        assert_eq!(column_letter(0), "A");
        assert_eq!(column_letter(25), "Z");
        assert_eq!(column_letter(26), "AA");
        assert_eq!(column_letter(27), "AB");
        assert_eq!(column_letter(701), "ZZ");
        assert_eq!(column_letter(702), "AAA");
    }

    #[test]
    fn fits_cells() {
        assert_eq!(fit("hello", 10), "hello");
        assert_eq!(fit("hello world", 6), "hello…");
        assert_eq!(fit("a\nb", 10), "a↵b");
        assert_eq!(fit("日本語", 4), "日…");
        assert_eq!(thousands(1234567), "1,234,567");
        assert_eq!(thousands(12), "12");
    }
}

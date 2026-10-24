//! Paints the editor: only the visible rows are shaped, so cost per frame is
//! the same for a 10-line file and a 10-million-line file.
//!
//! The view is a list of display rows. Without word wrap each line is one
//! row; with it, a line is split into rows by [`me_text::wrap`].

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    App, Bounds, ContentMask, CursorStyle, DispatchPhase, Element, ElementId, ElementInputHandler,
    Entity, GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId, IntoElement,
    LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point,
    ScrollWheelEvent, ShapedLine, SharedString, Style, TextAlign, TextRun, Window, fill, font,
    point, px, relative, size,
};
use me_text::movement::{char_col_at_visual, char_width as cell_width, visual_col};
use me_text::wrap::{row_of, row_range, wrap_line};
use me_text::{Document, EditorState};

use super::{Autoscroll, EditorView};
use crate::settings;
use crate::theme::{self, Palette};

const GUTTER_PAD_LEFT: f32 = 14.;
const GUTTER_PAD_RIGHT: f32 = 12.;
const TEXT_PAD_LEFT: f32 = 4.;
/// Room kept free right of wrapped text (the overlay scrollbar).
const WRAP_PAD_RIGHT: f32 = 14.;
const MIN_WRAP_COLS: usize = 20;
/// Unwrapped lines longer than this are shaped only around the visible
/// columns.
const LONG_LINE: usize = 4096;
/// Highlighting stops after this many bytes of visible text.
const MAX_HIGHLIGHT_BYTES: usize = 512 * 1024;
/// Wrap results are cached for lines at least this long.
const CACHE_WRAP_FROM: usize = 1024;
const SCROLLBAR_VISIBLE_FOR: Duration = Duration::from_millis(1200);

/// Geometry of the last painted frame, used for mouse hit-testing, IME and
/// row-wise movement.
#[derive(Clone)]
pub struct LayoutSnapshot {
    pub bounds: Bounds<Pixels>,
    /// Window x of column 0 when not scrolled horizontally.
    pub text_left: Pixels,
    pub line_height: Pixels,
    pub char_width: Pixels,
    pub scroll_x: Pixels,
    pub tab_width: usize,
    /// Columns lines wrap at, when word wrap is on.
    pub wrap_cols: Option<usize>,
    pub rows: Vec<RowLayout>,
}

/// One display row.
#[derive(Clone)]
pub struct RowLayout {
    pub line: usize,
    /// Char columns of the line shown in this row (for a windowed long line,
    /// the shaped window).
    pub start_col: usize,
    pub end_col: usize,
    /// Window y of the row's top.
    pub top: Pixels,
    /// x of each char boundary from `start_col` to `end_col`, relative to
    /// the row's column 0.
    pub char_x: Vec<Pixels>,
    pub first: bool,
    pub last: bool,
    /// A long unwrapped line shaped only around the viewport.
    pub windowed: bool,
}

impl RowLayout {
    /// Whether the cursor at `col` is drawn on this row. The end of a wrapped
    /// row is drawn at the start of the next one.
    fn shows(&self, col: usize) -> bool {
        self.windowed || (col >= self.start_col && (col < self.end_col || self.last))
    }

    fn x_for_col(&self, col: usize, char_width: Pixels) -> Pixels {
        let first = self.char_x.first().copied().unwrap_or_default();
        let last = self.char_x.last().copied().unwrap_or_default();
        if col < self.start_col {
            first - char_width * (self.start_col - col) as f32
        } else {
            match self.char_x.get(col - self.start_col) {
                Some(x) => *x,
                None => last + char_width * (col - self.end_col) as f32,
            }
        }
    }
}

pub struct Hit {
    pub pos: usize,
    pub line: usize,
    pub visual_col: usize,
}

impl LayoutSnapshot {
    pub fn hit(&self, p: Point<Pixels>, state: &EditorState) -> Hit {
        let doc = &state.doc;
        let x = (p.x - self.text_left + self.scroll_x).max(px(0.));
        let visual_col = (x / self.char_width).round().max(0.) as usize;
        let last_line = doc.len_lines().saturating_sub(1);
        let (Some(first), Some(last)) = (self.rows.first(), self.rows.last()) else {
            return Hit {
                pos: 0,
                line: 0,
                visual_col,
            };
        };
        let row = self
            .rows
            .iter()
            .find(|r| p.y >= r.top && p.y < r.top + self.line_height);
        let (line, col) = match row {
            Some(r) => {
                let mut col = r.start_col + nearest(&r.char_x, x);
                if !r.last && col >= r.end_col && r.end_col > r.start_col {
                    col = r.end_col - 1;
                }
                (r.line, col)
            }
            None => {
                // Outside the painted rows (dragging past the edge): count
                // one line per row height.
                let line = if p.y < first.top {
                    let up = ((first.top - p.y) / self.line_height).ceil() as usize;
                    first.line.saturating_sub(up)
                } else {
                    let down = ((p.y - last.top) / self.line_height).floor() as usize;
                    (last.line + down).min(last_line)
                };
                let col = char_col_at_visual(doc.line(line), visual_col, self.tab_width).0;
                (line, col)
            }
        };
        Hit {
            pos: doc.line_start(line) + col.min(doc.line_len(line)),
            line,
            visual_col,
        }
    }

    pub fn row_for(&self, line: usize, col: usize) -> Option<&RowLayout> {
        self.rows.iter().find(|r| r.line == line && r.shows(col))
    }

    pub fn point_for_pos(&self, pos: usize, state: &EditorState) -> Option<Point<Pixels>> {
        let (line, col) = state.doc.line_col(pos);
        let row = self.row_for(line, col)?;
        Some(point(
            self.text_left + row.x_for_col(col, self.char_width) - self.scroll_x,
            row.top,
        ))
    }
}

fn nearest(xs: &[Pixels], x: Pixels) -> usize {
    let ix = xs.partition_point(|v| *v < x);
    if ix == 0 {
        return 0;
    }
    if ix >= xs.len() {
        return xs.len() - 1;
    }
    if (xs[ix] - x) < (x - xs[ix - 1]) {
        ix
    } else {
        ix - 1
    }
}

/// Wrap results for long lines, valid for one (revision, columns, tab width).
#[derive(Default)]
pub struct WrapCache {
    key: (u64, usize, usize),
    lines: HashMap<usize, Arc<[usize]>>,
}

/// Row geometry of the document for the current width and font: how many
/// display rows each line takes. Vertical positions are fractional line
/// indices (see [`super::ScrollPosition`]).
pub(crate) struct Rows<'a> {
    doc: &'a Document,
    tab_width: usize,
    wrap_cols: Option<usize>,
    line_height: f64,
    cache: &'a mut WrapCache,
}

impl<'a> Rows<'a> {
    pub(crate) fn new(
        doc: &'a Document,
        tab_width: usize,
        wrap_cols: Option<usize>,
        line_height: f64,
        cache: &'a mut WrapCache,
    ) -> Self {
        if let Some(cols) = wrap_cols {
            let key = (doc.revision(), cols, tab_width);
            if cache.key != key {
                cache.key = key;
                cache.lines.clear();
            }
        }
        Self {
            doc,
            tab_width,
            wrap_cols,
            line_height,
            cache,
        }
    }

    /// Start columns of the rows of `line`.
    pub(crate) fn rows(&mut self, line: usize) -> Arc<[usize]> {
        let Some(cols) = self.wrap_cols else {
            return Arc::from([0usize].as_slice());
        };
        if self.doc.line_len(line) < CACHE_WRAP_FROM {
            return wrap_line(self.doc.line(line), cols, self.tab_width).into();
        }
        if let Some(rows) = self.cache.lines.get(&line) {
            return rows.clone();
        }
        let rows: Arc<[usize]> = wrap_line(self.doc.line(line), cols, self.tab_width).into();
        self.cache.lines.insert(line, rows.clone());
        rows
    }

    fn height(&mut self, line: usize) -> f64 {
        if self.wrap_cols.is_none() {
            return self.line_height;
        }
        self.rows(line).len() as f64 * self.line_height
    }

    fn last_line(&self) -> usize {
        self.doc.len_lines().saturating_sub(1)
    }

    /// (line, pixels into that line's rows) of a position.
    fn split(&mut self, y: f64) -> (usize, f64) {
        let line = (y.max(0.) as usize).min(self.last_line());
        let frac = (y - line as f64).clamp(0., 1.);
        (line, frac * self.height(line))
    }

    fn join(&mut self, line: usize, offset: f64) -> f64 {
        line as f64 + offset / self.height(line)
    }

    /// The position `delta` pixels below `y` (above, if negative).
    pub(crate) fn advance(&mut self, y: f64, delta: f64) -> f64 {
        if self.wrap_cols.is_none() {
            return (y + delta / self.line_height).max(0.);
        }
        let (mut line, mut offset) = self.split(y);
        offset += delta;
        while offset < 0. {
            if line == 0 {
                return 0.;
            }
            line -= 1;
            offset += self.height(line);
        }
        loop {
            let h = self.height(line);
            if offset < h || line >= self.last_line() {
                return self.join(line, offset.min(h));
            }
            offset -= h;
            line += 1;
        }
    }

    /// Pixels from `a` down to `b` (negative if `b` is above), giving up past
    /// `cap`.
    fn distance(&mut self, a: f64, b: f64, cap: f64) -> f64 {
        if self.wrap_cols.is_none() {
            return ((b - a) * self.line_height).clamp(-cap, cap);
        }
        if b < a {
            return -self.distance(b, a, cap);
        }
        let (la, oa) = self.split(a);
        let (lb, ob) = self.split(b);
        if la == lb {
            return ob - oa;
        }
        let mut total = self.height(la) - oa;
        for line in la + 1..lb {
            if total > cap {
                return cap;
            }
            total += self.height(line);
        }
        (total + ob).min(cap)
    }

    /// The largest scroll position: the last row at the bottom edge.
    fn max_scroll(&mut self, viewport: f64) -> f64 {
        let len = self.doc.len_lines();
        if self.wrap_cols.is_none() {
            return (len as f64 - viewport / self.line_height).max(0.);
        }
        let mut remaining = viewport;
        let mut line = self.last_line();
        loop {
            let h = self.height(line);
            if remaining <= h {
                return self.join(line, h - remaining);
            }
            remaining -= h;
            if line == 0 {
                return 0.;
            }
            line -= 1;
        }
    }

    /// Position of the top of the row showing the cursor at `pos`.
    fn cursor_row_top(&mut self, pos: usize) -> f64 {
        let (line, col) = self.doc.line_col(pos);
        if self.wrap_cols.is_none() {
            return line as f64;
        }
        let rows = self.rows(line);
        line as f64 + row_of(&rows, col) as f64 / rows.len() as f64
    }
}

pub struct EditorElement {
    view: Entity<EditorView>,
}

impl EditorElement {
    pub fn new(view: Entity<EditorView>) -> Self {
        Self { view }
    }
}

impl IntoElement for EditorElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

pub struct PrepaintState {
    hitbox: Hitbox,
    text_bounds: Bounds<Pixels>,
    backgrounds: Vec<PaintQuad>,
    lines: Vec<(Point<Pixels>, ShapedLine)>,
    gutter: Vec<(Point<Pixels>, ShapedLine)>,
    cursors: Vec<PaintQuad>,
    /// Underline of the Cmd-hovered word.
    link: Vec<PaintQuad>,
    link_hovered: bool,
    scrollbar: Option<PaintQuad>,
    notice: Option<(Point<Pixels>, ShapedLine)>,
    line_height: Pixels,
}

/// A row to shape this frame.
struct PlannedRow {
    line: usize,
    start_col: usize,
    end_col: usize,
    top: Pixels,
    first: bool,
    last: bool,
    windowed: bool,
}

impl Element for EditorElement {
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
        let font_size = px(settings.font_size);
        let line_height = px(settings.line_height());
        let text_system = window.text_system().clone();
        let font_id = text_system.resolve_font(&mono);
        let char_width = text_system
            .advance(font_id, font_size, 'm')
            .map(|s| s.width)
            .unwrap_or(font_size * 0.6);
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);

        let len_lines = self.view.read(cx).state.doc.len_lines();
        let digits = len_lines.to_string().len().max(3);
        let gutter_width = char_width * digits as f32 + px(GUTTER_PAD_LEFT + GUTTER_PAD_RIGHT);
        let text_left = bounds.left() + gutter_width + px(TEXT_PAD_LEFT);
        let text_width = (bounds.size.width - gutter_width - px(TEXT_PAD_LEFT)).max(px(0.));
        let wrap_cols = settings.word_wrap.then(|| {
            (((text_width - px(WRAP_PAD_RIGHT)) / char_width).floor() as usize).max(MIN_WRAP_COLS)
        });
        let lh = f32::from(line_height) as f64;
        let height = f32::from(bounds.size.height) as f64;

        // Scroll bookkeeping, then plan which rows are visible. Both need the
        // view mutably (the wrap cache).
        let (plan, max_scroll) = self.view.update(cx, |view, _| {
            let tab_width = view.state.tab_width;
            let head = view.state.selections().primary().head;
            let mut rows = Rows::new(
                &view.state.doc,
                tab_width,
                wrap_cols,
                lh,
                &mut view.wrap_cache,
            );
            if view.pending_scroll != 0. {
                view.scroll.y = rows.advance(view.scroll.y, view.pending_scroll);
                view.pending_scroll = 0.;
            }
            if let Some(mode) = view.autoscroll.take() {
                let top = rows.cursor_row_top(head);
                let margin = (lh * 3.).min(((height - lh) / 2.).max(0.));
                match mode {
                    Autoscroll::Center => view.scroll.y = rows.advance(top, -height * 0.4),
                    Autoscroll::Fit => {
                        let d = rows.distance(view.scroll.y, top, height * 4.);
                        if d < margin {
                            view.scroll.y = rows.advance(top, -margin);
                        } else if d + lh > height - margin {
                            view.scroll.y = rows.advance(top, -(height - margin - lh));
                        }
                    }
                }
                if wrap_cols.is_none() {
                    let (line, col) = view.state.doc.line_col(head);
                    let vcol = visual_col(view.state.doc.line(line), col, tab_width);
                    let x = char_width * vcol as f32;
                    if x < view.scroll.x + char_width * 2. {
                        view.scroll.x = (x - char_width * 8.).max(px(0.));
                    } else if x > view.scroll.x + text_width - char_width * 3. {
                        view.scroll.x = x - text_width + char_width * 8.;
                    }
                }
            }
            let max_scroll = rows.max_scroll(height);
            view.scroll.y = view.scroll.y.clamp(0., max_scroll);
            if wrap_cols.is_some() {
                view.scroll.x = px(0.);
            }
            view.scroll.x = view.scroll.x.max(px(0.));

            // Rows from the scroll position down to the bottom edge. Tops are
            // f64 relative to the viewport so a row deep inside a huge wrapped
            // line still lands exactly.
            let first_visible_col = (view.scroll.x / char_width) as usize;
            let visible_cols = (text_width / char_width) as usize + 2;
            let (mut line, offset) = rows.split(view.scroll.y);
            let mut top = -offset;
            let mut plan = Vec::new();
            while line < len_lines && top < height {
                let line_len = view.state.doc.line_len(line);
                if wrap_cols.is_some() {
                    let starts = rows.rows(line);
                    for (r, &start) in starts.iter().enumerate() {
                        if top + lh > 0. {
                            let (_, end) = row_range(&starts, r, line_len);
                            plan.push(PlannedRow {
                                line,
                                start_col: start,
                                end_col: end,
                                top: bounds.top() + px(top as f32),
                                first: r == 0,
                                last: r + 1 == starts.len(),
                                windowed: false,
                            });
                        }
                        top += lh;
                        if top >= height {
                            break;
                        }
                    }
                } else {
                    let (start_col, end_col, windowed) = if line_len > LONG_LINE {
                        let s = first_visible_col.saturating_sub(256).min(line_len);
                        (
                            s,
                            (first_visible_col + visible_cols + 256).min(line_len),
                            true,
                        )
                    } else {
                        (0, line_len, false)
                    };
                    plan.push(PlannedRow {
                        line,
                        start_col,
                        end_col,
                        top: bounds.top() + px(top as f32),
                        first: true,
                        last: true,
                        windowed,
                    });
                    top += lh;
                }
                line += 1;
            }
            (plan, max_scroll)
        });

        let focused = self.view.read(cx).focus_handle.is_focused(window);
        let view = self.view.read(cx);
        let state = &view.state;
        let doc = &state.doc;
        let scroll = view.scroll;
        let tab_width = state.tab_width;
        let rope = doc.rope();

        // Syntax spans for the visible bytes only.
        let row_pos = |r: &PlannedRow, col: usize| doc.line_start(r.line) + col;
        let (start_byte, end_byte) = match (plan.first(), plan.last()) {
            (Some(first), Some(last)) => (
                rope.char_to_byte(row_pos(first, first.start_col)),
                rope.char_to_byte(row_pos(last, last.end_col)),
            ),
            _ => (0, 0),
        };
        let end_byte = end_byte.min(start_byte + MAX_HIGHLIGHT_BYTES);
        let spans = view
            .syntax()
            .map(|s| s.highlight(rope, start_byte..end_byte))
            .unwrap_or_default();

        let mut lines = Vec::with_capacity(plan.len());
        let mut rows: Vec<RowLayout> = Vec::with_capacity(plan.len());
        let mut span_ix = 0;
        for r in &plan {
            let line_start = doc.line_start(r.line);
            let slice = doc.slice(line_start + r.start_col..line_start + r.end_col);
            let mut byte = rope.char_to_byte(line_start + r.start_col);
            let mut display = String::with_capacity(slice.len_bytes() + 8);
            let mut boundaries: Vec<usize> = Vec::with_capacity(slice.len_chars() + 1);
            let mut runs: Vec<TextRun> = Vec::new();
            // Wrapped rows lay tabs out from their own start.
            let mut vcol = if r.windowed { r.start_col } else { 0 };
            for ch in slice.chars() {
                boundaries.push(display.len());
                let before = display.len();
                if ch == '\t' {
                    let w = tab_width - vcol % tab_width;
                    display.extend(std::iter::repeat_n(' ', w));
                    vcol += w;
                } else if ch.is_control() {
                    display.push('\u{fffd}');
                    vcol += 1;
                } else {
                    display.push(ch);
                    vcol += cell_width(ch, vcol, tab_width).max(1);
                }
                while span_ix < spans.len() && spans[span_ix].range.end <= byte {
                    span_ix += 1;
                }
                let color = spans
                    .get(span_ix)
                    .filter(|s| s.range.start <= byte)
                    .and_then(|s| palette.syntax(s.highlight))
                    .unwrap_or(palette.text);
                push_run(&mut runs, display.len() - before, color, &mono);
                byte += ch.len_utf8();
            }
            boundaries.push(display.len());
            let shaped =
                text_system.shape_line(SharedString::from(display), font_size, &runs, None);
            let offset_x = if r.windowed {
                char_width * r.start_col as f32
            } else {
                px(0.)
            };
            let char_x: Vec<Pixels> = boundaries
                .iter()
                .map(|b| offset_x + shaped.x_for_index(*b))
                .collect();
            lines.push((point(text_left - scroll.x + offset_x, r.top), shaped));
            rows.push(RowLayout {
                line: r.line,
                start_col: r.start_col,
                end_col: r.end_col,
                top: r.top,
                char_x,
                first: r.first,
                last: r.last,
                windowed: r.windowed,
            });
        }

        let to_window = |x: Pixels, row: &RowLayout| point(text_left - scroll.x + x, row.top);

        // Backgrounds: current line, search matches, selections.
        let mut backgrounds = Vec::new();
        let full_width = bounds.size.width;
        let cursor_lines: Vec<usize> = state
            .selections()
            .iter()
            .filter(|s| s.is_empty())
            .map(|s| doc.char_to_line(s.head))
            .collect();
        for row in &rows {
            if cursor_lines.binary_search(&row.line).is_ok() {
                backgrounds.push(fill(
                    Bounds::new(point(bounds.left(), row.top), size(full_width, line_height)),
                    palette.current_line,
                ));
            }
        }
        if let (Some(matcher), Some(first), Some(last)) = (&view.search, rows.first(), rows.last())
        {
            let start = doc.line_start(first.line) + first.start_col;
            let end = doc.line_start(last.line) + last.end_col;
            for m in matcher.find_all(rope, start..end, 2000) {
                let color = if view.active_match.as_ref() == Some(&m) {
                    palette.find_match_active
                } else {
                    palette.find_match
                };
                push_range_quads(
                    &mut backgrounds,
                    doc,
                    m.clone(),
                    &rows,
                    &to_window,
                    line_height,
                    char_width,
                    color,
                    false,
                );
            }
        }
        let selection_color = if focused {
            palette.selection
        } else {
            palette.selection_inactive
        };
        for sel in state.selections().iter().filter(|s| !s.is_empty()) {
            push_range_quads(
                &mut backgrounds,
                doc,
                sel.range(),
                &rows,
                &to_window,
                line_height,
                char_width,
                selection_color,
                true,
            );
        }

        // The Cmd-hovered word, underlined like a link.
        let mut link = Vec::new();
        if let Some(range) = view.hover_link.clone() {
            push_range_quads(
                &mut link,
                doc,
                range,
                &rows,
                &to_window,
                line_height,
                char_width,
                palette.accent,
                false,
            );
            for quad in &mut link {
                quad.bounds.origin.y += line_height - px(3.);
                quad.bounds.size.height = px(1.);
                quad.corner_radii = Default::default();
            }
        }
        let link_hovered = view.hover_link.is_some();

        let snapshot = LayoutSnapshot {
            bounds,
            text_left,
            line_height,
            char_width,
            scroll_x: scroll.x,
            tab_width,
            wrap_cols,
            rows,
        };

        // Cursors.
        let mut cursors = Vec::new();
        if focused && view.cursor_visible {
            for sel in state.selections().iter() {
                let (line, col) = doc.line_col(sel.head);
                if let Some(row) = snapshot.row_for(line, col) {
                    let p = to_window(row.x_for_col(col, char_width), row);
                    cursors.push(fill(
                        Bounds::new(
                            point(p.x - px(0.5), p.y + px(1.)),
                            size(px(2.), line_height - px(2.)),
                        ),
                        palette.cursor,
                    ));
                }
            }
        }

        // Gutter numbers on each line's first row; cursor lines are brighter.
        let head_lines: Vec<usize> = state
            .selections()
            .iter()
            .map(|s| doc.char_to_line(s.head))
            .collect();
        let mut gutter = Vec::with_capacity(snapshot.rows.len());
        for row in snapshot.rows.iter().filter(|r| r.first) {
            let color = if head_lines.binary_search(&row.line).is_ok() {
                palette.gutter_active_text
            } else {
                palette.gutter_text
            };
            let label: SharedString = (row.line + 1).to_string().into();
            let run = TextRun {
                len: label.len(),
                font: mono.clone(),
                color,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let shaped = text_system.shape_line(label, font_size, &[run], None);
            let x = bounds.left() + gutter_width - px(GUTTER_PAD_RIGHT) - shaped.width;
            gutter.push((point(x, row.top), shaped));
        }

        // Overlay scrollbar while scrolling, like macOS. With word wrap the
        // total height is estimated from the visible rows.
        let scrollbar = view
            .scrolled_at
            .filter(|t| t.elapsed() < SCROLLBAR_VISIBLE_FOR)
            .and_then(|_| {
                let visible_lines = plan
                    .last()
                    .zip(plan.first())
                    .map(|(l, f)| l.line - f.line + 1)
                    .unwrap_or(1);
                let rows_per_line = plan.len().max(1) as f64 / visible_lines as f64;
                let content = lh * len_lines as f64 * rows_per_line;
                if content <= height || max_scroll <= 0. {
                    return None;
                }
                let track = bounds.size.height - px(4.);
                let thumb = (track * (height / content) as f32).max(px(24.));
                let ratio = (scroll.y / max_scroll) as f32;
                let y = bounds.top() + px(2.) + (track - thumb) * ratio;
                Some(
                    fill(
                        Bounds::new(point(bounds.right() - px(9.), y), size(px(6.), thumb)),
                        palette.text_tertiary.opacity(0.6),
                    )
                    .corner_radii(px(3.)),
                )
            });

        let notice = view.notice.clone().map(|text| {
            let run = TextRun {
                len: text.len(),
                font: font(theme::fonts(cx).ui.clone()),
                color: palette.text_secondary,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let shaped = text_system.shape_line(text, px(13.), &[run], None);
            let origin = point(
                bounds.left() + (bounds.size.width - shaped.width) / 2.,
                bounds.top() + bounds.size.height * 0.4,
            );
            (origin, shaped)
        });

        let text_bounds = Bounds::from_corners(
            point(bounds.left() + gutter_width, bounds.top()),
            bounds.bottom_right(),
        );
        self.view.update(cx, |view, _| view.layout = Some(snapshot));

        PrepaintState {
            hitbox,
            text_bounds,
            backgrounds,
            lines,
            gutter,
            cursors,
            link,
            link_hovered,
            scrollbar,
            notice,
            line_height,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        prepaint: &mut PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let palette: Palette = theme::palette(cx).clone();
        let focus_handle = self.view.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.view.clone()),
            cx,
        );
        let cursor = if prepaint.link_hovered {
            CursorStyle::PointingHand
        } else {
            CursorStyle::IBeam
        };
        window.set_cursor_style(cursor, &prepaint.hitbox);
        self.register_mouse_listeners(&prepaint.hitbox, prepaint.line_height, window);

        window.paint_quad(fill(bounds, palette.editor_bg));
        let line_height = prepaint.line_height;
        let backgrounds = std::mem::take(&mut prepaint.backgrounds);
        let lines = std::mem::take(&mut prepaint.lines);
        let cursors = std::mem::take(&mut prepaint.cursors);
        let link = std::mem::take(&mut prepaint.link);
        window.with_content_mask(
            Some(ContentMask {
                bounds: prepaint.text_bounds,
            }),
            |window| {
                for quad in backgrounds {
                    window.paint_quad(quad);
                }
                for (origin, line) in &lines {
                    let _ = line.paint(*origin, line_height, TextAlign::Left, None, window, cx);
                }
                for quad in link {
                    window.paint_quad(quad);
                }
                for cursor in cursors {
                    window.paint_quad(cursor);
                }
            },
        );
        for (origin, line) in &prepaint.gutter {
            let _ = line.paint(*origin, line_height, TextAlign::Left, None, window, cx);
        }
        if let Some((origin, line)) = &prepaint.notice {
            let _ = line.paint(*origin, line_height, TextAlign::Left, None, window, cx);
        }
        if let Some(scrollbar) = prepaint.scrollbar.take() {
            window.paint_quad(scrollbar);
        }
    }
}

impl EditorElement {
    fn register_mouse_listeners(&self, hitbox: &Hitbox, line_height: Pixels, window: &mut Window) {
        let view = self.view.clone();
        let hb = hitbox.clone();
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble || !hb.is_hovered(window) {
                return;
            }
            // Control-click is a right click on macOS.
            let secondary_click = event.button == MouseButton::Right
                || (cfg!(target_os = "macos")
                    && event.button == MouseButton::Left
                    && event.modifiers.control);
            if secondary_click {
                view.update(cx, |view, cx| view.context_menu(event.position, window, cx));
                cx.stop_propagation();
                return;
            }
            if event.button == MouseButton::Left {
                view.update(cx, |view, cx| {
                    view.mouse_down(
                        event.position,
                        event.click_count,
                        event.modifiers,
                        window,
                        cx,
                    )
                });
                cx.stop_propagation();
            }
        });
        let view = self.view.clone();
        let hb = hitbox.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            if event.pressed_button.is_none() {
                // Cmd over a word: show it as a link to its definition.
                let link = (event.modifiers.secondary() && hb.is_hovered(window))
                    .then(|| view.read(cx).link_at(event.position))
                    .flatten();
                if link.is_some() || view.read(cx).hover_link.is_some() {
                    view.update(cx, |view, cx| view.set_hover_link(link, cx));
                }
                return;
            }
            if event.pressed_button == Some(MouseButton::Left) && view.read(cx).is_dragging() {
                view.update(cx, |view, cx| view.mouse_drag(event.position, cx));
            }
        });
        let view = self.view.clone();
        let hb = hitbox.clone();
        window.on_modifiers_changed(move |event, window, cx| {
            let link = (event.modifiers.secondary() && hb.is_hovered(window))
                .then(|| view.read(cx).link_at(window.mouse_position()))
                .flatten();
            view.update(cx, |view, cx| view.set_hover_link(link, cx));
        });
        let view = self.view.clone();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, _window, cx| {
            if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
                view.update(cx, |view, cx| view.mouse_up(cx));
            }
        });
        let view = self.view.clone();
        let hb = hitbox.clone();
        window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble || !hb.is_hovered(window) {
                return;
            }
            let delta = event.delta.pixel_delta(line_height);
            view.update(cx, |view, cx| {
                view.scroll_by(delta, cx);
                view.scrolled_at = Some(Instant::now());
                // Repaint once more after the overlay scrollbar should fade.
                cx.spawn(async move |view, cx| {
                    cx.background_executor().timer(SCROLLBAR_VISIBLE_FOR).await;
                    view.update(cx, |_, cx| cx.notify()).ok();
                })
                .detach();
            });
            cx.stop_propagation();
        });
    }
}

fn push_run(runs: &mut Vec<TextRun>, len: usize, color: Hsla, font: &gpui::Font) {
    if len == 0 {
        return;
    }
    if let Some(last) = runs.last_mut()
        && last.color == color
    {
        last.len += len;
        return;
    }
    runs.push(TextRun {
        len,
        font: font.clone(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    });
}

/// Highlight quads for a char range, one per visible row it touches.
#[allow(clippy::too_many_arguments)]
fn push_range_quads(
    out: &mut Vec<PaintQuad>,
    doc: &Document,
    range: std::ops::Range<usize>,
    rows: &[RowLayout],
    to_window: &dyn Fn(Pixels, &RowLayout) -> Point<Pixels>,
    line_height: Pixels,
    char_width: Pixels,
    color: Hsla,
    show_newlines: bool,
) {
    let (start_line, start_col) = doc.line_col(range.start);
    let (end_line, end_col) = doc.line_col(range.end);
    for row in rows {
        if row.line < start_line || row.line > end_line {
            continue;
        }
        let (row_start, row_end) = if row.windowed {
            (0, doc.line_len(row.line))
        } else {
            (row.start_col, row.end_col)
        };
        let from = if row.line == start_line {
            start_col.max(row_start)
        } else {
            row_start
        };
        let to = if row.line == end_line {
            end_col.min(row_end)
        } else {
            row_end
        };
        // The line break is shown as a sliver after the line's last row.
        let newline = show_newlines && row.last && row.line < end_line;
        if from > to {
            continue;
        }
        let x1 = row.x_for_col(from, char_width);
        let mut x2 = row.x_for_col(to, char_width);
        if newline {
            x2 += char_width * 0.6;
        }
        if x2 <= x1 {
            continue;
        }
        let origin = to_window(x1, row);
        out.push(fill(Bounds::new(origin, size(x2 - x1, line_height)), color).corner_radii(px(2.)));
    }
}

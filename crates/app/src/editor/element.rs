//! Paints the editor: only the visible lines are shaped, so cost per frame is
//! the same for a 10-line file and a 10-million-line file.

use std::time::{Duration, Instant};

use editor_text::EditorState;
use editor_text::movement::{char_col_at_visual, char_width as cell_width, visual_col};
use gpui::{
    App, Bounds, ContentMask, CursorStyle, DispatchPhase, Element, ElementId, ElementInputHandler,
    Entity, GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId, IntoElement,
    LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point,
    ScrollWheelEvent, ShapedLine, SharedString, Style, TextAlign, TextRun, Window, fill, font,
    point, px, relative, size,
};

use super::{Autoscroll, EditorView};
use crate::theme::{self, Palette};

pub const FONT_SIZE: f32 = 13.;
pub const LINE_HEIGHT: f32 = 20.;
const GUTTER_PAD_LEFT: f32 = 14.;
const GUTTER_PAD_RIGHT: f32 = 12.;
const TEXT_PAD_LEFT: f32 = 4.;
/// Lines longer than this are shaped only around the visible columns.
const LONG_LINE: usize = 4096;
/// Highlighting stops after this many bytes of visible text.
const MAX_HIGHLIGHT_BYTES: usize = 512 * 1024;
const SCROLLBAR_VISIBLE_FOR: Duration = Duration::from_millis(1200);

/// Geometry of the last painted frame, used for mouse hit-testing and IME.
#[derive(Clone)]
pub struct LayoutSnapshot {
    pub bounds: Bounds<Pixels>,
    /// Window x of column 0 when not scrolled horizontally.
    pub text_left: Pixels,
    pub line_height: Pixels,
    pub char_width: Pixels,
    pub scroll: Point<Pixels>,
    pub tab_width: usize,
    pub lines: Vec<VisibleLine>,
}

#[derive(Clone)]
pub struct VisibleLine {
    pub line: usize,
    /// First char column shaped (non-zero only for very long lines).
    pub start_col: usize,
    /// x of each char boundary from `start_col`, relative to column 0.
    pub char_x: Vec<Pixels>,
}

pub struct Hit {
    pub pos: usize,
    pub line: usize,
    pub visual_col: usize,
}

impl LayoutSnapshot {
    pub fn hit(&self, p: Point<Pixels>, state: &EditorState) -> Hit {
        let doc = &state.doc;
        let y = p.y - self.bounds.top() + self.scroll.y;
        let last_line = doc.len_lines().saturating_sub(1);
        let line = if y < px(0.) { 0 } else { ((y / self.line_height) as usize).min(last_line) };
        let x = (p.x - self.text_left + self.scroll.x).max(px(0.));
        let visual_col = (x / self.char_width).round().max(0.) as usize;
        let col = match self.lines.iter().find(|l| l.line == line) {
            Some(vl) if !vl.char_x.is_empty() => vl.start_col + nearest(&vl.char_x, x),
            _ => char_col_at_visual(doc.line(line), visual_col, self.tab_width).0,
        };
        Hit { pos: doc.line_start(line) + col.min(doc.line_len(line)), line, visual_col }
    }

    pub fn point_for_pos(&self, pos: usize, state: &EditorState) -> Option<Point<Pixels>> {
        let (line, col) = state.doc.line_col(pos);
        let vl = self.lines.iter().find(|l| l.line == line)?;
        let x = *vl.char_x.get(col.checked_sub(vl.start_col)?)?;
        Some(point(
            self.text_left + x - self.scroll.x,
            self.bounds.top() + self.line_height * line as f32 - self.scroll.y,
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
    if (xs[ix] - x) < (x - xs[ix - 1]) { ix } else { ix - 1 }
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
    scrollbar: Option<PaintQuad>,
    notice: Option<(Point<Pixels>, ShapedLine)>,
    line_height: Pixels,
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
        let mono = font(theme::fonts(cx).mono.clone());
        let font_size = px(FONT_SIZE);
        let line_height = px(LINE_HEIGHT);
        let text_system = window.text_system().clone();
        let font_id = text_system.resolve_font(&mono);
        let char_width =
            text_system.advance(font_id, font_size, 'm').map(|s| s.width).unwrap_or(px(7.8));
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);

        // Scroll bookkeeping needs the view mutably; do it first.
        let len_lines = self.view.read(cx).state.doc.len_lines();
        let digits = len_lines.to_string().len().max(3);
        let gutter_width = char_width * digits as f32 + px(GUTTER_PAD_LEFT + GUTTER_PAD_RIGHT);
        let text_left = bounds.left() + gutter_width + px(TEXT_PAD_LEFT);
        let text_width = (bounds.size.width - gutter_width - px(TEXT_PAD_LEFT)).max(px(0.));
        let total_height = line_height * len_lines as f32;
        let max_scroll_y = (total_height - bounds.size.height * 0.5).max(px(0.));
        self.view.update(cx, |view, _| {
            if let Some(mode) = view.autoscroll.take() {
                let state = &view.state;
                let head = state.selections().primary().head;
                let (line, col) = state.doc.line_col(head);
                let vcol = visual_col(state.doc.line(line), col, state.tab_width);
                let y = line_height * line as f32;
                let x = char_width * vcol as f32;
                let margin = line_height * 3.;
                let height = bounds.size.height;
                match mode {
                    Autoscroll::Center => {
                        view.scroll.y = y - height * 0.4;
                    }
                    Autoscroll::Fit => {
                        if y < view.scroll.y + margin {
                            view.scroll.y = y - margin;
                        } else if y + line_height > view.scroll.y + height - margin {
                            view.scroll.y = y + line_height - height + margin;
                        }
                    }
                }
                if x < view.scroll.x + char_width * 2. {
                    view.scroll.x = (x - char_width * 8.).max(px(0.));
                } else if x > view.scroll.x + text_width - char_width * 3. {
                    view.scroll.x = x - text_width + char_width * 8.;
                }
            }
            view.scroll.y = view.scroll.y.clamp(px(0.), max_scroll_y);
            view.scroll.x = view.scroll.x.max(px(0.));
        });

        let focused = self.view.read(cx).focus_handle.is_focused(window);
        let view = self.view.read(cx);
        let state = &view.state;
        let doc = &state.doc;
        let scroll = view.scroll;
        let tab_width = state.tab_width;

        let first_line = ((scroll.y / line_height) as usize).min(len_lines.saturating_sub(1));
        let visible_count = (bounds.size.height / line_height).ceil() as usize + 1;
        let last_line = (first_line + visible_count).min(len_lines);
        let line_top = |line: usize| bounds.top() + line_height * line as f32 - scroll.y;
        let first_visible_col = (scroll.x / char_width) as usize;
        let visible_cols = (text_width / char_width) as usize + 2;

        // Syntax spans for the visible bytes only.
        let rope = doc.rope();
        let start_byte = rope.line_to_byte(first_line);
        let end_byte = if last_line >= len_lines { rope.len_bytes() } else { rope.line_to_byte(last_line) };
        let end_byte = end_byte.min(start_byte + MAX_HIGHLIGHT_BYTES);
        let spans = view.syntax().map(|s| s.highlight(rope, start_byte..end_byte)).unwrap_or_default();

        let mut lines = Vec::with_capacity(last_line - first_line);
        let mut visible = Vec::with_capacity(last_line - first_line);
        let mut span_ix = 0;
        let mut max_width = px(0.);
        for line in first_line..last_line {
            let line_start = doc.line_start(line);
            let line_len = doc.line_len(line);
            let (start_col, end_col) = if line_len > LONG_LINE {
                let s = first_visible_col.saturating_sub(256).min(line_len);
                (s, (first_visible_col + visible_cols + 256).min(line_len))
            } else {
                (0, line_len)
            };
            let slice = doc.slice(line_start + start_col..line_start + end_col);
            let mut byte = rope.char_to_byte(line_start + start_col);
            let mut display = String::with_capacity(slice.len_bytes() + 8);
            let mut boundaries: Vec<usize> = Vec::with_capacity(slice.len_chars() + 1);
            let mut runs: Vec<TextRun> = Vec::new();
            let mut vcol = start_col;
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
            let shaped = text_system.shape_line(SharedString::from(display), font_size, &runs, None);
            let offset_x = char_width * start_col as f32;
            let char_x: Vec<Pixels> =
                boundaries.iter().map(|b| offset_x + shaped.x_for_index(*b)).collect();
            max_width = max_width.max(offset_x + shaped.width);
            lines.push((point(text_left - scroll.x + offset_x, line_top(line)), shaped));
            visible.push(VisibleLine { line, start_col, char_x });
        }

        let x_at = |line: usize, col: usize| -> Pixels {
            match visible.iter().find(|v| v.line == line) {
                Some(v) => match col.checked_sub(v.start_col).and_then(|c| v.char_x.get(c)) {
                    Some(x) => *x,
                    None => char_width * col as f32,
                },
                None => char_width * col as f32,
            }
        };
        let to_window = |x: Pixels, line: usize| point(text_left - scroll.x + x, line_top(line));

        // Backgrounds: current line, search matches, selections.
        let mut backgrounds = Vec::new();
        let full_width = bounds.size.width;
        for sel in state.selections().iter() {
            if sel.is_empty() {
                let line = doc.char_to_line(sel.head);
                if (first_line..last_line).contains(&line) {
                    backgrounds.push(fill(
                        Bounds::new(point(bounds.left(), line_top(line)), size(full_width, line_height)),
                        palette.current_line,
                    ));
                }
            }
        }
        if let Some(matcher) = &view.search {
            let start = doc.line_start(first_line);
            let end = doc.line_start_after(last_line.saturating_sub(1).max(first_line));
            for m in matcher.find_all(rope, start..end, 2000) {
                let color = if view.active_match.as_ref() == Some(&m) {
                    palette.find_match_active
                } else {
                    palette.find_match
                };
                push_range_quads(&mut backgrounds, doc, m.clone(), first_line, last_line, &x_at, &to_window, line_height, char_width, color, false);
            }
        }
        let selection_color = if focused { palette.selection } else { palette.selection_inactive };
        for sel in state.selections().iter().filter(|s| !s.is_empty()) {
            push_range_quads(&mut backgrounds, doc, sel.range(), first_line, last_line, &x_at, &to_window, line_height, char_width, selection_color, true);
        }

        // Cursors.
        let mut cursors = Vec::new();
        if focused && view.cursor_visible {
            for sel in state.selections().iter() {
                let (line, col) = doc.line_col(sel.head);
                if (first_line..last_line).contains(&line) {
                    let p = to_window(x_at(line, col), line);
                    cursors.push(fill(
                        Bounds::new(point(p.x - px(0.5), p.y + px(1.)), size(px(2.), line_height - px(2.))),
                        palette.cursor,
                    ));
                }
            }
        }

        // Gutter numbers; the cursor lines are brighter.
        let cursor_lines: Vec<usize> = state.selections().iter().map(|s| doc.char_to_line(s.head)).collect();
        let mut gutter = Vec::with_capacity(last_line - first_line);
        for line in first_line..last_line {
            let color = if cursor_lines.binary_search(&line).is_ok() {
                palette.gutter_active_text
            } else {
                palette.gutter_text
            };
            let label: SharedString = (line + 1).to_string().into();
            let run = TextRun { len: label.len(), font: mono.clone(), color, background_color: None, underline: None, strikethrough: None };
            let shaped = text_system.shape_line(label, font_size, &[run], None);
            let x = bounds.left() + gutter_width - px(GUTTER_PAD_RIGHT) - shaped.width;
            gutter.push((point(x, line_top(line)), shaped));
        }

        // Overlay scrollbar while scrolling, like macOS.
        let scrollbar = view.scrolled_at.filter(|t| t.elapsed() < SCROLLBAR_VISIBLE_FOR).and_then(|_| {
            let content = total_height + bounds.size.height * 0.5;
            if content <= bounds.size.height {
                return None;
            }
            let track = bounds.size.height - px(4.);
            let thumb = (track * (bounds.size.height / content)).max(px(24.));
            let ratio = if max_scroll_y > px(0.) { scroll.y / max_scroll_y } else { 0. };
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

        let snapshot = LayoutSnapshot {
            bounds,
            text_left,
            line_height,
            char_width,
            scroll,
            tab_width,
            lines: visible,
        };
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
        window.handle_input(&focus_handle, ElementInputHandler::new(bounds, self.view.clone()), cx);
        window.set_cursor_style(CursorStyle::IBeam, &prepaint.hitbox);
        self.register_mouse_listeners(&prepaint.hitbox, window);

        window.paint_quad(fill(bounds, palette.editor_bg));
        let line_height = prepaint.line_height;
        let backgrounds = std::mem::take(&mut prepaint.backgrounds);
        let lines = std::mem::take(&mut prepaint.lines);
        let cursors = std::mem::take(&mut prepaint.cursors);
        window.with_content_mask(Some(ContentMask { bounds: prepaint.text_bounds }), |window| {
            for quad in backgrounds {
                window.paint_quad(quad);
            }
            for (origin, line) in &lines {
                let _ = line.paint(*origin, line_height, TextAlign::Left, None, window, cx);
            }
            for cursor in cursors {
                window.paint_quad(cursor);
            }
        });
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
    fn register_mouse_listeners(&self, hitbox: &Hitbox, window: &mut Window) {
        let view = self.view.clone();
        let hb = hitbox.clone();
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble || event.button != MouseButton::Left || !hb.is_hovered(window) {
                return;
            }
            view.update(cx, |view, cx| {
                view.mouse_down(event.position, event.click_count, event.modifiers, window, cx)
            });
            cx.stop_propagation();
        });
        let view = self.view.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _window, cx| {
            if phase != DispatchPhase::Bubble || event.pressed_button != Some(MouseButton::Left) {
                return;
            }
            if view.read(cx).is_dragging() {
                view.update(cx, |view, cx| view.mouse_drag(event.position, cx));
            }
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
            let delta = event.delta.pixel_delta(px(LINE_HEIGHT));
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

#[allow(clippy::too_many_arguments)]
fn push_range_quads(
    out: &mut Vec<PaintQuad>,
    doc: &editor_text::Document,
    range: std::ops::Range<usize>,
    first_line: usize,
    last_line: usize,
    x_at: &dyn Fn(usize, usize) -> Pixels,
    to_window: &dyn Fn(Pixels, usize) -> Point<Pixels>,
    line_height: Pixels,
    char_width: Pixels,
    color: Hsla,
    show_newlines: bool,
) {
    let (start_line, start_col) = doc.line_col(range.start);
    let (end_line, end_col) = doc.line_col(range.end);
    for line in start_line.max(first_line)..=end_line.min(last_line.saturating_sub(1)) {
        let from = if line == start_line { start_col } else { 0 };
        let to = if line == end_line { end_col } else { doc.line_len(line) };
        let x1 = x_at(line, from);
        let mut x2 = x_at(line, to);
        if show_newlines && line < end_line {
            x2 += char_width * 0.6;
        }
        if x2 <= x1 {
            continue;
        }
        let origin = to_window(x1, line);
        out.push(fill(Bounds::new(origin, size(x2 - x1, line_height)), color).corner_radii(px(2.)));
    }
}

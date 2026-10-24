//! A fuzzy list overlay, like the command palette, for choosing among
//! symbols and locations (Cmd+R, Cmd+Shift+R, ambiguous go to definition).

use gpui::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, HighlightStyle,
    InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Render, ScrollStrategy,
    SharedString, StatefulInteractiveElement, Styled, StyledText, Subscription,
    UniformListScrollHandle, Window, div, prelude::FluentBuilder, px, svg, uniform_list,
};
use gpui_component::input::{Input, InputEvent, InputState};
use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use nucleo::{Matcher, Utf32Str};

use me_syntax::SymbolKind;

use crate::file_icons::FileIcon;
use crate::theme;

const ROW_HEIGHT: f32 = 34.;
const VISIBLE_ROWS: usize = 12;

#[derive(Clone)]
pub struct PickerItem {
    pub label: SharedString,
    /// Muted text after the label (a path, a kind).
    pub detail: SharedString,
    pub icon: Option<FileIcon>,
    /// A symbol kind badge, instead of a file icon.
    pub symbol: Option<SymbolKind>,
    /// Indentation level (nested symbols).
    pub depth: usize,
}

pub enum PickerEvent {
    /// The index of the chosen item, in the list given to `show`.
    Picked(usize),
    Dismiss,
}

pub struct Picker {
    input: Entity<InputState>,
    items: Vec<PickerItem>,
    matches: Vec<(usize, Vec<u32>)>,
    selected: usize,
    scroll: UniformListScrollHandle,
    matcher: Matcher,
    /// With an empty query, select this item first (the current symbol).
    initial: usize,
    _subscription: Subscription,
}

impl EventEmitter<PickerEvent> for Picker {}

impl Focusable for Picker {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}

impl Picker {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx));
        let subscription =
            cx.subscribe_in(
                &input,
                window,
                |this, _, event: &InputEvent, _, cx| match event {
                    InputEvent::Change => this.filter(cx),
                    InputEvent::PressEnter { .. } => this.confirm(cx),
                    _ => {}
                },
            );
        Self {
            input,
            items: Vec::new(),
            matches: Vec::new(),
            selected: 0,
            scroll: UniformListScrollHandle::new(),
            matcher: Matcher::new(nucleo::Config::DEFAULT),
            initial: 0,
            _subscription: subscription,
        }
    }

    pub fn show(
        &mut self,
        placeholder: &str,
        items: Vec<PickerItem>,
        initial: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.items = items;
        self.initial = initial;
        let placeholder = SharedString::from(placeholder.to_string());
        self.input.update(cx, |input, cx| {
            input.set_placeholder(placeholder, window, cx);
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        self.filter(cx);
    }

    fn filter(&mut self, cx: &mut Context<Self>) {
        let query = self.input.read(cx).value().to_string();
        let query = query.trim();
        if query.is_empty() {
            self.matches = (0..self.items.len()).map(|ix| (ix, Vec::new())).collect();
            self.selected = self.initial.min(self.items.len().saturating_sub(1));
        } else {
            let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
            let mut buf = Vec::new();
            let mut scored: Vec<(u32, usize, Vec<u32>)> = Vec::new();
            for (ix, item) in self.items.iter().enumerate() {
                let mut indices = Vec::new();
                let haystack = Utf32Str::new(&item.label, &mut buf);
                if let Some(score) = pattern.indices(haystack, &mut self.matcher, &mut indices) {
                    indices.sort_unstable();
                    indices.dedup();
                    scored.push((score, ix, indices));
                }
            }
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            self.matches = scored.into_iter().map(|(_, ix, i)| (ix, i)).collect();
            self.selected = 0;
        }
        self.scroll
            .scroll_to_item(self.selected, ScrollStrategy::Center);
        cx.notify();
    }

    fn select(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.matches.is_empty() {
            return;
        }
        let n = self.matches.len() as isize;
        self.selected = (self.selected as isize + delta).rem_euclid(n) as usize;
        self.scroll
            .scroll_to_item(self.selected, ScrollStrategy::Top);
        cx.notify();
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        if let Some((ix, _)) = self.matches.get(self.selected) {
            cx.emit(PickerEvent::Picked(*ix));
        }
    }

    fn on_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        let ctrl = ks.modifiers.control;
        match ks.key.as_str() {
            "up" => self.select(-1, cx),
            "down" => self.select(1, cx),
            "p" if ctrl => self.select(-1, cx),
            "n" if ctrl => self.select(1, cx),
            "pageup" => self.select(-(VISIBLE_ROWS as isize), cx),
            "pagedown" => self.select(VISIBLE_ROWS as isize, cx),
            "escape" => cx.emit(PickerEvent::Dismiss),
            _ => return,
        }
        cx.stop_propagation();
    }
}

impl Render for Picker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::palette(cx).clone();
        let rows: Vec<(PickerItem, Vec<u32>)> = self
            .matches
            .iter()
            .map(|(ix, indices)| (self.items[*ix].clone(), indices.clone()))
            .collect();
        let selected = self.selected;
        let list_height = px(ROW_HEIGHT * rows.len().clamp(1, VISIBLE_ROWS) as f32 + 8.);
        div()
            .id("picker")
            .key_context("Picker")
            .capture_key_down(cx.listener(Self::on_key))
            .w(px(640.))
            .flex()
            .flex_col()
            .bg(palette.overlay_bg)
            .border_1()
            .border_color(palette.border)
            .rounded(px(12.))
            .shadow_lg()
            .overflow_hidden()
            .child(
                div()
                    .p(px(10.))
                    .border_b_1()
                    .border_color(palette.border)
                    .child(Input::new(&self.input).appearance(false).text_size(px(17.))),
            )
            .child(
                div()
                    .h(list_height)
                    .py(px(4.))
                    .when(rows.is_empty(), |el| {
                        el.child(
                            div()
                                .px(px(16.))
                                .py(px(8.))
                                .text_size(px(13.))
                                .text_color(palette.text_tertiary)
                                .child("Nothing matches"),
                        )
                    })
                    .when(!rows.is_empty(), |el| {
                        el.child(
                            uniform_list(
                                "picker-results",
                                rows.len(),
                                cx.processor(move |_, range: std::ops::Range<usize>, _, cx| {
                                    range
                                        .map(|ix| {
                                            let (item, indices) = &rows[ix];
                                            render_row(
                                                ix,
                                                item,
                                                indices,
                                                ix == selected,
                                                &palette,
                                                cx,
                                            )
                                        })
                                        .collect()
                                }),
                            )
                            .track_scroll(&self.scroll)
                            .h(list_height - px(8.)),
                        )
                    }),
            )
    }
}

fn render_row(
    ix: usize,
    item: &PickerItem,
    indices: &[u32],
    selected: bool,
    palette: &theme::Palette,
    cx: &mut Context<Picker>,
) -> gpui::AnyElement {
    let highlights: Vec<(std::ops::Range<usize>, HighlightStyle)> = item
        .label
        .char_indices()
        .enumerate()
        .filter(|(ci, _)| indices.binary_search(&(*ci as u32)).is_ok())
        .map(|(_, (bi, ch))| {
            (
                bi..bi + ch.len_utf8(),
                HighlightStyle {
                    color: Some(palette.accent),
                    font_weight: Some(gpui::FontWeight::SEMIBOLD),
                    ..Default::default()
                },
            )
        })
        .collect();
    let row = div()
        .id("row")
        .w_full()
        .h(px(ROW_HEIGHT))
        .pl(px(10. + item.depth as f32 * 16.))
        .pr(px(10.))
        .flex()
        .items_center()
        .gap(px(8.))
        .rounded(px(7.))
        .text_size(px(14.))
        .text_color(palette.text)
        .when(selected, |el| el.bg(palette.row_selected))
        .when(!selected, |el| el.hover(|s| s.bg(palette.hover)))
        .when_some(item.symbol, |el, kind| {
            el.child(symbol_badge(kind, palette))
        })
        .when_some(item.icon.clone(), |el, icon| {
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
                .flex_none()
                .max_w(px(360.))
                .overflow_hidden()
                .whitespace_nowrap()
                .child(StyledText::new(item.label.clone()).with_highlights(highlights)),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_size(px(12.5))
                .text_color(palette.text_tertiary)
                .child(item.detail.clone()),
        );
    div()
        .id(ix)
        .w_full()
        .px(px(6.))
        .child(row)
        .on_click(cx.listener(move |this, _, _, cx| {
            this.selected = ix;
            this.confirm(cx);
        }))
        .into_any_element()
}

/// A small colored letter for a symbol kind, like VS Code's symbol icons.
pub fn symbol_badge(kind: SymbolKind, palette: &theme::Palette) -> gpui::Div {
    let glyph = match kind {
        SymbolKind::Function => "ƒ",
        SymbolKind::Method => "m",
        SymbolKind::Class => "C",
        SymbolKind::Interface => "I",
        SymbolKind::Module => "M",
        SymbolKind::Macro => "!",
        SymbolKind::Constant => "π",
        SymbolKind::Type => "T",
        SymbolKind::Impl => "i",
        SymbolKind::Heading => "#",
        SymbolKind::Other => "·",
    };
    let color = palette.symbol(kind);
    div()
        .flex_none()
        .size(px(16.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .bg(color.opacity(0.16))
        .text_color(color)
        .text_size(px(11.))
        .font_weight(gpui::FontWeight::BOLD)
        .child(glyph)
}

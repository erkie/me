//! Cmd+F: find (and replace) in the current file.

use gpui::{
    App, AppContext, ClickEvent, Context, Entity, EventEmitter, InteractiveElement, IntoElement,
    KeyDownEvent, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled,
    Subscription, Window, div, prelude::FluentBuilder, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use me_text::SearchQuery;

use crate::theme;

pub enum FindBarEvent {
    /// The query text or options changed (`None` when empty or invalid).
    QueryChanged(Option<SearchQuery>),
    Next(SearchQuery),
    Previous(SearchQuery),
    /// Alt+Enter: turn every match into a cursor.
    SelectAll(SearchQuery),
    Replace(SearchQuery, String),
    ReplaceAll(SearchQuery, String),
    Close,
}

pub struct FindBar {
    input: Entity<InputState>,
    replace: Entity<InputState>,
    case_sensitive: bool,
    whole_word: bool,
    regex: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<FindBarEvent> for FindBar {}

impl FindBar {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Find"));
        let replace = cx.new(|cx| InputState::new(window, cx).placeholder("Replace"));
        let subscriptions =
            vec![
                cx.subscribe_in(&input, window, |this, _, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.emit_query(cx);
                    }
                }),
            ];
        Self {
            input,
            replace,
            case_sensitive: false,
            whole_word: false,
            regex: false,
            _subscriptions: subscriptions,
        }
    }

    pub fn query(&self, cx: &App) -> Option<SearchQuery> {
        let pattern = self.input.read(cx).value().to_string();
        if pattern.is_empty() {
            return None;
        }
        let query = SearchQuery {
            pattern,
            regex: self.regex,
            case_sensitive: self.case_sensitive,
            whole_word: self.whole_word,
        };
        query.compile().ok().map(|_| query)
    }

    fn emit_query(&mut self, cx: &mut Context<Self>) {
        cx.emit(FindBarEvent::QueryChanged(self.query(cx)));
        cx.notify();
    }

    pub fn show(&mut self, seed: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(seed) = seed.filter(|s| !s.is_empty()) {
            self.input
                .update(cx, |input, cx| input.set_value(seed, window, cx));
        }
        self.input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        self.emit_query(cx);
    }

    pub fn set_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| {
            input.set_value(text.to_string(), window, cx)
        });
        self.emit_query(cx);
    }

    fn on_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        match ks.key.as_str() {
            "enter" => {
                let Some(query) = self.query(cx) else { return };
                if ks.modifiers.alt {
                    cx.emit(FindBarEvent::SelectAll(query));
                } else if ks.modifiers.shift {
                    cx.emit(FindBarEvent::Previous(query));
                } else {
                    cx.emit(FindBarEvent::Next(query));
                }
            }
            "escape" => cx.emit(FindBarEvent::Close),
            _ => return,
        }
        cx.stop_propagation();
    }

    fn toggle(&mut self, which: u8, cx: &mut Context<Self>) {
        match which {
            0 => self.case_sensitive = !self.case_sensitive,
            1 => self.whole_word = !self.whole_word,
            _ => self.regex = !self.regex,
        }
        self.emit_query(cx);
    }
}

pub fn toggle_button(
    id: &'static str,
    label: &'static str,
    tooltip: &'static str,
    active: bool,
    palette: &theme::Palette,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let _ = tooltip;
    div()
        .id(id)
        .h(px(22.))
        .min_w(px(24.))
        .px(px(5.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(5.))
        .text_size(px(12.))
        .when(active, |el| {
            el.bg(palette.accent.opacity(0.18))
                .text_color(palette.accent)
        })
        .when(!active, |el| {
            el.text_color(palette.text_secondary)
                .hover(|s| s.bg(palette.hover))
        })
        .child(SharedString::from(label))
        .on_click(on_click)
}

pub fn text_button(
    id: &'static str,
    label: &'static str,
    palette: &theme::Palette,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .h(px(24.))
        .px(px(10.))
        .flex()
        .items_center()
        .rounded(px(6.))
        .border_1()
        .border_color(palette.border)
        .bg(palette.editor_bg)
        .text_size(px(12.))
        .text_color(palette.text)
        .hover(|s| s.bg(palette.hover))
        .child(label)
        .on_click(on_click)
}

impl Render for FindBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::palette(cx).clone();
        let input_box = |input: &Entity<InputState>| {
            div()
                .flex_1()
                .h(px(28.))
                .px(px(4.))
                .rounded(px(6.))
                .border_1()
                .border_color(palette.border)
                .bg(palette.editor_bg)
                .child(Input::new(input).appearance(false))
        };
        div()
            .id("find-bar")
            .key_context("FindBar")
            .capture_key_down(cx.listener(Self::on_key))
            .flex()
            .flex_col()
            .gap(px(6.))
            .px(px(10.))
            .py(px(7.))
            .bg(palette.tab_bar_bg)
            .border_b_1()
            .border_color(palette.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(input_box(&self.input))
                    .child(toggle_button(
                        "case",
                        "Aa",
                        "Match case",
                        self.case_sensitive,
                        &palette,
                        cx.listener(|this, _, _, cx| this.toggle(0, cx)),
                    ))
                    .child(toggle_button(
                        "word",
                        "W",
                        "Whole word",
                        self.whole_word,
                        &palette,
                        cx.listener(|this, _, _, cx| this.toggle(1, cx)),
                    ))
                    .child(toggle_button(
                        "regex",
                        ".*",
                        "Regular expression",
                        self.regex,
                        &palette,
                        cx.listener(|this, _, _, cx| this.toggle(2, cx)),
                    ))
                    .child(text_button(
                        "prev",
                        "‹",
                        &palette,
                        cx.listener(|this, _, _, cx| {
                            if let Some(q) = this.query(cx) {
                                cx.emit(FindBarEvent::Previous(q));
                            }
                        }),
                    ))
                    .child(text_button(
                        "next",
                        "›",
                        &palette,
                        cx.listener(|this, _, _, cx| {
                            if let Some(q) = this.query(cx) {
                                cx.emit(FindBarEvent::Next(q));
                            }
                        }),
                    ))
                    .child(text_button(
                        "all",
                        "All",
                        &palette,
                        cx.listener(|this, _, _, cx| {
                            if let Some(q) = this.query(cx) {
                                cx.emit(FindBarEvent::SelectAll(q));
                            }
                        }),
                    )),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(input_box(&self.replace))
                    .child(text_button(
                        "replace",
                        "Replace",
                        &palette,
                        cx.listener(|this, _, _, cx| {
                            if let Some(q) = this.query(cx) {
                                let with = this.replace.read(cx).value().to_string();
                                cx.emit(FindBarEvent::Replace(q, with));
                            }
                        }),
                    ))
                    .child(text_button(
                        "replace-all",
                        "Replace All",
                        &palette,
                        cx.listener(|this, _, _, cx| {
                            if let Some(q) = this.query(cx) {
                                let with = this.replace.read(cx).value().to_string();
                                cx.emit(FindBarEvent::ReplaceAll(q, with));
                            }
                        }),
                    )),
            )
    }
}

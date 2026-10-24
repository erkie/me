//! Ctrl+G: jump to a line (and optionally `:column`) in the current file.

use gpui::{
    AppContext, Context, Entity, EventEmitter, InteractiveElement, IntoElement, KeyDownEvent,
    ParentElement, Render, SharedString, Styled, Window, div, px,
};
use gpui_component::input::{Input, InputState};

use crate::theme;

pub enum GoToLineEvent {
    Go { line: u32, column: Option<u32> },
    Dismiss,
}

pub struct GoToLinePrompt {
    input: Entity<InputState>,
    hint: SharedString,
}

impl EventEmitter<GoToLineEvent> for GoToLinePrompt {}

impl GoToLinePrompt {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Line number, or line:column"));
        Self {
            input,
            hint: SharedString::default(),
        }
    }

    pub fn show(
        &mut self,
        current: usize,
        total: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.hint = format!("Line {current} of {total}").into();
        self.input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    fn on_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "enter" => {
                let value = self.input.read(cx).value().to_string();
                let mut parts = value.trim().trim_start_matches(':').splitn(2, ':');
                let line = parts.next().and_then(|l| l.trim().parse().ok());
                let column = parts.next().and_then(|c| c.trim().parse().ok());
                match line {
                    Some(line) => cx.emit(GoToLineEvent::Go { line, column }),
                    None => cx.emit(GoToLineEvent::Dismiss),
                }
            }
            "escape" => cx.emit(GoToLineEvent::Dismiss),
            _ => return,
        }
        cx.stop_propagation();
    }
}

impl Render for GoToLinePrompt {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::palette(cx).clone();
        div()
            .id("go-to-line")
            .capture_key_down(cx.listener(Self::on_key))
            .w(px(340.))
            .p(px(10.))
            .flex()
            .flex_col()
            .gap(px(6.))
            .bg(palette.overlay_bg)
            .border_1()
            .border_color(palette.border)
            .rounded(px(12.))
            .shadow_lg()
            .child(Input::new(&self.input).appearance(false).text_size(px(16.)))
            .child(
                div()
                    .px(px(4.))
                    .text_size(px(12.))
                    .text_color(palette.text_tertiary)
                    .child(self.hint.clone()),
            )
    }
}

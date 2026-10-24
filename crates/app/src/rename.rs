//! F2: the box for a symbol's new name, under the symbol.

use gpui::{
    AppContext, Context, Entity, EventEmitter, InteractiveElement, IntoElement, KeyDownEvent,
    ParentElement, Render, Styled, Subscription, Window, div, px,
};
use gpui_component::input::{Input, InputEvent, InputState};

use crate::theme;

pub enum RenameEvent {
    Rename(String),
    /// Escape.
    Cancel,
    /// Focus went elsewhere (a click outside).
    Blur,
}

pub struct RenamePrompt {
    input: Entity<InputState>,
    _subscription: Subscription,
}

impl EventEmitter<RenameEvent> for RenamePrompt {}

impl RenamePrompt {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx));
        let subscription = cx.subscribe_in(&input, window, |_, _, event: &InputEvent, _, cx| {
            if let InputEvent::Blur = event {
                cx.emit(RenameEvent::Blur);
            }
        });
        Self {
            input,
            _subscription: subscription,
        }
    }

    /// Opens with `name`, all selected.
    pub fn show(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| {
            input.set_value(name.to_string(), window, cx);
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        cx.notify();
    }

    fn on_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "enter" => {
                let name = self.input.read(cx).value().trim().to_string();
                cx.emit(RenameEvent::Rename(name));
            }
            "escape" => cx.emit(RenameEvent::Cancel),
            _ => return,
        }
        cx.stop_propagation();
    }
}

impl Render for RenamePrompt {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::palette(cx).clone();
        let mono = theme::fonts(cx).mono.clone();
        let size = crate::settings::get(cx).font_size;
        div()
            .id("rename")
            .capture_key_down(cx.listener(Self::on_key))
            .w(px(300.))
            .p(px(6.))
            .flex()
            .flex_col()
            .gap(px(4.))
            .bg(palette.overlay_bg)
            .border_1()
            .border_color(palette.border)
            .rounded(px(8.))
            .shadow_lg()
            .child(
                Input::new(&self.input)
                    .appearance(false)
                    .font_family(mono)
                    .text_size(px(size)),
            )
            .child(
                div()
                    .px(px(4.))
                    .text_size(px(11.5))
                    .text_color(palette.text_tertiary)
                    .child("Enter to rename, Escape to cancel"),
            )
    }
}

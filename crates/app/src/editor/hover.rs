//! The hover popover: the problems under the pointer (or cursor) and what
//! the language server says about the symbol there, as markdown.

use std::ops::Range;

use gpui::{
    AnyElement, App, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement, Pixels,
    SharedString, StatefulInteractiveElement, StyleRefinement, Styled, div, prelude::FluentBuilder,
    px, relative, rems,
};
use gpui_base::{TextView, TextViewStyle};

use super::EditorView;
use super::diagnostics::{Diagnostic, Severity};
use crate::picker::Badge;
use crate::theme::{self, Palette};

pub const MAX_WIDTH: f32 = 560.;
pub const MAX_HEIGHT: f32 = 380.;

/// A popover being shown (or waiting for the server's answer).
pub struct HoverState {
    pub id: u64,
    /// The chars it's about: the word, the server's range or the problem.
    pub range: Range<usize>,
    pub diagnostics: Vec<Diagnostic>,
    /// The server's markdown.
    pub info: Option<SharedString>,
    /// Opened from the keyboard: stays until the cursor moves or Escape.
    pub sticky: bool,
    /// The pointer is over the popover.
    pub inside: bool,
}

impl HoverState {
    pub fn is_visible(&self) -> bool {
        !self.diagnostics.is_empty() || self.info.is_some()
    }
}

/// What a language server said about a symbol.
#[derive(Clone, Debug)]
pub struct HoverInfo {
    pub markdown: SharedString,
    /// The chars it applies to, if the server said.
    pub range: Option<Range<usize>>,
}

pub fn severity_color(severity: Severity, palette: &Palette) -> gpui::Hsla {
    match severity {
        Severity::Error => palette.error,
        Severity::Warning => palette.warning,
        Severity::Info | Severity::Hint => palette.info,
    }
}

/// A small colored mark for a severity, like the symbol badges.
pub fn severity_badge(severity: Severity, palette: &Palette) -> Badge {
    let glyph = match severity {
        Severity::Error => "✕",
        Severity::Warning => "!",
        Severity::Info => "i",
        Severity::Hint => "?",
    };
    Badge {
        glyph,
        color: severity_color(severity, palette),
    }
}

fn hover_text_style(palette: &Palette) -> TextViewStyle {
    TextViewStyle::default()
        .with_foreground(palette.text)
        .with_muted_foreground(palette.text_secondary)
        .with_link(palette.accent)
        .with_selection(palette.selection)
        .with_code_background(palette.code_bg)
        .with_border(palette.border)
        .with_dark(palette.is_dark)
        .with_paragraph_gap(rems(0.5))
        .with_heading(|_| {
            StyleRefinement::default()
                .text_size(px(13.5))
                .font_weight(FontWeight::SEMIBOLD)
        })
        .with_code_block(
            StyleRefinement::default()
                .border_0()
                .rounded(px(6.))
                .px(px(10.))
                .py(px(6.))
                .text_size(px(12.5)),
        )
        .with_inline_code(gpui::HighlightStyle {
            background_color: Some(palette.code_bg),
            ..Default::default()
        })
}

/// The popover's content. Hovering it keeps it open; leaving it closes it.
pub fn popover(
    view: &Entity<EditorView>,
    hover: &HoverState,
    max_height: Pixels,
    cx: &App,
) -> AnyElement {
    let palette = theme::palette(cx).clone();
    let mono = theme::fonts(cx).mono.clone();
    let view = view.clone();
    let diagnostics = hover.diagnostics.iter().map(|d| {
        let mut detail = d.source.clone().unwrap_or_default();
        if let Some(code) = &d.code {
            detail = if detail.is_empty() {
                code.clone()
            } else {
                format!("{detail}({code})")
            };
        }
        div()
            .flex()
            .gap(px(8.))
            .child(
                div()
                    .pt(px(2.))
                    .child(severity_badge(d.severity, &palette).element()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .child(d.message.clone())
                    .when(!detail.is_empty(), |el| {
                        el.child(
                            div()
                                .text_size(px(12.))
                                .text_color(palette.text_tertiary)
                                .child(detail),
                        )
                    }),
            )
    });
    let info = hover.info.clone().map(|markdown| {
        TextView::markdown(SharedString::from(format!("hover-{}", hover.id)), markdown)
            .style(hover_text_style(&palette))
            .shared_code_block_highlighter(crate::markdown::shared_code_highlighter(&palette))
            // No language label in the corner: signatures are long and the
            // label would sit on top of them.
            .code_block_actions(|_, _, _| div())
            .selectable(true)
            .on_link_click(|url, _, _, cx| cx.open_url(url))
    });
    let has_diagnostics = !hover.diagnostics.is_empty();
    div()
        .id(("hover", hover.id as usize))
        .occlude()
        .on_hover(move |hovered, _, cx| {
            view.update(cx, |view, cx| view.popover_hovered(*hovered, cx))
        })
        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
        .max_w(px(MAX_WIDTH))
        .max_h(max_height)
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .bg(palette.overlay_bg)
        .border_1()
        .border_color(palette.border)
        .rounded(px(8.))
        .shadow_lg()
        .text_size(px(13.))
        .line_height(relative(1.5))
        .text_color(palette.text)
        .cursor_default()
        .when(has_diagnostics, |el| {
            el.child(
                div()
                    .px(px(12.))
                    .py(px(8.))
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .font_family(mono.clone())
                    .text_size(px(12.5))
                    .children(diagnostics),
            )
        })
        .when_some(info, |el, info| {
            el.when(has_diagnostics, |el| {
                el.child(div().h(px(1.)).flex_none().bg(palette.border))
            })
            .child(div().px(px(12.)).py(px(8.)).child(info))
        })
        .into_any_element()
}

//! Rendered markdown for a tab. The source lives in an [`EditorView`] so
//! flipping to source (Cmd+Shift+V) shows the same, live text.

use std::path::PathBuf;
use std::time::Duration;

use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Subscription, Task,
    Window, div, px, rems,
};
use gpui_component::highlighter::HighlightTheme;
use gpui_component::text::{TextView, TextViewStyle};

use crate::editor::{EditorEvent, EditorView};
use crate::theme;

/// Re-render at most this often while the source changes.
const REFRESH_DELAY: Duration = Duration::from_millis(120);

pub enum MarkdownEvent {
    /// A link to a local file was clicked (already resolved to a path).
    OpenPath(PathBuf),
}

pub struct MarkdownPreview {
    focus_handle: FocusHandle,
    editor: Entity<EditorView>,
    text: SharedString,
    revision: u64,
    refresh: Option<Task<()>>,
    _subscription: Subscription,
}

impl EventEmitter<MarkdownEvent> for MarkdownPreview {}

impl Focusable for MarkdownPreview {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// Whether a file should open as rendered markdown.
pub fn is_markdown(path: &std::path::Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        matches!(
            e.to_ascii_lowercase().as_str(),
            "md" | "markdown" | "mdx" | "mdown" | "mkd"
        )
    })
}

impl MarkdownPreview {
    pub fn new(editor: Entity<EditorView>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&editor, |this, _, event: &EditorEvent, cx| {
            if matches!(event, EditorEvent::Changed | EditorEvent::Loaded) {
                this.schedule_refresh(cx);
            }
        });
        let (text, revision) = {
            let e = editor.read(cx);
            (SharedString::from(e.text()), e.revision())
        };
        Self {
            focus_handle: cx.focus_handle(),
            editor,
            text,
            revision,
            refresh: None,
            _subscription: subscription,
        }
    }

    fn schedule_refresh(&mut self, cx: &mut Context<Self>) {
        if self.refresh.is_some() {
            return;
        }
        self.refresh = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(REFRESH_DELAY).await;
            this.update(cx, |this, cx| {
                this.refresh = None;
                let editor = this.editor.read(cx);
                if editor.revision() != this.revision || editor.is_loading() {
                    this.revision = editor.revision();
                    this.text = editor.text().into();
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    fn resolve_link(&self, url: &str, cx: &App) -> Option<PathBuf> {
        if url.contains("://") || url.starts_with("mailto:") || url.starts_with('#') {
            return None;
        }
        let path_part = url.split('#').next().unwrap_or(url);
        let decoded = path_part.replace("%20", " ");
        let base = self.editor.read(cx).path()?.parent()?.to_path_buf();
        Some(me_ipc::normalize(&base.join(decoded)))
    }
}

impl Render for MarkdownPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::palette(cx).clone();
        let style = TextViewStyle::default()
            .paragraph_gap(rems(0.9))
            .heading_font_size(|level, _| match level {
                1 => px(28.),
                2 => px(22.),
                3 => px(18.),
                4 => px(16.),
                _ => px(14.),
            });
        let style = TextViewStyle {
            highlight_theme: if palette.is_dark {
                HighlightTheme::default_dark().clone()
            } else {
                HighlightTheme::default_light().clone()
            },
            is_dark: palette.is_dark,
            ..style
        };
        let this = cx.weak_entity();
        let view = TextView::markdown(
            SharedString::from(format!("markdown-{}", cx.entity_id())),
            self.text.clone(),
        )
        .style(style)
        .selectable(true)
        .on_link_click(move |url, _, window, cx| {
            let url = url.clone();
            let this = this.clone();
            window.defer(cx, move |_, cx| {
                let Some(this) = this.upgrade() else { return };
                let path = this.read(cx).resolve_link(&url, cx);
                match path {
                    Some(path) => this.update(cx, |_, cx| cx.emit(MarkdownEvent::OpenPath(path))),
                    None => cx.open_url(&url),
                }
            });
        });

        div()
            .id("markdown-scroll")
            .key_context("MarkdownPreview")
            .track_focus(&self.focus_handle)
            .size_full()
            .overflow_y_scroll()
            .bg(palette.editor_bg)
            .text_color(palette.text)
            .text_size(px(15.))
            .line_height(rems(1.6))
            .child(
                div().w_full().flex().justify_center().child(
                    div()
                        .w_full()
                        .max_w(px(820.))
                        .px(px(40.))
                        .py(px(32.))
                        .child(view),
                ),
            )
    }
}

//! Rendered markdown for a tab. The source lives in an [`EditorView`] so
//! flipping to source (Cmd+Shift+V) shows the same, live text.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, HighlightStyle,
    ImageSource, InteractiveElement, IntoElement, ParentElement, Render, SharedString, SharedUri,
    StatefulInteractiveElement, StyleRefinement, Styled, Subscription, Task, Window, div, px,
    relative, rems,
};
use gpui_base::text::{CodeBlock, MarkdownExtensions};
use gpui_base::{TextView, TextViewStyle};
use me_syntax::{Syntax, language_by_name};
use me_text::Rope;

use crate::editor::{EditorEvent, EditorView};
use crate::theme::Palette;
use crate::{settings, theme};

type CodeHighlighter = Arc<dyn Fn(&CodeBlock) -> Vec<(Range<usize>, HighlightStyle)> + Send + Sync>;

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
    /// Kept across frames: code blocks reuse their highlights only while the
    /// highlighter stays the same `Arc`. Rebuilt when the theme flips.
    highlighter: Option<(bool, CodeHighlighter)>,
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
            "md" | "markdown" | "mdx" | "mdc" | "mdown" | "mkd"
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
            highlighter: None,
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

/// Highlights fenced code with the editor's own grammars and colors.
fn code_highlighter(palette: Palette) -> CodeHighlighter {
    Arc::new(move |block: &CodeBlock| {
        let Some(language) = block
            .lang()
            .and_then(|lang| language_by_name(&lang))
            .and_then(|def| def.load())
        else {
            return Vec::new();
        };
        let rope = Rope::from_str(&block.code());
        let mut syntax = Syntax::new(language);
        syntax.parse(&rope, Duration::from_millis(100));
        syntax
            .highlight(&rope, 0..rope.len_bytes())
            .into_iter()
            .filter_map(|span| {
                let color = palette.syntax(span.highlight)?;
                Some((
                    span.range,
                    HighlightStyle {
                        color: Some(color),
                        ..Default::default()
                    },
                ))
            })
            .collect()
    })
}

/// Loads relative and absolute image paths from disk, next to the file.
fn image_source(base: Option<PathBuf>) -> impl Fn(&SharedUri) -> ImageSource + Send + Sync {
    move |uri: &SharedUri| {
        let url = uri.as_ref();
        if url.contains("://") && !url.starts_with("file://") || url.starts_with("data:") {
            return uri.clone().into();
        }
        let path = url
            .strip_prefix("file://")
            .unwrap_or(url)
            .replace("%20", " ");
        let path = Path::new(&path);
        match &base {
            Some(base) if path.is_relative() => ImageSource::from(base.join(path)),
            _ => ImageSource::from(path.to_path_buf()),
        }
    }
}

/// Mermaid fences become diagrams. Rebuilt each frame; the parser revision
/// keeps the parsed document until the theme (which the diagrams bake in)
/// changes.
fn markdown_extensions(palette: &Palette) -> MarkdownExtensions {
    let p = palette.clone();
    MarkdownExtensions::default()
        .block_parser(move |node, _| crate::mermaid::parse(node, &p))
        .block_renderer(crate::mermaid::NODE_NAME, |node, _, cx| {
            crate::mermaid::element(node, cx)
        })
        .parser_revision(if palette.is_dark { 2 } else { 1 })
}

fn text_style(palette: &Palette, body: f32) -> TextViewStyle {
    let heading_scale = |level: u8| match level {
        1 => 2.,
        2 => 1.5,
        3 => 1.25,
        4 => 1.0625,
        5 => 1.,
        _ => 0.9,
    };
    TextViewStyle::default()
        .with_foreground(palette.text)
        .with_muted_foreground(palette.text_secondary)
        .with_link(palette.accent)
        .with_selection(palette.selection)
        .with_code_background(palette.code_bg)
        .with_border(palette.border)
        .with_dark(palette.is_dark)
        .with_paragraph_gap(rems(body / 16.))
        .with_heading(move |level| {
            let style =
                StyleRefinement::default().text_size(px((body * heading_scale(level)).round()));
            if level == 1 {
                style.font_weight(FontWeight::BOLD)
            } else {
                style
            }
        })
        .with_code_block(
            StyleRefinement::default()
                .border_0()
                .rounded(px(8.))
                .px(px(18.))
                .text_size(px((body * 0.84).round())),
        )
        .with_inline_code(HighlightStyle {
            background_color: Some(palette.code_bg),
            ..Default::default()
        })
        .with_table(StyleRefinement::default().rounded(px(8.)))
        .with_table_head(
            StyleRefinement::default()
                .bg(palette.code_bg)
                .text_color(palette.text)
                .font_weight(FontWeight::SEMIBOLD),
        )
}

impl Render for MarkdownPreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::palette(cx).clone();
        // Everything scales with the body size, which follows Cmd+/Cmd-.
        let body = settings::get(cx).markdown_size();
        let highlighter = match &self.highlighter {
            Some((dark, h)) if *dark == palette.is_dark => h.clone(),
            _ => {
                let h = code_highlighter(palette.clone());
                self.highlighter = Some((palette.is_dark, h.clone()));
                h
            }
        };
        let base = self
            .editor
            .read(cx)
            .path()
            .and_then(Path::parent)
            .map(Path::to_path_buf);
        let this = cx.weak_entity();
        let view = TextView::markdown(
            SharedString::from(format!("markdown-{}", cx.entity_id())),
            self.text.clone(),
        )
        .style(text_style(&palette, body))
        .shared_code_block_highlighter(highlighter)
        .markdown_extensions(markdown_extensions(&palette))
        .image_source(image_source(base))
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
            .text_size(px(body))
            .line_height(relative(1.65))
            .child(
                div().w_full().flex().justify_center().child(
                    // About 75 characters per line.
                    div()
                        .w_full()
                        .max_w(px(body * 46. + 96.))
                        .px(px(48.))
                        .pt(px(36.))
                        .pb(px(64.))
                        .child(view),
                ),
            )
    }
}

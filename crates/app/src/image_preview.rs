//! Image files open as a picture. SVGs are text too: Cmd+Shift+V flips to
//! the source, and the picture follows edits to it.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    App, Context, Entity, FocusHandle, Focusable, Image, ImageFormat, ImageSource,
    InteractiveElement, IntoElement, ObjectFit, ParentElement, Render, SharedString, Styled,
    StyledImage, Subscription, Task, Window, div, img, px,
};

use crate::editor::{EditorEvent, EditorView, human_size};
use crate::theme;

const REFRESH_DELAY: Duration = Duration::from_millis(150);

/// Image formats shown as pictures.
pub fn is_image(path: &Path) -> bool {
    format_for(path).is_some()
}

pub fn is_svg(path: &Path) -> bool {
    matches!(format_for(path), Some(ImageFormat::Svg))
}

fn format_for(path: &Path) -> Option<ImageFormat> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => ImageFormat::Png,
        "jpg" | "jpeg" => ImageFormat::Jpeg,
        "gif" => ImageFormat::Gif,
        "webp" => ImageFormat::Webp,
        "bmp" => ImageFormat::Bmp,
        "tif" | "tiff" => ImageFormat::Tiff,
        "ico" => ImageFormat::Ico,
        "svg" => ImageFormat::Svg,
        _ => return None,
    })
}

pub struct ImagePreview {
    focus_handle: FocusHandle,
    path: PathBuf,
    format: ImageFormat,
    /// The SVG's source, rendered live. `None` for raster images.
    editor: Option<Entity<EditorView>>,
    image: Option<Arc<Image>>,
    /// "PNG · 512 × 512 · 48.3 KB", for the status bar.
    pub info: SharedString,
    error: Option<SharedString>,
    revision: u64,
    refresh: Option<Task<()>>,
    _subscription: Option<Subscription>,
}

impl Focusable for ImagePreview {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ImagePreview {
    /// A raster image, read from disk in the background.
    pub fn raster(path: PathBuf, cx: &mut Context<Self>) -> Self {
        let format = format_for(&path).unwrap_or(ImageFormat::Png);
        let mut this = Self::empty(path, format, None, cx);
        this.reload(cx);
        this
    }

    /// An SVG backed by its source in `editor`.
    pub fn svg(path: PathBuf, editor: Entity<EditorView>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&editor, |this, _, event: &EditorEvent, cx| {
            if matches!(event, EditorEvent::Changed | EditorEvent::Loaded) {
                this.schedule_refresh(cx);
            }
        });
        let mut this = Self::empty(path, ImageFormat::Svg, Some(editor), cx);
        this._subscription = Some(subscription);
        this.render_svg(cx);
        this
    }

    fn empty(
        path: PathBuf,
        format: ImageFormat,
        editor: Option<Entity<EditorView>>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            path,
            format,
            editor,
            image: None,
            info: SharedString::default(),
            error: None,
            revision: 0,
            refresh: None,
            _subscription: None,
        }
    }

    /// Re-reads a raster image (the window regained focus, or it opened).
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        if self.editor.is_some() {
            return;
        }
        let path = self.path.clone();
        let format = self.format;
        self.refresh = Some(cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move {
                    let bytes = std::fs::read(&path)?;
                    let dims = image::ImageReader::new(std::io::Cursor::new(&bytes))
                        .with_guessed_format()
                        .ok()
                        .and_then(|r| r.into_dimensions().ok());
                    anyhow::Ok((bytes, dims))
                })
                .await;
            this.update(cx, |this, cx| {
                match loaded {
                    Ok((bytes, dims)) => {
                        let size = bytes.len() as u64;
                        if this.image.as_ref().is_none_or(|i| i.bytes != bytes) {
                            this.image = Some(Arc::new(Image::from_bytes(format, bytes)));
                        }
                        this.info = describe(format, dims, size);
                        this.error = None;
                    }
                    Err(err) => this.error = Some(err.to_string().into()),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn schedule_refresh(&mut self, cx: &mut Context<Self>) {
        if self.refresh.is_some() {
            return;
        }
        self.refresh = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(REFRESH_DELAY).await;
            this.update(cx, |this, cx| {
                this.refresh = None;
                this.render_svg(cx);
            })
            .ok();
        }));
    }

    fn render_svg(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = &self.editor else { return };
        let editor = editor.read(cx);
        if editor.revision() == self.revision && self.image.is_some() {
            return;
        }
        self.revision = editor.revision();
        let text = editor.text();
        let size = text.len() as u64;
        self.info = describe(ImageFormat::Svg, svg_size(&text), size);
        self.image = Some(Arc::new(Image::from_bytes(
            ImageFormat::Svg,
            text.into_bytes(),
        )));
        cx.notify();
    }
}

fn describe(format: ImageFormat, dims: Option<(u32, u32)>, bytes: u64) -> SharedString {
    let name = match format {
        ImageFormat::Png => "PNG",
        ImageFormat::Jpeg => "JPEG",
        ImageFormat::Gif => "GIF",
        ImageFormat::Webp => "WebP",
        ImageFormat::Bmp => "BMP",
        ImageFormat::Tiff => "TIFF",
        ImageFormat::Ico => "ICO",
        ImageFormat::Svg => "SVG",
        _ => "Image",
    };
    match dims {
        Some((w, h)) => format!("{name} · {w} × {h} · {}", human_size(bytes)),
        None => format!("{name} · {}", human_size(bytes)),
    }
    .into()
}

/// The size an SVG declares, from `width`/`height` or its `viewBox`.
fn svg_size(svg: &str) -> Option<(u32, u32)> {
    let start = svg.find("<svg")?;
    let root = &svg[start..start + svg[start..].find('>')?];
    let attr = |name: &str| -> Option<&str> {
        let key = format!(" {name}=\"");
        let i = root.find(&key)? + key.len();
        Some(&root[i..i + root[i..].find('"')?])
    };
    let number = |s: &str| -> Option<f32> { s.trim_end_matches("px").trim().parse().ok() };
    if let (Some(w), Some(h)) = (
        attr("width").and_then(number),
        attr("height").and_then(number),
    ) {
        return Some((w.round() as u32, h.round() as u32));
    }
    let mut parts = attr("viewBox")?
        .split(|c: char| c == ' ' || c == ',')
        .filter(|p| !p.is_empty())
        .filter_map(|p| p.parse::<f32>().ok());
    let (_, _, w, h) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    Some((w.round() as u32, h.round() as u32))
}

impl Render for ImagePreview {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::palette(cx).clone();
        let content = match (&self.image, &self.error) {
            (_, Some(error)) => div()
                .text_color(palette.text_secondary)
                .child(format!("Could not open the image: {error}"))
                .into_any_element(),
            (Some(image), None) => img(ImageSource::Image(image.clone()))
                .max_w_full()
                .max_h_full()
                .object_fit(ObjectFit::ScaleDown)
                .into_any_element(),
            (None, None) => div().into_any_element(),
        };
        div()
            .id("image-preview")
            .key_context("ImagePreview")
            .track_focus(&self.focus_handle)
            .size_full()
            .p(px(32.))
            .flex()
            .items_center()
            .justify_center()
            .overflow_hidden()
            .bg(palette.editor_bg)
            .child(content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_images() {
        assert!(is_image(Path::new("a/logo.PNG")));
        assert!(is_svg(Path::new("icon.svg")));
        assert!(!is_image(Path::new("main.rs")));
    }

    #[test]
    fn svg_sizes() {
        assert_eq!(
            svg_size(r#"<?xml?><svg width="24" height="16px" viewBox="0 0 48 32">"#),
            Some((24, 16))
        );
        assert_eq!(svg_size(r#"<svg viewBox="0 0 512 256">"#), Some((512, 256)));
        assert_eq!(svg_size("<svg>"), None);
    }
}

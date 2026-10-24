//! Image files open as a picture you can paint on: a toolbar of tools,
//! brush sizes and colours above it, zoom down to crisp pixels, undo per
//! stroke, and Cmd+S writes the file back in its own format. Animated
//! images, icons and 16-bit images are shown but not painted on.
//!
//! SVGs are text too: Cmd+Shift+V flips to the source, and the picture
//! follows edits to it.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::Context as _;
use gpui::{
    AnyElement, App, AppContext, ClickEvent, Context, ElementId, Entity, EventEmitter, FocusHandle,
    Focusable, Global, Hsla, Image, ImageFormat, ImageSource, InteractiveElement, IntoElement,
    ObjectFit, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled,
    StyledImage, Subscription, Task, Window, div, img, prelude::FluentBuilder, px, svg,
};
use gpui_component::color_picker::{ColorPicker, ColorPickerEvent, ColorPickerState};
use image::RgbaImage;

use crate::actions::{
    BiggerBrush, Cancel, EllipseTool, EraserTool, EyedropperTool, FillTool, HandTool, LineTool,
    PencilTool, RectangleTool, Redo, Save, SaveAs, SmallerBrush, Undo, ZoomIn, ZoomOut, ZoomToFit,
};
use crate::editor::{EditorEvent, EditorView, human_size, write_atomically};
use crate::paint::{self, Canvas, Color, Meta, Opened};
use crate::theme;

mod canvas;

use canvas::{CanvasElement, Paint};

const REFRESH_DELAY: Duration = Duration::from_millis(150);
const BRUSH_SIZES: [u32; 6] = [1, 2, 4, 8, 16, 32];
/// How big each brush size's dot is in the toolbar.
const BRUSH_DOTS: [f32; 6] = [2., 3., 5., 7., 10., 13.];
const SWATCHES: [Color; 12] = [
    [0, 0, 0, 255],
    [127, 127, 127, 255],
    [195, 195, 195, 255],
    [255, 255, 255, 255],
    [237, 28, 36, 255],
    [255, 127, 39, 255],
    [255, 242, 0, 255],
    [34, 177, 76, 255],
    [0, 162, 232, 255],
    [63, 72, 204, 255],
    [163, 73, 164, 255],
    [255, 174, 201, 255],
];

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

// -------------------------------------------------------------------------
// Tools
// -------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Hand,
    Pencil,
    Eraser,
    Fill,
    Eyedropper,
    Line,
    Rectangle,
    Ellipse,
}

impl Tool {
    const ALL: [Tool; 8] = [
        Tool::Hand,
        Tool::Pencil,
        Tool::Eraser,
        Tool::Fill,
        Tool::Eyedropper,
        Tool::Line,
        Tool::Rectangle,
        Tool::Ellipse,
    ];

    fn icon(self) -> &'static str {
        match self {
            Tool::Hand => "icons/hand.svg",
            Tool::Pencil => "icons/pencil.svg",
            Tool::Eraser => "icons/eraser.svg",
            Tool::Fill => "icons/paint-bucket.svg",
            Tool::Eyedropper => "icons/pipette.svg",
            Tool::Line => "icons/slash.svg",
            Tool::Rectangle => "icons/square.svg",
            Tool::Ellipse => "icons/circle.svg",
        }
    }

    fn uses_brush(self) -> bool {
        matches!(self, Tool::Pencil | Tool::Eraser)
    }

    fn is_shape(self) -> bool {
        matches!(self, Tool::Line | Tool::Rectangle | Tool::Ellipse)
    }
}

/// The tool, colour and brush size, shared by every image.
#[derive(Clone, Copy, Debug)]
struct PaintSettings {
    tool: Tool,
    color: Color,
    size: u32,
}

impl Default for PaintSettings {
    fn default() -> Self {
        Self {
            tool: Tool::Pencil,
            color: [0, 0, 0, 255],
            size: 1,
        }
    }
}

impl Global for PaintSettings {}

fn settings(cx: &mut App) -> &mut PaintSettings {
    cx.default_global::<PaintSettings>()
}

fn to_hsla(c: Color) -> Hsla {
    gpui::Rgba {
        r: c[0] as f32 / 255.,
        g: c[1] as f32 / 255.,
        b: c[2] as f32 / 255.,
        a: c[3] as f32 / 255.,
    }
    .into()
}

fn from_hsla(c: Hsla) -> Color {
    let c = c.to_rgb();
    let byte = |v: f32| (v.clamp(0., 1.) * 255.).round() as u8;
    [byte(c.r), byte(c.g), byte(c.b), byte(c.a)]
}

// -------------------------------------------------------------------------
// The view
// -------------------------------------------------------------------------

pub enum ImageEvent {
    /// The pixels changed: an edit finished, was undone or redone, or the
    /// file was read again.
    Edited,
    Saved,
    /// Saved under another name.
    Moved(PathBuf),
    Message(SharedString),
}

/// Modification time and length of the file as last read or written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp(Option<SystemTime>, u64);

impl Stamp {
    fn of(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        Some(Self(meta.modified().ok(), meta.len()))
    }
}

pub struct ImagePreview {
    focus_handle: FocusHandle,
    path: PathBuf,
    format: ImageFormat,
    /// The SVG's source, rendered live. `None` for raster images.
    editor: Option<Entity<EditorView>>,
    /// What `img` shows: an SVG, or a picture that can't be painted on.
    image: Option<Arc<Image>>,
    /// A raster image being painted on.
    paint: Option<Paint>,
    /// "PNG · 512 × 512 · 48.3 KB", for the status bar.
    pub info: SharedString,
    error: Option<SharedString>,
    disk: Option<Stamp>,
    /// Writing the file: it isn't read back meanwhile.
    saving: bool,
    revision: u64,
    refresh: Option<Task<()>>,
    picker: Option<Entity<ColorPickerState>>,
    /// The colour the picker was last set to or picked.
    picker_color: Option<Color>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ImageEvent> for ImagePreview {}

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
        this.load(None, cx);
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
        this._subscriptions.push(subscription);
        this.render_svg(cx);
        this
    }

    fn empty(
        path: PathBuf,
        format: ImageFormat,
        editor: Option<Entity<EditorView>>,
        cx: &mut Context<Self>,
    ) -> Self {
        // Textures live in every window's atlas until dropped.
        let release = cx.on_release(|this: &mut Self, cx| {
            if let Some(paint) = this.paint.take() {
                paint.tiles.drop_all(cx);
            }
        });
        Self {
            focus_handle: cx.focus_handle(),
            path,
            format,
            editor,
            image: None,
            paint: None,
            info: SharedString::default(),
            error: None,
            disk: None,
            saving: false,
            revision: 0,
            refresh: None,
            picker: None,
            picker_color: None,
            _subscriptions: vec![release],
        }
    }

    /// The status bar: the file, and the pixel under the pointer.
    pub fn status(&self) -> SharedString {
        match self.paint.as_ref().and_then(|p| p.hovered_pixel()) {
            Some((x, y)) => format!("{} · {x}, {y}", self.info).into(),
            None => self.info.clone(),
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.paint.as_ref().is_some_and(|p| p.canvas.is_dirty())
    }

    /// The unsaved pixels and their state id, to keep in the session.
    pub fn unsaved_pixels(&self) -> Option<(u64, Arc<RgbaImage>)> {
        let canvas = &self.paint.as_ref()?.canvas;
        canvas
            .is_dirty()
            .then(|| (canvas.state_id(), canvas.pixels().clone()))
    }

    /// Brings back unsaved pixels from a session (a PNG).
    pub fn restore_unsaved(&mut self, png: Vec<u8>, cx: &mut Context<Self>) {
        self.load(Some(png), cx);
    }

    /// The file was renamed or moved.
    pub fn set_path(&mut self, path: PathBuf) {
        self.path = path;
    }

    /// Re-reads a raster image if it changed on disk (the window regained
    /// focus). Unsaved paint stays.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let busy = self.refresh.is_some() || self.saving;
        let painting = self.paint.as_ref().is_some_and(|p| p.drag.is_some());
        if self.editor.is_some() || busy || painting || self.is_dirty() {
            return;
        }
        self.load(None, cx);
    }

    fn load(&mut self, unsaved: Option<Vec<u8>>, cx: &mut Context<Self>) {
        let path = self.path.clone();
        let format = self.format;
        let known = self.disk.filter(|_| unsaved.is_none());
        self.refresh = Some(cx.spawn(async move |this, cx| {
            let read = cx
                .background_executor()
                .spawn(async move { read_image(&path, format, known, unsaved) })
                .await;
            this.update(cx, |this, cx| {
                this.refresh = None;
                this.loaded(read, cx);
            })
            .ok();
        }));
    }

    fn loaded(&mut self, read: anyhow::Result<Read>, cx: &mut Context<Self>) {
        let read = match read {
            Ok(read) => read,
            Err(err) => {
                self.error = Some(format!("{err:#}").into());
                cx.notify();
                return;
            }
        };
        self.error = None;
        match read.loaded {
            Loaded::Unchanged => return,
            Loaded::Paint {
                pixels,
                meta,
                unsaved,
            } => {
                self.image = None;
                match &mut self.paint {
                    // The file changed on disk: same view, new pixels.
                    Some(paint)
                        if !unsaved
                            && pixels.dimensions() == paint.canvas.pixels().dimensions() =>
                    {
                        paint.canvas.replace(pixels);
                        paint.meta = meta;
                        paint.redraw();
                    }
                    _ => {
                        let canvas = if unsaved {
                            Canvas::unsaved(pixels)
                        } else {
                            Canvas::new(pixels)
                        };
                        if let Some(old) = self.paint.replace(Paint::new(canvas, meta)) {
                            old.tiles.drop_all(cx);
                        }
                    }
                }
                cx.emit(ImageEvent::Edited);
            }
            Loaded::Picture { bytes } => {
                if let Some(old) = self.paint.take() {
                    old.tiles.drop_all(cx);
                }
                self.image = Some(Arc::new(Image::from_bytes(self.format, bytes)));
            }
        }
        self.info = read.info;
        self.disk = read.stamp;
        cx.notify();
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

    // ---------------------------------------------------------------------
    // Saving
    // ---------------------------------------------------------------------

    pub fn save(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<()>> {
        if let Some(editor) = &self.editor {
            return editor.update(cx, |editor, cx| editor.save(window, cx));
        }
        self.save_to(self.path.clone(), cx)
    }

    pub fn save_as(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<()>> {
        if let Some(editor) = &self.editor {
            return editor.update(cx, |editor, cx| editor.save_as(window, cx));
        }
        if self.paint.is_none() {
            return Task::ready(Ok(()));
        }
        let dir = self
            .path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("/"));
        let name = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned());
        let rx = cx.prompt_for_new_path(&dir, name.as_deref());
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = rx.await else {
                return Ok(());
            };
            this.update(cx, |this, cx| this.save_to(path, cx))?.await
        })
    }

    /// Writes the pixels to `path`, in the format its name says.
    fn save_to(&mut self, path: PathBuf, cx: &mut Context<Self>) -> Task<anyhow::Result<()>> {
        let Some(paint) = &mut self.paint else {
            return Task::ready(Ok(()));
        };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let Some(format) = paint::format_for_path(&path).filter(|f| paint::can_write(*f)) else {
            let message =
                format!("Can't save {name}: Me writes PNG, JPEG, GIF, WebP, BMP and TIFF");
            cx.emit(ImageEvent::Message(message.clone().into()));
            return Task::ready(Err(anyhow::anyhow!(message)));
        };
        let pixels = paint.canvas.pixels().clone();
        let meta = paint.meta.clone();
        let state = paint.canvas.state_id();
        let write_path = path.clone();
        self.saving = true;
        cx.spawn(async move |this, cx| {
            let written = cx
                .background_executor()
                .spawn(async move {
                    let bytes = paint::encode(&pixels, &meta, format)?;
                    write_atomically(&write_path, &bytes)?;
                    anyhow::Ok(bytes.len() as u64)
                })
                .await;
            this.update(cx, |this, cx| {
                this.saving = false;
                let len = match &written {
                    Ok(len) => *len,
                    Err(err) => {
                        cx.emit(ImageEvent::Message(
                            format!("Couldn't save {name}: {err:#}").into(),
                        ));
                        return;
                    }
                };
                let moved = path != this.path;
                if moved {
                    this.format = format_for(&path).unwrap_or(this.format);
                    this.path = path.clone();
                }
                this.disk = Stamp::of(&path);
                if let Some(paint) = &mut this.paint {
                    paint.canvas.mark_saved(state);
                    paint.meta.format = format;
                    let (w, h) = paint.canvas.pixels().dimensions();
                    this.info = describe(this.format, Some((w, h)), len);
                }
                if moved {
                    cx.emit(ImageEvent::Moved(path.clone()));
                }
                cx.emit(ImageEvent::Saved);
                cx.notify();
            })?;
            written.map(|_| ())
        })
    }

    // ---------------------------------------------------------------------
    // Painting
    // ---------------------------------------------------------------------

    /// After pixels change mid-drag: re-upload them.
    fn redraw(&mut self, cx: &mut Context<Self>) {
        if let Some(paint) = &mut self.paint {
            paint.redraw();
        }
        cx.notify();
    }

    /// After an edit is finished (or undone): the tab and session hear of it.
    fn edited(&mut self, cx: &mut Context<Self>) {
        self.redraw(cx);
        cx.emit(ImageEvent::Edited);
    }

    fn set_tool(&mut self, tool: Tool, cx: &mut Context<Self>) {
        settings(cx).tool = tool;
        cx.notify();
    }

    fn set_color(&mut self, color: Color, cx: &mut Context<Self>) {
        settings(cx).color = color;
        cx.notify();
    }

    fn step_brush(&mut self, bigger: bool, cx: &mut Context<Self>) {
        let current = settings(cx).size;
        let next = if bigger {
            BRUSH_SIZES.iter().copied().find(|&s| s > current)
        } else {
            BRUSH_SIZES.iter().rev().copied().find(|&s| s < current)
        };
        if let Some(size) = next {
            settings(cx).size = size;
            cx.notify();
        }
    }

    fn undo(&mut self, redo: bool, cx: &mut Context<Self>) {
        let Some(paint) = &mut self.paint else { return };
        paint.drag = None;
        let changed = if redo {
            paint.canvas.redo()
        } else {
            paint.canvas.undo()
        };
        if changed {
            self.edited(cx);
        }
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        let Some(paint) = self.paint.as_mut().filter(|p| p.drag.is_some()) else {
            cx.propagate();
            return;
        };
        paint.drag = None;
        paint.canvas.cancel_stroke();
        self.redraw(cx);
    }

    fn zoom(&mut self, step: Option<bool>, cx: &mut Context<Self>) {
        let Some(paint) = &mut self.paint else { return };
        match step {
            Some(zoom_in) => paint.zoom_step(zoom_in),
            None => paint.zoom = None,
        }
        cx.notify();
    }

    // ---------------------------------------------------------------------
    // Toolbar
    // ---------------------------------------------------------------------

    /// The colour picker, showing the paint colour.
    fn color_picker(
        &mut self,
        color: Color,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<ColorPickerState> {
        let picker = match &self.picker {
            Some(picker) => picker.clone(),
            None => {
                let picker = cx.new(|cx| ColorPickerState::new(window, cx));
                self._subscriptions.push(cx.subscribe(
                    &picker,
                    |this, _, event: &ColorPickerEvent, cx| {
                        if let ColorPickerEvent::Change(Some(hsla)) = event {
                            let color = from_hsla(*hsla);
                            this.picker_color = Some(color);
                            this.set_color(color, cx);
                        }
                    },
                ));
                self.picker = Some(picker.clone());
                picker
            }
        };
        if self.picker_color != Some(color) {
            self.picker_color = Some(color);
            picker.update(cx, |picker, cx| {
                picker.set_value(to_hsla(color), window, cx)
            });
        }
        picker
    }

    fn render_toolbar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let palette = theme::palette(cx).clone();
        let settings = *settings(cx);
        let picker = self.color_picker(settings.color, window, cx);
        let Some(paint) = &self.paint else {
            return div().into_any_element();
        };
        let zoom = paint
            .layout
            .map(|l| format!("{}%", (l.scale * 100.).round() as u32))
            .unwrap_or_default();
        let (can_undo, can_redo) = (paint.canvas.can_undo(), paint.canvas.can_redo());
        let separator = || {
            div()
                .w(px(1.))
                .h(px(18.))
                .mx(px(6.))
                .flex_none()
                .bg(palette.border)
        };

        let tools = Tool::ALL.map(|tool| {
            icon_button(
                ElementId::Name(format!("tool-{tool:?}").into()),
                tool.icon(),
                settings.tool == tool,
                true,
                &palette,
                cx.listener(move |this, _: &ClickEvent, _, cx| this.set_tool(tool, cx)),
            )
        });
        let sizes = BRUSH_SIZES.iter().zip(BRUSH_DOTS).map(|(&size, dot)| {
            let active = settings.size == size;
            div()
                .id(("brush-size", size as usize))
                .size(px(24.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .when(active, |el| el.bg(palette.hover))
                .hover(|s| s.bg(palette.hover))
                .child(div().size(px(dot)).rounded_full().bg(if active {
                    palette.text
                } else {
                    palette.text_secondary
                }))
                .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                    self::settings(cx).size = size;
                    cx.notify();
                }))
        });
        let swatches = SWATCHES.map(|color| {
            let selected = settings.color == color;
            div()
                .id(ElementId::Name(format!("swatch-{color:?}").into()))
                .size(px(16.))
                .flex_none()
                .rounded(px(3.))
                .border_1()
                .border_color(palette.border)
                .bg(to_hsla(color))
                .when(selected, |el| el.border_2().border_color(palette.accent))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.set_color(color, cx)))
        });

        div()
            .id("paint-toolbar")
            .flex_none()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(2.))
            .px(px(8.))
            .py(px(4.))
            .min_h(px(36.))
            .border_b_1()
            .border_color(palette.border)
            .bg(palette.editor_bg)
            .text_size(px(12.))
            .text_color(palette.text_secondary)
            .children(tools)
            .child(separator())
            .children(sizes)
            .child(separator())
            .child(
                div().mr(px(4.)).child(
                    ColorPicker::new(&picker)
                        .featured_colors(SWATCHES.iter().copied().map(to_hsla).collect()),
                ),
            )
            .child(div().flex().items_center().gap(px(3.)).children(swatches))
            .child(div().flex_1())
            .child(icon_button(
                "undo".into(),
                "icons/undo-2.svg",
                false,
                can_undo,
                &palette,
                cx.listener(|this, _: &ClickEvent, _, cx| this.undo(false, cx)),
            ))
            .child(icon_button(
                "redo".into(),
                "icons/redo-2.svg",
                false,
                can_redo,
                &palette,
                cx.listener(|this, _: &ClickEvent, _, cx| this.undo(true, cx)),
            ))
            .child(separator())
            .child(icon_button(
                "zoom-out".into(),
                "icons/zoom-out.svg",
                false,
                true,
                &palette,
                cx.listener(|this, _: &ClickEvent, _, cx| this.zoom(Some(false), cx)),
            ))
            .child(
                // Click: between fitting the view and actual size.
                div()
                    .id("zoom")
                    .min_w(px(44.))
                    .h(px(24.))
                    .px(px(4.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .hover(|s| s.bg(palette.hover).text_color(palette.text))
                    .child(zoom)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        if let Some(paint) = &mut this.paint {
                            paint.zoom = match paint.zoom {
                                None => Some(1.),
                                Some(_) => None,
                            };
                            paint.center = paint.image_center();
                        }
                        cx.notify();
                    })),
            )
            .child(icon_button(
                "zoom-in".into(),
                "icons/zoom-in.svg",
                false,
                true,
                &palette,
                cx.listener(|this, _: &ClickEvent, _, cx| this.zoom(Some(true), cx)),
            ))
            .into_any_element()
    }
}

fn icon_button(
    id: ElementId,
    icon: &'static str,
    active: bool,
    enabled: bool,
    palette: &theme::Palette,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let color = if active {
        palette.text
    } else if enabled {
        palette.text_secondary
    } else {
        palette.text_tertiary
    };
    div()
        .id(id)
        .size(px(26.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .when(active, |el| el.bg(palette.hover))
        .when(enabled, |el| {
            el.hover(|s| s.bg(palette.hover)).on_click(on_click)
        })
        .child(svg().path(icon).size(px(15.)).text_color(color))
}

impl Render for ImagePreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::palette(cx).clone();
        let root = div()
            .id("image-preview")
            .key_context("ImagePreview")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(palette.editor_bg)
            .on_action(cx.listener(|this, _: &Save, window, cx| {
                this.save(window, cx).detach();
            }))
            .on_action(cx.listener(|this, _: &SaveAs, window, cx| {
                this.save_as(window, cx).detach();
            }));

        if self.paint.is_some() && self.error.is_none() {
            let toolbar = self.render_toolbar(window, cx);
            return root
                .flex()
                .flex_col()
                .on_action(cx.listener(|this, _: &HandTool, _, cx| this.set_tool(Tool::Hand, cx)))
                .on_action(
                    cx.listener(|this, _: &PencilTool, _, cx| this.set_tool(Tool::Pencil, cx)),
                )
                .on_action(
                    cx.listener(|this, _: &EraserTool, _, cx| this.set_tool(Tool::Eraser, cx)),
                )
                .on_action(cx.listener(|this, _: &FillTool, _, cx| this.set_tool(Tool::Fill, cx)))
                .on_action(cx.listener(|this, _: &EyedropperTool, _, cx| {
                    this.set_tool(Tool::Eyedropper, cx)
                }))
                .on_action(cx.listener(|this, _: &LineTool, _, cx| this.set_tool(Tool::Line, cx)))
                .on_action(
                    cx.listener(|this, _: &RectangleTool, _, cx| {
                        this.set_tool(Tool::Rectangle, cx)
                    }),
                )
                .on_action(
                    cx.listener(|this, _: &EllipseTool, _, cx| this.set_tool(Tool::Ellipse, cx)),
                )
                .on_action(cx.listener(|this, _: &BiggerBrush, _, cx| this.step_brush(true, cx)))
                .on_action(cx.listener(|this, _: &SmallerBrush, _, cx| this.step_brush(false, cx)))
                .on_action(cx.listener(|this, _: &ZoomIn, _, cx| this.zoom(Some(true), cx)))
                .on_action(cx.listener(|this, _: &ZoomOut, _, cx| this.zoom(Some(false), cx)))
                .on_action(cx.listener(|this, _: &ZoomToFit, _, cx| this.zoom(None, cx)))
                .on_action(cx.listener(|this, _: &Undo, _, cx| this.undo(false, cx)))
                .on_action(cx.listener(|this, _: &Redo, _, cx| this.undo(true, cx)))
                .on_action(cx.listener(|this, _: &Cancel, _, cx| this.cancel(cx)))
                .child(toolbar)
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .overflow_hidden()
                        .child(CanvasElement { view: cx.entity() }),
                )
                .into_any_element();
        }

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
        root.p(px(32.))
            .flex()
            .items_center()
            .justify_center()
            .overflow_hidden()
            .child(content)
            .into_any_element()
    }
}

// -------------------------------------------------------------------------
// Reading
// -------------------------------------------------------------------------

enum Loaded {
    /// The file is as it was.
    Unchanged,
    Paint {
        pixels: RgbaImage,
        meta: Meta,
        /// From the session, not the file.
        unsaved: bool,
    },
    /// Shown with `img`, not painted on.
    Picture { bytes: Vec<u8> },
}

struct Read {
    loaded: Loaded,
    stamp: Option<Stamp>,
    info: SharedString,
}

/// Reads and decodes `path` (unless its stamp is still `known`), or the
/// session's `unsaved` pixels for it.
fn read_image(
    path: &Path,
    format: ImageFormat,
    known: Option<Stamp>,
    unsaved: Option<Vec<u8>>,
) -> anyhow::Result<Read> {
    let stamp = Stamp::of(path);
    if known.is_some() && stamp == known {
        return Ok(Read {
            loaded: Loaded::Unchanged,
            stamp,
            info: SharedString::default(),
        });
    }
    let file_format = paint::format_for_path(path).context("not an image")?;
    if let Some(unsaved) = unsaved {
        let pixels = paint::decode_unsaved(&unsaved)?;
        // The file, if it's still there, says how to write it back.
        let bytes = std::fs::read(path).ok();
        let meta = bytes
            .as_deref()
            .and_then(|bytes| match paint::open(bytes, file_format) {
                Ok(Opened::Editable { meta, .. }) => Some(meta),
                _ => None,
            })
            .or_else(|| Meta::guess(path))
            .context("not an image")?;
        let info = describe(
            format,
            Some(pixels.dimensions()),
            bytes.map_or(0, |b| b.len() as u64),
        );
        return Ok(Read {
            loaded: Loaded::Paint {
                pixels,
                meta,
                unsaved: true,
            },
            stamp,
            info,
        });
    }
    let bytes = std::fs::read(path)?;
    let len = bytes.len() as u64;
    Ok(match paint::open(&bytes, file_format)? {
        Opened::Editable { pixels, meta } => Read {
            info: describe(format, Some(pixels.dimensions()), len),
            loaded: Loaded::Paint {
                pixels,
                meta,
                unsaved: false,
            },
            stamp,
        },
        Opened::ViewOnly(note) => {
            let dims = image::ImageReader::new(std::io::Cursor::new(&bytes))
                .with_guessed_format()
                .ok()
                .and_then(|r| r.into_dimensions().ok());
            Read {
                info: format!("{} · {note}", describe(format, dims, len)).into(),
                loaded: Loaded::Picture { bytes },
                stamp,
            }
        }
    })
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

    #[test]
    fn colors_round_trip() {
        for color in SWATCHES {
            assert_eq!(from_hsla(to_hsla(color)), color);
        }
    }
}

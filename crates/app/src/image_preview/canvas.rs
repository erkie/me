//! The picture being painted on: where it sits in the view (zoom and
//! scrolling), drawing it (textures when zoomed out, a square per pixel
//! when zoomed in), and turning the mouse into strokes, pans and zooms.

use std::ops::Range;
use std::sync::Arc;

use gpui::{
    App, BorderStyle, Bounds, ContentMask, Context, Corners, CursorStyle, DispatchPhase, Element,
    ElementId, Entity, GlobalElementId, Hitbox, HitboxBehavior, InspectorElementId, IntoElement,
    LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PinchEvent, Pixels, Point,
    RenderImage, ScrollWheelEvent, Style, Window, fill, outline, point, px, relative, size,
};
use image::RgbaImage;

use super::{ImagePreview, PaintSettings, Tool, settings, to_hsla};
use crate::paint::{self, Canvas, Color, Meta, Rect};
use crate::theme;

/// Room around a picture that fits the view.
const PADDING: f32 = 24.;
/// Pixels per texture tile; each tile also has a one-pixel border of its
/// neighbours, so scaling doesn't show seams.
const TILE: u32 = 254;
/// Zoomed in this far, pixels are drawn as squares (textures blur).
const CRISP_ZOOM: f32 = 2.;
const MAX_PIXEL_QUADS: u64 = 80_000;
const ZOOM_STEPS: &[f32] = &[
    0.05, 0.1, 0.125, 0.25, 0.333, 0.5, 0.667, 1., 1.5, 2., 3., 4., 6., 8., 12., 16., 24., 32.,
    48., 64.,
];

impl ImagePreview {
    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle, cx);
        let settings = *settings(cx);
        let Some(paint) = &mut self.paint else { return };
        let Some(layout) = paint.layout else { return };
        let p = layout.to_image(event.position);
        let pan = event.button == MouseButton::Middle
            || (event.button == MouseButton::Left && settings.tool == Tool::Hand);
        if pan {
            paint.drag = Some(Drag::Pan {
                last: event.position,
            });
            cx.notify();
            return;
        }
        if event.button != MouseButton::Left {
            return;
        }
        if event.modifiers.alt || settings.tool == Tool::Eyedropper {
            paint.drag = Some(Drag::Pick);
            self.pick(p, cx);
            return;
        }
        let color = paint.color_for(settings);
        let size = settings.size;
        match settings.tool {
            Tool::Pencil | Tool::Eraser => {
                // Shift-click: a straight line from where the last one ended.
                let from = paint
                    .last_point
                    .filter(|_| event.modifiers.shift)
                    .unwrap_or(p);
                paint.canvas.begin_stroke();
                paint.canvas.line(from, p, size, color);
                paint.drag = Some(Drag::Stroke {
                    last: p,
                    color,
                    size,
                });
                self.redraw(cx);
            }
            Tool::Fill => {
                let (x, y) = (p.0.floor(), p.1.floor());
                let inside = x >= 0.
                    && y >= 0.
                    && (x as u32) < paint.canvas.width()
                    && (y as u32) < paint.canvas.height();
                if inside && paint.canvas.fill(x as u32, y as u32, color) {
                    paint.canvas.end_stroke();
                    self.edited(cx);
                }
            }
            tool if tool.is_shape() => {
                paint.canvas.begin_stroke();
                paint.drag = Some(Drag::Shape {
                    tool,
                    start: p,
                    color,
                    size,
                });
            }
            _ => {}
        }
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, hovered: bool, cx: &mut Context<Self>) {
        let Some(paint) = &mut self.paint else { return };
        let Some(layout) = paint.layout else { return };
        let p = layout.to_image(event.position);
        let before = paint.hovered_pixel();
        paint.hover = hovered.then_some(event.position);
        let hover_changed = paint.hovered_pixel() != before;
        match &mut paint.drag {
            Some(Drag::Pan { last }) => {
                let delta = event.position - *last;
                *last = event.position;
                paint.pan(delta);
                cx.notify();
            }
            Some(Drag::Stroke { last, color, size }) => {
                paint.canvas.line(*last, p, *size, *color);
                *last = p;
                self.redraw(cx);
            }
            Some(Drag::Shape {
                tool,
                start,
                color,
                size,
            }) => {
                let (tool, start, color, size) = (*tool, *start, *color, *size);
                paint.canvas.revert_stroke();
                paint.draw_shape(tool, start, p, event.modifiers.shift, size, color);
                self.redraw(cx);
            }
            Some(Drag::Pick) => self.pick(p, cx),
            None if hover_changed => cx.notify(),
            None => {}
        }
    }

    fn mouse_up(&mut self, event: &MouseUpEvent, cx: &mut Context<Self>) {
        if !matches!(event.button, MouseButton::Left | MouseButton::Middle) {
            return;
        }
        let Some(paint) = &mut self.paint else { return };
        let Some(drag) = paint.drag.take() else {
            return;
        };
        match drag {
            Drag::Stroke { last, .. } => {
                paint.last_point = Some(last);
                if paint.canvas.end_stroke() {
                    self.edited(cx);
                } else {
                    self.redraw(cx);
                }
            }
            Drag::Shape { .. } => {
                if paint.canvas.end_stroke() {
                    self.edited(cx);
                } else {
                    self.redraw(cx);
                }
            }
            Drag::Pan { .. } | Drag::Pick => cx.notify(),
        }
    }

    fn scroll(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let Some(paint) = &mut self.paint else { return };
        let mut delta = event.delta.pixel_delta(px(20.));
        if event.modifiers.secondary() || event.modifiers.control {
            let factor = (f32::from(delta.y) / 300.).exp();
            paint.zoom_by(factor, Some(event.position));
        } else {
            // A plain mouse wheel scrolls sideways with Shift.
            if event.modifiers.shift && delta.x == px(0.) {
                delta = point(delta.y, px(0.));
            }
            paint.pan(delta);
        }
        cx.notify();
    }

    fn pinch(&mut self, event: &PinchEvent, cx: &mut Context<Self>) {
        let Some(paint) = &mut self.paint else { return };
        paint.zoom_by(1. + event.delta, Some(event.position));
        cx.notify();
    }

    /// Eyedropper: the colour under `p` becomes the paint colour.
    fn pick(&mut self, p: (f32, f32), cx: &mut Context<Self>) {
        let Some(paint) = &self.paint else { return };
        if p.0 < 0. || p.1 < 0. {
            return;
        }
        if let Some(color) = paint.canvas.pixel(p.0 as u32, p.1 as u32) {
            self.set_color(color, cx);
        }
    }

    fn paint_canvas(
        &mut self,
        bounds: Bounds<Pixels>,
        hitbox: &Hitbox,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let palette = theme::palette(cx).clone();
        let settings = *settings(cx);
        let Some(paint) = &mut self.paint else { return };
        paint.tiles.collect_garbage(window);
        let layout = paint.layout(bounds, window.scale_factor());
        if paint.layout.map(|l| l.scale) != Some(layout.scale) {
            // The toolbar shows the zoom.
            let view = cx.entity();
            window.on_next_frame(move |_, cx| view.update(cx, |_, cx| cx.notify()));
        }
        paint.layout = Some(layout);

        let cursor = match (&paint.drag, settings.tool) {
            (Some(Drag::Pan { .. }), _) => CursorStyle::ClosedHand,
            (_, Tool::Hand) => CursorStyle::OpenHand,
            _ => CursorStyle::Crosshair,
        };
        window.set_cursor_style(cursor, hitbox);
        window.paint_quad(fill(bounds, palette.editor_bg));

        let (w, h) = paint.canvas.pixels().dimensions();
        let visible = layout.rect(0., 0., w as f32, h as f32).intersect(&bounds);
        if visible.size.width <= px(0.) || visible.size.height <= px(0.) {
            return;
        }
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            paint_checkerboard(window, &layout, visible, palette.is_dark);
            let (x0, y0) = layout.to_image(visible.origin);
            let (x1, y1) = layout.to_image(visible.bottom_right());
            let xs = (x0.floor().max(0.) as u32)..(x1.ceil().max(0.) as u32).min(w);
            let ys = (y0.floor().max(0.) as u32)..(y1.ceil().max(0.) as u32).min(h);
            let count = xs.len() as u64 * ys.len() as u64;
            if layout.scale >= CRISP_ZOOM && count <= MAX_PIXEL_QUADS {
                paint_pixels(window, &layout, paint.canvas.pixels(), xs, ys);
            } else if !xs.is_empty() && !ys.is_empty() {
                for ty in ys.start / TILE..=(ys.end - 1) / TILE {
                    for tx in xs.start / TILE..=(xs.end - 1) / TILE {
                        let tile = paint.tiles.tile(tx, ty, paint.canvas.pixels());
                        let (cx0, cy0) = ((tx * TILE) as f32, (ty * TILE) as f32);
                        let content = layout.rect(
                            cx0,
                            cy0,
                            ((tx + 1) * TILE).min(w) as f32,
                            ((ty + 1) * TILE).min(h) as f32,
                        );
                        let apron = content.dilate(px(layout.scale));
                        window.with_content_mask(Some(ContentMask { bounds: content }), |window| {
                            window
                                .paint_image(apron, apron, Corners::default(), tile, 0, false)
                                .ok();
                        });
                    }
                }
            }
            if let (Some(hover), true, false) = (
                paint.hover,
                settings.tool.uses_brush(),
                matches!(paint.drag, Some(Drag::Pan { .. })),
            ) {
                paint_brush_outline(window, &layout, layout.to_image(hover), settings.size);
            }
        });
    }
}

pub(super) enum Drag {
    Pan {
        last: Point<Pixels>,
    },
    Stroke {
        last: (f32, f32),
        color: Color,
        size: u32,
    },
    Shape {
        tool: Tool,
        start: (f32, f32),
        color: Color,
        size: u32,
    },
    Pick,
}

/// Where the picture was last drawn.
#[derive(Clone, Copy, Debug)]
pub(super) struct Layout {
    bounds: Bounds<Pixels>,
    origin: Point<Pixels>,
    /// Logical pixels per image pixel.
    pub(super) scale: f32,
}

impl Layout {
    fn to_image(self, p: Point<Pixels>) -> (f32, f32) {
        (
            f32::from(p.x - self.origin.x) / self.scale,
            f32::from(p.y - self.origin.y) / self.scale,
        )
    }

    fn to_screen(self, x: f32, y: f32) -> Point<Pixels> {
        point(
            self.origin.x + px(x * self.scale),
            self.origin.y + px(y * self.scale),
        )
    }

    fn rect(self, x0: f32, y0: f32, x1: f32, y1: f32) -> Bounds<Pixels> {
        Bounds::from_corners(self.to_screen(x0, y0), self.to_screen(x1, y1))
    }
}

pub(super) struct Paint {
    pub(super) canvas: Canvas,
    pub(super) meta: Meta,
    pub(super) tiles: Tiles,
    /// `None` fits the picture in the view (never enlarging it).
    pub(super) zoom: Option<f32>,
    /// The image point in the middle of the view.
    pub(super) center: (f32, f32),
    pub(super) drag: Option<Drag>,
    /// The pointer, while over the canvas.
    pub(super) hover: Option<Point<Pixels>>,
    /// Where the last pencil stroke ended, for Shift-click lines.
    pub(super) last_point: Option<(f32, f32)>,
    pub(super) layout: Option<Layout>,
}

impl Paint {
    pub(super) fn new(canvas: Canvas, meta: Meta) -> Self {
        let tiles = Tiles::new(canvas.width(), canvas.height());
        let mut this = Self {
            canvas,
            meta,
            tiles,
            zoom: None,
            center: (0., 0.),
            drag: None,
            hover: None,
            last_point: None,
            layout: None,
        };
        this.center = this.image_center();
        this
    }

    pub(super) fn image_center(&self) -> (f32, f32) {
        (
            self.canvas.width() as f32 / 2.,
            self.canvas.height() as f32 / 2.,
        )
    }

    pub(super) fn hovered_pixel(&self) -> Option<(u32, u32)> {
        let (x, y) = self.layout?.to_image(self.hover?);
        (x >= 0. && y >= 0.)
            .then_some((x as u32, y as u32))
            .filter(|&(x, y)| x < self.canvas.width() && y < self.canvas.height())
    }

    /// What the eraser paints where there's no transparency.
    fn color_for(&self, settings: PaintSettings) -> Color {
        match settings.tool {
            Tool::Eraser if paint::has_alpha(self.meta.format) => paint::TRANSPARENT,
            Tool::Eraser => paint::WHITE,
            _ => settings.color,
        }
    }

    /// Re-uploads whatever changed.
    pub(super) fn redraw(&mut self) {
        if let Some(rect) = self.canvas.take_damage() {
            self.tiles.invalidate(rect);
        }
    }

    fn draw_shape(
        &mut self,
        tool: Tool,
        start: (f32, f32),
        end: (f32, f32),
        constrain: bool,
        size: u32,
        color: Color,
    ) {
        let pixel = |p: (f32, f32)| (p.0.floor() as i64, p.1.floor() as i64);
        match tool {
            Tool::Line => {
                let end = if constrain {
                    // Horizontal, vertical or diagonal.
                    let (dx, dy) = (end.0 - start.0, end.1 - start.1);
                    let angle = dy.atan2(dx);
                    let snapped =
                        (angle / std::f32::consts::FRAC_PI_4).round() * std::f32::consts::FRAC_PI_4;
                    let len = dx.hypot(dy);
                    (start.0 + len * snapped.cos(), start.1 + len * snapped.sin())
                } else {
                    end
                };
                self.canvas.line(start, end, size, color);
            }
            Tool::Rectangle | Tool::Ellipse => {
                let a = pixel(start);
                let mut b = pixel(end);
                if constrain {
                    // Square, or circle, towards the pointer.
                    let side = (b.0 - a.0).abs().max((b.1 - a.1).abs());
                    let toward =
                        |from: i64, to: i64| if to < from { from - side } else { from + side };
                    b = (toward(a.0, b.0), toward(a.1, b.1));
                }
                if tool == Tool::Rectangle {
                    self.canvas.rectangle(a, b, size, color);
                } else {
                    self.canvas.ellipse(a, b, size, color);
                }
            }
            _ => {}
        }
    }

    fn fit_scale(&self, bounds: Bounds<Pixels>) -> f32 {
        let room_w = (f32::from(bounds.size.width) - 2. * PADDING).max(1.);
        let room_h = (f32::from(bounds.size.height) - 2. * PADDING).max(1.);
        (room_w / self.canvas.width() as f32)
            .min(room_h / self.canvas.height() as f32)
            .min(1.)
    }

    /// Keeps the picture from leaving the view: a picture smaller than the
    /// view stays centred; a bigger one can scroll to its edges.
    fn clamp_center(&self, center: (f32, f32), scale: f32, bounds: Bounds<Pixels>) -> (f32, f32) {
        let axis = |c: f32, len: u32, view: Pixels| {
            let len = len as f32;
            let view = f32::from(view) / scale;
            let pad = PADDING / scale;
            if len + 2. * pad <= view {
                len / 2.
            } else {
                c.clamp(view / 2. - pad, len - view / 2. + pad)
            }
        };
        (
            axis(center.0, self.canvas.width(), bounds.size.width),
            axis(center.1, self.canvas.height(), bounds.size.height),
        )
    }

    fn layout(&mut self, bounds: Bounds<Pixels>, scale_factor: f32) -> Layout {
        let scale = self.zoom.unwrap_or_else(|| self.fit_scale(bounds));
        self.center = match self.zoom {
            Some(_) => self.clamp_center(self.center, scale, bounds),
            None => self.image_center(),
        };
        let mid = bounds.center();
        // Whole device pixels, so zoomed pixels line up.
        let snap = |v: Pixels| px((f32::from(v) * scale_factor).round() / scale_factor);
        Layout {
            bounds,
            origin: point(
                snap(mid.x - px(self.center.0 * scale)),
                snap(mid.y - px(self.center.1 * scale)),
            ),
            scale,
        }
    }

    fn pan(&mut self, delta: Point<Pixels>) {
        // A picture fitted to the view has nowhere to go.
        let (Some(layout), Some(_)) = (self.layout, self.zoom) else {
            return;
        };
        let center = (
            self.center.0 - f32::from(delta.x) / layout.scale,
            self.center.1 - f32::from(delta.y) / layout.scale,
        );
        self.center = self.clamp_center(center, layout.scale, layout.bounds);
    }

    /// Zooms to `scale`, keeping the image point under `anchor` (or the
    /// middle of the view) where it is.
    fn zoom_to(&mut self, scale: f32, anchor: Option<Point<Pixels>>) {
        let Some(layout) = self.layout else { return };
        let fit = self.fit_scale(layout.bounds);
        let min = ZOOM_STEPS[0].min(fit);
        let max = *ZOOM_STEPS.last().unwrap();
        let scale = scale.clamp(min, max);
        let anchor = anchor.unwrap_or_else(|| layout.bounds.center());
        let (ix, iy) = layout.to_image(anchor);
        let mid = layout.bounds.center();
        let center = (
            ix - f32::from(anchor.x - mid.x) / scale,
            iy - f32::from(anchor.y - mid.y) / scale,
        );
        self.zoom = Some(scale);
        self.center = self.clamp_center(center, scale, layout.bounds);
    }

    fn zoom_by(&mut self, factor: f32, anchor: Option<Point<Pixels>>) {
        if let Some(layout) = self.layout {
            self.zoom_to(layout.scale * factor, anchor);
        }
    }

    pub(super) fn zoom_step(&mut self, zoom_in: bool) {
        let Some(layout) = self.layout else { return };
        let current = layout.scale;
        let fit = self.fit_scale(layout.bounds);
        let next = if zoom_in {
            ZOOM_STEPS.iter().copied().find(|&s| s > current * 1.01)
        } else {
            ZOOM_STEPS
                .iter()
                .rev()
                .copied()
                .find(|&s| s < current * 0.99)
        };
        match next {
            // Zooming out past the fit stops at the fit.
            Some(scale) if !zoom_in && scale < fit && current > fit * 1.01 => self.zoom = None,
            Some(scale) => self.zoom_to(scale, None),
            None => {}
        }
    }
}

/// The picture as textures, made as tiles come into view and remade when
/// their pixels change.
pub(super) struct Tiles {
    cols: u32,
    rows: u32,
    images: Vec<Option<Arc<RenderImage>>>,
    /// Replaced this frame; dropped from the atlas next frame, once no
    /// scene uses them.
    retired: Vec<Arc<RenderImage>>,
    dropping: Vec<Arc<RenderImage>>,
}

impl Tiles {
    fn new(w: u32, h: u32) -> Self {
        let (cols, rows) = (w.div_ceil(TILE), h.div_ceil(TILE));
        Self {
            cols,
            rows,
            images: vec![None; (cols * rows) as usize],
            retired: Vec::new(),
            dropping: Vec::new(),
        }
    }

    /// Tiles showing any pixel of `rect`, borders included.
    fn invalidate(&mut self, rect: Rect) {
        if self.cols == 0 || self.rows == 0 || rect.w == 0 || rect.h == 0 {
            return;
        }
        let range = |from: u32, to: u32, count: u32| {
            from.saturating_sub(1) / TILE..=(to / TILE).min(count - 1)
        };
        for ty in range(rect.y, rect.bottom(), self.rows) {
            for tx in range(rect.x, rect.right(), self.cols) {
                if let Some(image) = self.images[(ty * self.cols + tx) as usize].take() {
                    self.retired.push(image);
                }
            }
        }
    }

    fn tile(&mut self, tx: u32, ty: u32, pixels: &RgbaImage) -> Arc<RenderImage> {
        self.images[(ty * self.cols + tx) as usize]
            .get_or_insert_with(|| build_tile(pixels, tx, ty))
            .clone()
    }

    fn collect_garbage(&mut self, window: &mut Window) {
        for image in self.dropping.drain(..) {
            window.drop_image(image).ok();
        }
        self.dropping = std::mem::take(&mut self.retired);
    }

    pub(super) fn drop_all(self, cx: &mut App) {
        let images = self.images.into_iter().flatten();
        for image in images.chain(self.retired).chain(self.dropping) {
            cx.drop_image(image, None);
        }
    }
}

/// Tile `(tx, ty)` as BGRA, with a one-pixel border from its neighbours
/// (or its own edge at the picture's edge).
fn build_tile(pixels: &RgbaImage, tx: u32, ty: u32) -> Arc<RenderImage> {
    let (w, h) = pixels.dimensions();
    let (x0, y0) = (tx * TILE, ty * TILE);
    let (cw, ch) = (TILE.min(w - x0), TILE.min(h - y0));
    let (tw, th) = (cw + 2, ch + 2);
    let raw = pixels.as_raw();
    let mut out = Vec::with_capacity((tw * th * 4) as usize);
    let columns = std::iter::once(x0.saturating_sub(1))
        .chain(x0..x0 + cw)
        .chain(std::iter::once((x0 + cw).min(w - 1)));
    let columns: Vec<usize> = columns.map(|x| x as usize * 4).collect();
    for j in 0..th {
        let y = (y0 + j).saturating_sub(1).min(h - 1) as usize;
        let row = &raw[y * w as usize * 4..(y + 1) * w as usize * 4];
        for &x in &columns {
            out.extend_from_slice(&[row[x + 2], row[x + 1], row[x], row[x + 3]]);
        }
    }
    let buffer = RgbaImage::from_raw(tw, th, out).expect("tile buffer size");
    Arc::new(RenderImage::new(vec![image::Frame::new(buffer)]))
}

/// Squares behind the picture, so transparency shows.
fn paint_checkerboard(window: &mut Window, layout: &Layout, visible: Bounds<Pixels>, dark: bool) {
    const SQUARE: f32 = 8.;
    let (light, shade) = if dark {
        (gpui::rgb(0x3a3a3a), gpui::rgb(0x2e2e2e))
    } else {
        (gpui::rgb(0xffffff), gpui::rgb(0xe6e6e6))
    };
    window.paint_quad(fill(visible, light));
    let offset = |v: Pixels, o: Pixels| f32::from(v - o) / SQUARE;
    let (i0, i1) = (
        offset(visible.left(), layout.origin.x).floor() as i64,
        offset(visible.right(), layout.origin.x).ceil() as i64,
    );
    let (j0, j1) = (
        offset(visible.top(), layout.origin.y).floor() as i64,
        offset(visible.bottom(), layout.origin.y).ceil() as i64,
    );
    for j in j0..j1 {
        for i in i0..i1 {
            if (i + j).rem_euclid(2) == 1 {
                let square = Bounds::new(
                    point(
                        layout.origin.x + px(i as f32 * SQUARE),
                        layout.origin.y + px(j as f32 * SQUARE),
                    ),
                    size(px(SQUARE), px(SQUARE)),
                )
                .intersect(&visible);
                window.paint_quad(fill(square, shade));
            }
        }
    }
}

/// Zoomed in: each run of same-coloured pixels in a row as one square-edged
/// quad.
fn paint_pixels(
    window: &mut Window,
    layout: &Layout,
    pixels: &RgbaImage,
    xs: Range<u32>,
    ys: Range<u32>,
) {
    if xs.is_empty() {
        return;
    }
    for y in ys {
        let mut start = xs.start;
        let mut color = pixels.get_pixel(start, y).0;
        for x in xs.start + 1..=xs.end {
            let next = (x < xs.end).then(|| pixels.get_pixel(x, y).0);
            if next == Some(color) {
                continue;
            }
            if color[3] > 0 {
                let run = layout.rect(start as f32, y as f32, x as f32, (y + 1) as f32);
                window.paint_quad(fill(run, to_hsla(color)));
            }
            if let Some(next) = next {
                (start, color) = (x, next);
            }
        }
    }
}

/// The brush's footprint under the pointer, outlined in dark and light so
/// it shows on any colour.
fn paint_brush_outline(window: &mut Window, layout: &Layout, p: (f32, f32), size: u32) {
    let (x, y) = paint::brush_origin(p, size);
    let s = size as f32;
    let bounds = layout.rect(x as f32, y as f32, x as f32 + s, y as f32 + s);
    if bounds.size.width < px(4.) {
        return;
    }
    let radius = |b: Bounds<Pixels>| {
        if size >= 3 { b.size.width / 2. } else { px(0.) }
    };
    let inner = bounds.inset(px(1.));
    window.paint_quad(
        outline(bounds, gpui::black().opacity(0.6), BorderStyle::Solid)
            .corner_radii(radius(bounds)),
    );
    window.paint_quad(
        outline(inner, gpui::white().opacity(0.8), BorderStyle::Solid).corner_radii(radius(inner)),
    );
}

/// Draws the canvas and turns the mouse into strokes, pans and zooms.
pub(super) struct CanvasElement {
    pub(super) view: Entity<ImagePreview>,
}

impl IntoElement for CanvasElement {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for CanvasElement {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

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
        _cx: &mut App,
    ) -> Hitbox {
        window.insert_hitbox(bounds, HitboxBehavior::Normal)
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        hitbox: &mut Hitbox,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.view
            .update(cx, |view, cx| view.paint_canvas(bounds, hitbox, window, cx));

        let view = self.view.clone();
        let hb = hitbox.clone();
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble || !hb.is_hovered(window) {
                return;
            }
            view.update(cx, |view, cx| view.mouse_down(event, window, cx));
            cx.stop_propagation();
        });
        let view = self.view.clone();
        let hb = hitbox.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble {
                let hovered = hb.is_hovered(window);
                view.update(cx, |view, cx| view.mouse_move(event, hovered, cx));
            }
        });
        let view = self.view.clone();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, _window, cx| {
            if phase == DispatchPhase::Bubble {
                view.update(cx, |view, cx| view.mouse_up(event, cx));
            }
        });
        let view = self.view.clone();
        let hb = hitbox.clone();
        window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble && hb.is_hovered(window) {
                view.update(cx, |view, cx| view.scroll(event, cx));
                cx.stop_propagation();
            }
        });
        let view = self.view.clone();
        let hb = hitbox.clone();
        window.on_mouse_event(move |event: &PinchEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble && hb.is_hovered(window) {
                view.update(cx, |view, cx| view.pinch(event, cx));
                cx.stop_propagation();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiles_have_borders_from_their_neighbours() {
        let mut pixels = RgbaImage::new(TILE + 3, 2);
        for (x, _, p) in pixels.enumerate_pixels_mut() {
            *p = image::Rgba([x as u8, 0, 200, 255]);
        }
        let first = build_tile(&pixels, 0, 0);
        let bytes = first.as_bytes(0).unwrap();
        let width = TILE as usize + 2;
        // BGRA; the left border repeats the edge, the right one is the
        // next tile's first column.
        assert_eq!(&bytes[..4], &[200, 0, 0, 255]);
        assert_eq!(&bytes[4..8], &[200, 0, 0, 255]);
        assert_eq!(bytes[(width - 1) * 4 + 2], TILE as u8);
        let second = build_tile(&pixels, 1, 0);
        assert_eq!(second.size(0).width.0, 3 + 2);
        assert_eq!(second.as_bytes(0).unwrap()[2], (TILE - 1) as u8);

        let mut tiles = Tiles::new(TILE * 2, TILE);
        tiles.tile(0, 0, &pixels_of(TILE * 2, TILE));
        tiles.tile(1, 0, &pixels_of(TILE * 2, TILE));
        // A pixel on the second tile's left edge is in the first one's
        // border too.
        tiles.invalidate(Rect {
            x: TILE,
            y: 0,
            w: 1,
            h: 1,
        });
        assert_eq!(tiles.retired.len(), 2);
    }

    fn pixels_of(w: u32, h: u32) -> RgbaImage {
        RgbaImage::new(w, h)
    }
}

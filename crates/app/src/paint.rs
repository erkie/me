//! Pixels for painting on images: the brush, lines, shapes and fill, undo,
//! and reading and writing image files. No UI.

use std::io::Cursor;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context as _, bail};
use image::codecs::bmp::BmpEncoder;
use image::codecs::gif::{GifDecoder, GifEncoder};
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType, FilterType, PngDecoder, PngEncoder};
use image::codecs::tiff::TiffEncoder;
use image::codecs::webp::{WebPDecoder, WebPEncoder};
use image::{
    AnimationDecoder, ColorType, DynamicImage, ExtendedColorType, ImageDecoder, ImageEncoder,
    ImageFormat, RgbaImage,
};

/// Straight (not premultiplied) RGBA.
pub type Color = [u8; 4];

pub const TRANSPARENT: Color = [0, 0, 0, 0];
pub const WHITE: Color = [255, 255, 255, 255];

/// Undo keeps at most this many steps, and at most this many bytes of them.
const MAX_UNDO: usize = 200;
const MAX_UNDO_BYTES: usize = 256 << 20;

/// A rectangle of pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub fn right(&self) -> u32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> u32 {
        self.y + self.h
    }

    pub fn union(self, other: Rect) -> Rect {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        Rect {
            x,
            y,
            w: self.right().max(other.right()) - x,
            h: self.bottom().max(other.bottom()) - y,
        }
    }

    /// The part of `x0..x1` × `y0..y1` inside a `w` × `h` image, if any.
    fn clipped(x0: i64, y0: i64, x1: i64, y1: i64, w: u32, h: u32) -> Option<Rect> {
        let (x0, y0) = (x0.max(0), y0.max(0));
        let (x1, y1) = (x1.min(w as i64), y1.min(h as i64));
        (x0 < x1 && y0 < y1).then(|| Rect {
            x: x0 as u32,
            y: y0 as u32,
            w: (x1 - x0) as u32,
            h: (y1 - y0) as u32,
        })
    }
}

/// The pixels a step changed, as they were on the other side of it: undo
/// and redo swap them with the image.
struct Patch {
    id: u64,
    rect: Rect,
    pixels: Vec<u8>,
}

/// An edit in progress: a stroke, or a shape being dragged out.
struct Stroke {
    before: Arc<RgbaImage>,
    touched: Option<Rect>,
}

/// An image being painted on, with its undo history.
pub struct Canvas {
    /// Shared with the session and the saver; edits copy it if they still
    /// hold it.
    pixels: Arc<RgbaImage>,
    undo: Vec<Patch>,
    redo: Vec<Patch>,
    next_id: u64,
    /// The state with every undo step undone (older steps were dropped).
    base_id: u64,
    /// The state the file on disk has.
    saved_id: u64,
    stroke: Option<Stroke>,
    damage: Option<Rect>,
}

impl Canvas {
    pub fn new(pixels: RgbaImage) -> Self {
        Self {
            pixels: Arc::new(pixels),
            undo: Vec::new(),
            redo: Vec::new(),
            next_id: 2,
            base_id: 1,
            saved_id: 1,
            stroke: None,
            damage: None,
        }
    }

    /// Canvas for pixels that differ from the file (unsaved work brought
    /// back from a session).
    pub fn unsaved(pixels: RgbaImage) -> Self {
        let mut canvas = Self::new(pixels);
        canvas.saved_id = 0;
        canvas
    }

    pub fn width(&self) -> u32 {
        self.pixels.width()
    }

    pub fn height(&self) -> u32 {
        self.pixels.height()
    }

    pub fn pixels(&self) -> &Arc<RgbaImage> {
        &self.pixels
    }

    pub fn pixel(&self, x: u32, y: u32) -> Option<Color> {
        (x < self.width() && y < self.height()).then(|| self.pixels.get_pixel(x, y).0)
    }

    /// Identifies what the pixels look like now: undo and redo come back to
    /// the same id.
    pub fn state_id(&self) -> u64 {
        self.undo.last().map_or(self.base_id, |p| p.id)
    }

    pub fn is_dirty(&self) -> bool {
        self.state_id() != self.saved_id
    }

    /// The file now holds state `id` (from [`Self::state_id`] when saving
    /// began).
    pub fn mark_saved(&mut self, id: u64) {
        self.saved_id = id;
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// The pixels changed since the last call.
    pub fn take_damage(&mut self) -> Option<Rect> {
        self.damage.take()
    }

    fn touch(&mut self, rect: Rect) {
        self.damage = Some(self.damage.map_or(rect, |d| d.union(rect)));
        if let Some(stroke) = &mut self.stroke {
            stroke.touched = Some(stroke.touched.map_or(rect, |t| t.union(rect)));
        }
    }

    /// Starts an edit; every change until [`Self::end_stroke`] is one undo
    /// step.
    pub fn begin_stroke(&mut self) {
        if self.stroke.is_none() {
            self.stroke = Some(Stroke {
                before: self.pixels.clone(),
                touched: None,
            });
        }
    }

    /// Puts back what the current edit changed so far (a shape being
    /// dragged is redrawn from scratch on every move).
    pub fn revert_stroke(&mut self) {
        let Some(stroke) = &mut self.stroke else {
            return;
        };
        let Some(rect) = stroke.touched.take() else {
            return;
        };
        let before = stroke.before.clone();
        copy_rect(&before, Arc::make_mut(&mut self.pixels), rect);
        self.damage = Some(self.damage.map_or(rect, |d| d.union(rect)));
    }

    /// Abandons the current edit, putting its pixels back.
    pub fn cancel_stroke(&mut self) {
        self.revert_stroke();
        self.stroke = None;
    }

    /// Finishes the current edit; returns whether it changed anything.
    pub fn end_stroke(&mut self) -> bool {
        let Some(stroke) = self.stroke.take() else {
            return false;
        };
        let Some(rect) = stroke.touched else {
            return false;
        };
        let before = read_rect(&stroke.before, rect);
        drop(stroke);
        if before == read_rect(&self.pixels, rect) {
            return false;
        }
        self.redo.clear();
        let id = self.next_id;
        self.next_id += 1;
        self.undo.push(Patch {
            id,
            rect,
            pixels: before,
        });
        self.trim_undo();
        true
    }

    fn trim_undo(&mut self) {
        let mut bytes: usize = self.undo.iter().map(|p| p.pixels.len()).sum();
        let mut drop = 0;
        while self.undo.len() - drop > 1
            && (self.undo.len() - drop > MAX_UNDO || bytes > MAX_UNDO_BYTES)
        {
            bytes -= self.undo[drop].pixels.len();
            drop += 1;
        }
        if drop > 0 {
            self.base_id = self.undo[drop - 1].id;
            self.undo.drain(..drop);
        }
    }

    pub fn undo(&mut self) -> bool {
        self.cancel_stroke();
        let Some(mut patch) = self.undo.pop() else {
            return false;
        };
        self.swap(&mut patch);
        self.redo.push(patch);
        true
    }

    pub fn redo(&mut self) -> bool {
        self.cancel_stroke();
        let Some(mut patch) = self.redo.pop() else {
            return false;
        };
        self.swap(&mut patch);
        self.undo.push(patch);
        true
    }

    fn swap(&mut self, patch: &mut Patch) {
        let current = read_rect(&self.pixels, patch.rect);
        write_rect(Arc::make_mut(&mut self.pixels), patch.rect, &patch.pixels);
        patch.pixels = current;
        self.touch(patch.rect);
    }

    /// Replaces every pixel (the file changed on disk) and forgets the
    /// history.
    pub fn replace(&mut self, pixels: RgbaImage) {
        let rect = Rect {
            x: 0,
            y: 0,
            w: pixels.width().max(self.width()),
            h: pixels.height().max(self.height()),
        };
        *self = Self {
            next_id: self.next_id + 1,
            base_id: self.next_id,
            saved_id: self.next_id,
            ..Self::new(pixels)
        };
        self.damage = Some(rect);
    }

    // ---------------------------------------------------------------------
    // Drawing
    // ---------------------------------------------------------------------

    /// A line of brush `size` pixels across from `from` to `to`, in image
    /// coordinates (pixel `(x, y)` covers `x..x + 1`).
    pub fn line(&mut self, from: (f32, f32), to: (f32, f32), size: u32, color: Color) {
        let size = size.max(1);
        let rows = brush_rows(size);
        let (x0, y0) = brush_origin(from, size);
        let (x1, y1) = brush_origin(to, size);
        let s = size as i64;
        let Some(rect) = Rect::clipped(
            x0.min(x1),
            y0.min(y1),
            x0.max(x1) + s,
            y0.max(y1) + s,
            self.width(),
            self.height(),
        ) else {
            return;
        };
        self.begin_stroke();
        let image = Arc::make_mut(&mut self.pixels);
        // Bresenham between the brush positions.
        let (dx, dy) = ((x1 - x0).abs(), -(y1 - y0).abs());
        let (sx, sy) = (if x0 < x1 { 1 } else { -1 }, if y0 < y1 { 1 } else { -1 });
        let (mut x, mut y, mut err) = (x0, y0, dx + dy);
        loop {
            stamp(image, x, y, &rows, color);
            if x == x1 && y == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x += sx;
            }
            if e2 <= dx {
                err += dx;
                y += sy;
            }
        }
        self.touch(rect);
    }

    /// The outline of the rectangle with corner pixels `a` and `b`, `size`
    /// pixels thick, drawn inwards.
    pub fn rectangle(&mut self, a: (i64, i64), b: (i64, i64), size: u32, color: Color) {
        let (x0, x1) = (a.0.min(b.0), a.0.max(b.0) + 1);
        let (y0, y1) = (a.1.min(b.1), a.1.max(b.1) + 1);
        let t = size.max(1) as i64;
        let bands = [
            (x0, y0, x1, (y0 + t).min(y1)),
            (x0, (y1 - t).max(y0), x1, y1),
            (x0, y0, (x0 + t).min(x1), y1),
            ((x1 - t).max(x0), y0, x1, y1),
        ];
        let (w, h) = (self.width(), self.height());
        let Some(rect) = Rect::clipped(x0, y0, x1, y1, w, h) else {
            return;
        };
        self.begin_stroke();
        let image = Arc::make_mut(&mut self.pixels);
        for (bx0, by0, bx1, by1) in bands {
            if let Some(band) = Rect::clipped(bx0, by0, bx1, by1, w, h) {
                fill_rect(image, band, color);
            }
        }
        self.touch(rect);
    }

    /// The outline of the ellipse inside the rectangle with corner pixels
    /// `a` and `b`, `size` pixels thick, drawn inwards.
    pub fn ellipse(&mut self, a: (i64, i64), b: (i64, i64), size: u32, color: Color) {
        let (x0, x1) = (a.0.min(b.0), a.0.max(b.0) + 1);
        let (y0, y1) = (a.1.min(b.1), a.1.max(b.1) + 1);
        let (w, h) = (self.width(), self.height());
        let Some(rect) = Rect::clipped(x0, y0, x1, y1, w, h) else {
            return;
        };
        let t = size.max(1) as f64;
        let (cx, cy) = ((x0 + x1) as f64 / 2., (y0 + y1) as f64 / 2.);
        let (rx, ry) = ((x1 - x0) as f64 / 2., (y1 - y0) as f64 / 2.);
        let (irx, iry) = (rx - t, ry - t);
        let half_width = |r: f64, rr: f64, dy: f64| -> Option<f64> {
            (r > 0. && rr > 0. && dy.abs() < rr).then(|| r * (1. - (dy / rr).powi(2)).sqrt())
        };
        self.begin_stroke();
        let image = Arc::make_mut(&mut self.pixels);
        for y in rect.y..rect.bottom() {
            let dy = y as f64 + 0.5 - cy;
            let Some(outer) = half_width(rx, ry, dy) else {
                continue;
            };
            // Pixel x is in when its centre x + 0.5 is.
            let first = |edge: f64| (edge - 0.5).ceil() as i64;
            let last = |edge: f64| (edge - 0.5).floor() as i64;
            let spans = match half_width(irx, iry, dy) {
                Some(inner) => [
                    (first(cx - outer), first(cx - inner) - 1),
                    (last(cx + inner) + 1, last(cx + outer)),
                ],
                None => [(first(cx - outer), last(cx + outer)), (1, 0)],
            };
            for (from, to) in spans {
                if let Some(span) = Rect::clipped(from, y as i64, to + 1, y as i64 + 1, w, h) {
                    fill_rect(image, span, color);
                }
            }
        }
        self.touch(rect);
    }

    /// Fills the area of pixels the same colour as `(x, y)`, connected
    /// through their sides, with `color`. Returns whether anything changed.
    pub fn fill(&mut self, x: u32, y: u32, color: Color) -> bool {
        let Some(target) = self.pixel(x, y) else {
            return false;
        };
        if same_color(target, color) {
            return false;
        }
        self.begin_stroke();
        let (w, h) = (self.width(), self.height());
        let image = Arc::make_mut(&mut self.pixels);
        let matches =
            |image: &RgbaImage, x: u32, y: u32| same_color(image.get_pixel(x, y).0, target);
        let (mut min_x, mut min_y, mut max_x, mut max_y) = (x, y, x, y);
        let mut stack = vec![(x, y)];
        while let Some((x, y)) = stack.pop() {
            if !matches(image, x, y) {
                continue;
            }
            let mut left = x;
            while left > 0 && matches(image, left - 1, y) {
                left -= 1;
            }
            let mut right = x;
            while right + 1 < w && matches(image, right + 1, y) {
                right += 1;
            }
            for px in left..=right {
                image.get_pixel_mut(px, y).0 = color;
            }
            (min_x, max_x) = (min_x.min(left), max_x.max(right));
            (min_y, max_y) = (min_y.min(y), max_y.max(y));
            for ny in [y.checked_sub(1), (y + 1 < h).then_some(y + 1)]
                .into_iter()
                .flatten()
            {
                let mut in_run = false;
                for px in left..=right {
                    let m = matches(image, px, ny);
                    if m && !in_run {
                        stack.push((px, ny));
                    }
                    in_run = m;
                }
            }
        }
        self.touch(Rect {
            x: min_x,
            y: min_y,
            w: max_x - min_x + 1,
            h: max_y - min_y + 1,
        });
        true
    }
}

/// Every fully transparent pixel counts as the same colour.
fn same_color(a: Color, b: Color) -> bool {
    a == b || (a[3] == 0 && b[3] == 0)
}

/// Top-left pixel of a brush `size` pixels across centred on `p`: odd
/// brushes centre on the pixel under `p`, even ones on its nearest corner.
pub fn brush_origin(p: (f32, f32), size: u32) -> (i64, i64) {
    let half = size as f32 / 2. - 0.5;
    ((p.0 - half).floor() as i64, (p.1 - half).floor() as i64)
}

/// For each row of a round brush, the columns it covers (`start..end`).
pub fn brush_rows(size: u32) -> Vec<(u32, u32)> {
    let r = size as f64 / 2.;
    (0..size)
        .map(|row| {
            if size <= 2 {
                return (0, size);
            }
            let dy = row as f64 + 0.5 - r;
            let half = (r * r - dy * dy).max(0.).sqrt();
            let start = (r - half - 0.5).ceil().max(0.) as u32;
            let end = ((r + half - 0.5).floor() as u32 + 1).min(size);
            (start, end.max(start))
        })
        .collect()
}

fn stamp(image: &mut RgbaImage, x: i64, y: i64, rows: &[(u32, u32)], color: Color) {
    let (w, h) = (image.width() as i64, image.height() as i64);
    for (row, &(start, end)) in rows.iter().enumerate() {
        let py = y + row as i64;
        if py < 0 || py >= h {
            continue;
        }
        let from = (x + start as i64).max(0);
        let to = (x + end as i64).min(w);
        for px in from..to {
            image.get_pixel_mut(px as u32, py as u32).0 = color;
        }
    }
}

fn fill_rect(image: &mut RgbaImage, rect: Rect, color: Color) {
    for y in rect.y..rect.bottom() {
        for x in rect.x..rect.right() {
            image.get_pixel_mut(x, y).0 = color;
        }
    }
}

fn row_range(image: &RgbaImage, rect: Rect, y: u32) -> std::ops::Range<usize> {
    let start = ((y * image.width() + rect.x) * 4) as usize;
    start..start + rect.w as usize * 4
}

fn read_rect(image: &RgbaImage, rect: Rect) -> Vec<u8> {
    let mut out = Vec::with_capacity(rect.w as usize * rect.h as usize * 4);
    for y in rect.y..rect.bottom() {
        out.extend_from_slice(&image.as_raw()[row_range(image, rect, y)]);
    }
    out
}

fn write_rect(image: &mut RgbaImage, rect: Rect, pixels: &[u8]) {
    let row_len = rect.w as usize * 4;
    for (i, y) in (rect.y..rect.bottom()).enumerate() {
        let range = row_range(image, rect, y);
        let raw: &mut [u8] = image;
        raw[range].copy_from_slice(&pixels[i * row_len..(i + 1) * row_len]);
    }
}

fn copy_rect(from: &RgbaImage, to: &mut RgbaImage, rect: Rect) {
    for y in rect.y..rect.bottom() {
        let range = row_range(from, rect, y);
        let raw: &mut [u8] = to;
        raw[range.clone()].copy_from_slice(&from.as_raw()[range]);
    }
}

// -------------------------------------------------------------------------
// Files
// -------------------------------------------------------------------------

/// What a file holds besides its pixels, to write it back the same way.
#[derive(Clone, Debug)]
pub struct Meta {
    pub format: ImageFormat,
    /// The colour type as decoded, before conversion to RGBA.
    pub color: ColorType,
    pub icc: Option<Vec<u8>>,
}

impl Meta {
    /// For a file we know only the name of.
    pub fn guess(path: &Path) -> Option<Self> {
        Some(Self {
            format: format_for_path(path)?,
            color: ColorType::Rgba8,
            icc: None,
        })
    }
}

pub enum Opened {
    Editable {
        pixels: RgbaImage,
        meta: Meta,
    },
    /// Shown as a picture only; the reason, for the status bar.
    ViewOnly(&'static str),
}

pub fn format_for_path(path: &Path) -> Option<ImageFormat> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => ImageFormat::Png,
        "jpg" | "jpeg" => ImageFormat::Jpeg,
        "gif" => ImageFormat::Gif,
        "webp" => ImageFormat::WebP,
        "bmp" => ImageFormat::Bmp,
        "tif" | "tiff" => ImageFormat::Tiff,
        "ico" => ImageFormat::Ico,
        _ => return None,
    })
}

/// Whether we can write `format`.
pub fn can_write(format: ImageFormat) -> bool {
    matches!(
        format,
        ImageFormat::Png
            | ImageFormat::Jpeg
            | ImageFormat::Gif
            | ImageFormat::WebP
            | ImageFormat::Bmp
            | ImageFormat::Tiff
    )
}

/// Whether pixels in `format` can be transparent (the eraser paints white
/// where they can't).
pub fn has_alpha(format: ImageFormat) -> bool {
    format != ImageFormat::Jpeg
}

/// Decodes a file for painting, unless writing it back would lose
/// something (frames, sizes, bit depth).
pub fn open(bytes: &[u8], format: ImageFormat) -> anyhow::Result<Opened> {
    if format == ImageFormat::Ico {
        return Ok(Opened::ViewOnly("icons are view only"));
    }
    let animated = match format {
        ImageFormat::Gif => {
            GifDecoder::new(Cursor::new(bytes))?
                .into_frames()
                .take(2)
                .count()
                > 1
        }
        ImageFormat::WebP => WebPDecoder::new(Cursor::new(bytes))?.has_animation(),
        ImageFormat::Png => PngDecoder::new(Cursor::new(bytes))?.is_apng()?,
        _ => false,
    };
    if animated {
        return Ok(Opened::ViewOnly("animated, view only"));
    }
    let mut decoder = image::ImageReader::with_format(Cursor::new(bytes), format)
        .into_decoder()
        .context("reading the image")?;
    let color = decoder.color_type();
    if !matches!(
        color,
        ColorType::L8 | ColorType::La8 | ColorType::Rgb8 | ColorType::Rgba8
    ) {
        return Ok(Opened::ViewOnly("16-bit, view only"));
    }
    let icc = decoder.icc_profile().ok().flatten();
    let orientation = decoder.orientation()?;
    let mut image = DynamicImage::from_decoder(decoder).context("decoding the image")?;
    image.apply_orientation(orientation);
    Ok(Opened::Editable {
        pixels: image.into_rgba8(),
        meta: Meta { format, color, icc },
    })
}

/// Encodes `pixels` as `format`, as close to the original file (`meta`) as
/// the pixels allow: grey stays grey and opaque stays opaque.
pub fn encode(pixels: &RgbaImage, meta: &Meta, format: ImageFormat) -> anyhow::Result<Vec<u8>> {
    let raw = pixels.as_raw();
    let opaque = raw.chunks_exact(4).all(|p| p[3] == 255);
    let grey = matches!(meta.color, ColorType::L8 | ColorType::La8)
        && raw.chunks_exact(4).all(|p| p[0] == p[1] && p[1] == p[2]);
    let alpha = has_alpha(format) && (meta.color.has_alpha() || !opaque);
    let (w, h) = pixels.dimensions();
    let mut out = Vec::new();

    let (data, color): (Vec<u8>, ExtendedColorType) = match (grey, alpha) {
        (true, true) if format != ImageFormat::Tiff && format != ImageFormat::Gif => (
            raw.chunks_exact(4).flat_map(|p| [p[0], p[3]]).collect(),
            ExtendedColorType::La8,
        ),
        (true, false) if format != ImageFormat::Gif => (
            raw.chunks_exact(4).map(|p| flatten(p)[0]).collect(),
            ExtendedColorType::L8,
        ),
        (_, true) => (raw.clone(), ExtendedColorType::Rgba8),
        (_, false) => (
            raw.chunks_exact(4).flat_map(flatten).collect(),
            ExtendedColorType::Rgb8,
        ),
    };
    let icc = meta.icc.clone();
    match format {
        ImageFormat::Png => {
            let mut encoder = PngEncoder::new_with_quality(
                &mut out,
                CompressionType::Default,
                FilterType::Adaptive,
            );
            if let Some(icc) = icc {
                let _ = encoder.set_icc_profile(icc);
            }
            encoder.write_image(&data, w, h, color)?;
        }
        ImageFormat::Jpeg => {
            let mut encoder = JpegEncoder::new_with_quality(&mut out, 92);
            if let Some(icc) = icc {
                let _ = encoder.set_icc_profile(icc);
            }
            encoder.write_image(&data, w, h, color)?;
        }
        ImageFormat::WebP => {
            let mut encoder = WebPEncoder::new_lossless(&mut out);
            if let Some(icc) = icc {
                let _ = encoder.set_icc_profile(icc);
            }
            encoder.write_image(&data, w, h, color)?;
        }
        ImageFormat::Tiff => {
            let mut encoder = TiffEncoder::new(Cursor::new(&mut out));
            if let Some(icc) = icc {
                let _ = encoder.set_icc_profile(icc);
            }
            encoder.write_image(&data, w, h, color)?;
        }
        ImageFormat::Bmp => BmpEncoder::new(&mut out).write_image(&data, w, h, color)?,
        ImageFormat::Gif => GifEncoder::new(&mut out).encode(&data, w, h, color)?,
        _ => bail!(
            "can't write {} files",
            format.extensions_str()[0].to_uppercase()
        ),
    }
    Ok(out)
}

/// A pixel over white, for formats without transparency.
fn flatten(p: &[u8]) -> [u8; 3] {
    let a = p[3] as u32;
    let over = |c: u8| ((c as u32 * a + 255 * (255 - a) + 127) / 255) as u8;
    [over(p[0]), over(p[1]), over(p[2])]
}

/// Unsaved pixels, for the session: quick to write, exact to read back.
pub fn encode_unsaved(pixels: &RgbaImage) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();
    PngEncoder::new_with_quality(&mut out, CompressionType::Fast, FilterType::Adaptive)
        .write_image(
            pixels.as_raw(),
            pixels.width(),
            pixels.height(),
            ExtendedColorType::Rgba8,
        )?;
    Ok(out)
}

pub fn decode_unsaved(bytes: &[u8]) -> anyhow::Result<RgbaImage> {
    Ok(image::load_from_memory_with_format(bytes, ImageFormat::Png)?.into_rgba8())
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Color = [255, 0, 0, 255];
    const BLUE: Color = [0, 0, 255, 255];

    fn canvas(w: u32, h: u32) -> Canvas {
        Canvas::new(RgbaImage::from_pixel(w, h, image::Rgba(WHITE)))
    }

    /// The image as rows of `#` (`color`) and `.` (anything else).
    fn picture(c: &Canvas, color: Color) -> Vec<String> {
        (0..c.height())
            .map(|y| {
                (0..c.width())
                    .map(|x| {
                        if c.pixel(x, y) == Some(color) {
                            '#'
                        } else {
                            '.'
                        }
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn brushes_are_round() {
        assert_eq!(brush_rows(1), vec![(0, 1)]);
        assert_eq!(brush_rows(2), vec![(0, 2), (0, 2)]);
        assert_eq!(brush_rows(5), vec![(1, 4), (0, 5), (0, 5), (0, 5), (1, 4)]);
        assert_eq!(brush_origin((3.7, 2.2), 1), (3, 2));
        assert_eq!(brush_origin((3.7, 2.2), 3), (2, 1));
        assert_eq!(brush_origin((3.7, 2.2), 2), (3, 1));
    }

    #[test]
    fn pencil_lines_are_one_pixel_wide() {
        let mut c = canvas(6, 4);
        c.begin_stroke();
        c.line((0.5, 0.5), (5.5, 3.5), 1, RED);
        assert!(c.end_stroke());
        assert_eq!(picture(&c, RED), ["#.....", ".##...", "...##.", ".....#"]);
    }

    #[test]
    fn rectangles_and_ellipses() {
        let mut c = canvas(6, 5);
        c.rectangle((4, 3), (1, 0), 1, RED);
        assert_eq!(
            picture(&c, RED),
            [".####.", ".#..#.", ".#..#.", ".####.", "......"]
        );
        let mut c = canvas(9, 7);
        c.ellipse((0, 0), (8, 6), 1, BLUE);
        assert_eq!(
            picture(&c, BLUE),
            [
                "..#####..",
                ".#.....#.",
                "#.......#",
                "#.......#",
                "#.......#",
                ".#.....#.",
                "..#####..",
            ]
        );
        // Thicker than the shape is wide: filled.
        let mut c = canvas(4, 4);
        c.ellipse((0, 0), (3, 3), 3, BLUE);
        assert_eq!(picture(&c, BLUE), [".##.", "####", "####", ".##."]);
    }

    #[test]
    fn fill_stops_at_other_colors() {
        let mut c = canvas(5, 5);
        c.rectangle((0, 0), (3, 3), 1, RED);
        c.end_stroke();
        assert!(c.fill(1, 1, BLUE));
        c.end_stroke();
        assert_eq!(
            picture(&c, BLUE),
            [".....", ".##..", ".##..", ".....", "....."]
        );
        // Outside the square: everything else that's white.
        assert!(c.fill(4, 4, BLUE));
        assert_eq!(c.pixel(4, 0), Some(BLUE));
        assert_eq!(c.pixel(0, 4), Some(BLUE));
        assert_eq!(c.pixel(0, 0), Some(RED));
        assert!(!c.fill(4, 4, BLUE));
    }

    #[test]
    fn undo_redo_and_dirty() {
        let mut c = canvas(4, 4);
        assert!(!c.is_dirty());
        c.line((0.5, 0.5), (3.5, 0.5), 1, RED);
        c.end_stroke();
        assert!(c.is_dirty());
        c.line((0.5, 3.5), (3.5, 3.5), 1, BLUE);
        c.end_stroke();
        let saved = c.state_id();
        c.mark_saved(saved);
        assert!(!c.is_dirty());

        assert!(c.undo());
        assert!(c.is_dirty());
        assert_eq!(c.pixel(0, 3), Some(WHITE));
        assert_eq!(c.pixel(0, 0), Some(RED));
        assert!(c.redo());
        assert!(!c.is_dirty());
        assert_eq!(c.pixel(0, 3), Some(BLUE));

        // A new edit after undoing drops the redo history.
        c.undo();
        c.fill(0, 3, RED);
        c.end_stroke();
        assert!(!c.can_redo());
        assert!(c.is_dirty());
        c.undo();
        c.undo();
        c.undo();
        assert!(!c.can_undo());
        assert_eq!(picture(&c, WHITE), ["####"; 4]);
    }

    #[test]
    fn painting_the_same_color_is_not_an_edit() {
        let mut c = canvas(3, 3);
        c.line((0.5, 0.5), (2.5, 2.5), 1, WHITE);
        assert!(!c.end_stroke());
        assert!(!c.is_dirty());
        assert!(!c.can_undo());
    }

    #[test]
    fn shapes_redraw_while_dragging() {
        let mut c = canvas(5, 5);
        c.begin_stroke();
        c.rectangle((0, 0), (4, 4), 1, RED);
        c.revert_stroke();
        c.rectangle((1, 1), (2, 2), 1, RED);
        assert!(c.end_stroke());
        assert_eq!(
            picture(&c, RED),
            [".....", ".##..", ".##..", ".....", "....."]
        );
        c.undo();
        assert_eq!(picture(&c, WHITE), ["#####"; 5]);
    }

    #[test]
    fn undo_history_is_bounded() {
        let mut c = canvas(2, 2);
        for i in 0..MAX_UNDO + 10 {
            let color = if i % 2 == 0 { RED } else { BLUE };
            c.fill(0, 0, color);
            c.end_stroke();
        }
        assert_eq!(c.undo.len(), MAX_UNDO);
        while c.undo() {}
        // The oldest steps are gone, so this isn't what the file has.
        assert!(c.is_dirty());
    }

    #[test]
    fn round_trips_through_files() {
        let mut pixels = RgbaImage::from_pixel(3, 2, image::Rgba(RED));
        pixels.put_pixel(1, 1, image::Rgba(TRANSPARENT));
        let meta = Meta {
            format: ImageFormat::Png,
            color: ColorType::Rgb8,
            icc: None,
        };
        for format in [
            ImageFormat::Png,
            ImageFormat::WebP,
            ImageFormat::Tiff,
            ImageFormat::Bmp,
        ] {
            let bytes = encode(&pixels, &meta, format).unwrap();
            let Opened::Editable { pixels: back, meta } = open(&bytes, format).unwrap() else {
                panic!("{format:?} should be editable");
            };
            assert_eq!(back, pixels, "{format:?}");
            assert!(meta.color.has_alpha(), "{format:?}");
        }
        // No transparency: written without an alpha channel.
        let opaque = RgbaImage::from_pixel(2, 2, image::Rgba(BLUE));
        let bytes = encode(&opaque, &meta, ImageFormat::Png).unwrap();
        let Opened::Editable { meta, .. } = open(&bytes, ImageFormat::Png).unwrap() else {
            panic!()
        };
        assert_eq!(meta.color, ColorType::Rgb8);
        // JPEG flattens onto white.
        let bytes = encode(&pixels, &meta, ImageFormat::Jpeg).unwrap();
        assert!(matches!(
            open(&bytes, ImageFormat::Jpeg).unwrap(),
            Opened::Editable { .. }
        ));
        let back = decode_unsaved(&encode_unsaved(&pixels).unwrap()).unwrap();
        assert_eq!(back, pixels);
    }

    #[test]
    fn grey_stays_grey() {
        let pixels = RgbaImage::from_pixel(2, 2, image::Rgba([90, 90, 90, 255]));
        let meta = Meta {
            format: ImageFormat::Png,
            color: ColorType::L8,
            icc: None,
        };
        let bytes = encode(&pixels, &meta, ImageFormat::Png).unwrap();
        let Opened::Editable { meta: back, .. } = open(&bytes, ImageFormat::Png).unwrap() else {
            panic!()
        };
        assert_eq!(back.color, ColorType::L8);
        // Paint in colour and it's colour.
        let mut colored = pixels.clone();
        colored.put_pixel(0, 0, image::Rgba(RED));
        let bytes = encode(&colored, &meta, ImageFormat::Png).unwrap();
        let Opened::Editable { meta: back, .. } = open(&bytes, ImageFormat::Png).unwrap() else {
            panic!()
        };
        assert_eq!(back.color, ColorType::Rgb8);
    }

    #[test]
    fn view_only_formats() {
        assert!(matches!(
            open(&[], ImageFormat::Ico).unwrap(),
            Opened::ViewOnly(_)
        ));
        assert!(!can_write(ImageFormat::Ico));
        assert_eq!(
            format_for_path(Path::new("a/B.JPEG")),
            Some(ImageFormat::Jpeg)
        );
    }
}

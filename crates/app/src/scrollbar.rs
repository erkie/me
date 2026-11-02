//! Scrollbars that behave like macOS ones: they fade in while scrolling and
//! out after, widen under the pointer, and can be dragged or clicked. The
//! drawing and interaction are gpui-component's [`Scrollbar`]; whether they
//! hide follows the system's "Show scroll bars" setting through its theme.
//!
//! Views that scroll a gpui [`gpui::ScrollHandle`] hand it to the scrollbar
//! directly. The editor and the CSV table paint and scroll themselves, so
//! they share their geometry through [`ScrollMetrics`] instead.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{App, Bounds, ElementId, Pixels, Point, Size, point, px};
use gpui_component::scroll::Scrollbar;
use gpui_component::scroll::{ScrollbarHandle, ScrollbarMode};

use crate::theme;

/// Scroll geometry a self-painted view shares with its [`Scrollbar`].
///
/// The view publishes its viewport, content size and offset every prepaint.
/// A drag or click on the scrollbar records the offset it wants, which the
/// view applies (and clamps) on its next prepaint; the scrollbar repaints the
/// view right after asking. Offsets are negative, like a `ScrollHandle`'s.
#[derive(Clone, Default)]
pub struct ScrollMetrics(Rc<RefCell<Metrics>>);

#[derive(Default)]
struct Metrics {
    viewport: Bounds<Pixels>,
    content: Size<Pixels>,
    offset: Point<Pixels>,
    requested: Option<Point<Pixels>>,
}

impl ScrollMetrics {
    pub fn publish(&self, viewport: Bounds<Pixels>, content: Size<Pixels>, offset: Point<Pixels>) {
        let mut m = self.0.borrow_mut();
        m.viewport = viewport;
        m.content = content;
        m.offset = offset;
    }

    /// The offset the scrollbar asked for since the last call, if any.
    pub fn take_request(&self) -> Option<Point<Pixels>> {
        self.0.borrow_mut().requested.take()
    }

    /// How far down `offset` is through the last published range, 0 to 1.
    pub fn ratio_y(&self, offset: Point<Pixels>) -> f64 {
        let m = self.0.borrow();
        let range = m.content.height - m.viewport.size.height;
        if range <= px(0.) {
            return 0.;
        }
        ((-offset.y / range) as f64).clamp(0., 1.)
    }
}

impl ScrollbarHandle for ScrollMetrics {
    fn viewport_bounds(&self) -> Bounds<Pixels> {
        self.0.borrow().viewport
    }

    fn offset(&self) -> Point<Pixels> {
        let m = self.0.borrow();
        m.requested.unwrap_or(m.offset)
    }

    fn set_offset(&self, offset: Point<Pixels>) {
        let mut m = self.0.borrow_mut();
        let max_x = (m.content.width - m.viewport.size.width).max(px(0.));
        let max_y = (m.content.height - m.viewport.size.height).max(px(0.));
        m.requested = Some(point(
            offset.x.max(-max_x).min(px(0.)),
            offset.y.max(-max_y).min(px(0.)),
        ));
    }

    fn content_size(&self) -> Size<Pixels> {
        self.0.borrow().content
    }
}

/// A vertical scrollbar in the app's style. See [`style`].
pub fn vertical<H: ScrollbarHandle + Clone>(handle: &H, id: ElementId, cx: &App) -> Scrollbar {
    style(Scrollbar::vertical(handle).id(id), cx)
}

/// Vertical and horizontal scrollbars in the app's style. See [`style`].
pub fn both<H: ScrollbarHandle + Clone>(handle: &H, id: ElementId, cx: &App) -> Scrollbar {
    style(Scrollbar::new(handle).id(id), cx)
}

/// Like macOS: a rounded grey thumb that thickens under the pointer or while
/// dragged, over a track that only shows while the pointer is on it. With
/// the system's overlay scrollers (the trackpad default) it appears while
/// scrolling or when the pointer reaches the edge; with "Show scroll bars:
/// Always" it stays (Windows has the same setting). gpui can't read it on
/// Linux and says "never hide", so Linux keeps the overlay kind.
fn style(bar: Scrollbar, cx: &App) -> Scrollbar {
    let mode = if !cfg!(target_os = "linux") && !cx.should_auto_hide_scrollbars() {
        ScrollbarMode::Always
    } else {
        ScrollbarMode::Hover
    };
    let palette = theme::palette(cx);
    let ink = if palette.is_dark {
        gpui::white()
    } else {
        gpui::black()
    };
    let (track, border) = (palette.code_bg.opacity(0.9), palette.border);
    bar.mode(mode).styles(|s| {
        s.thumb(|t| {
            t.bg(ink.opacity(0.35))
                .width(px(7.))
                .inset(px(4.))
                .radius(px(8.))
        })
        .thumb_hover(|t| {
            t.bg(ink.opacity(0.5))
                .width(px(9.))
                .inset(px(3.5))
                .radius(px(8.))
        })
        .thumb_active(|t| {
            t.bg(ink.opacity(0.55))
                .width(px(9.))
                .inset(px(3.5))
                .radius(px(8.))
        })
        .track_hover(|t| t.bg(track).border_color(border))
        .track_active(|t| t.bg(track).border_color(border))
    })
}

/// A scrollbar id unique to one view, so each tab keeps its own fade and
/// hover state.
pub fn id(name: &'static str, entity: gpui::EntityId) -> ElementId {
    ElementId::NamedInteger(name.into(), entity.as_u64())
}

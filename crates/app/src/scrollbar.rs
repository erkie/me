//! Every scrolling view uses gpui-component's stock [`Scrollbar`], styled by
//! its theme: it fades in while scrolling, can be dragged or clicked, and
//! follows the system's "Show scroll bars" setting.
//!
//! Views that scroll a gpui [`gpui::ScrollHandle`] hand it to the scrollbar
//! directly. The editor and the CSV table paint and scroll themselves, so
//! they share their geometry through [`ScrollMetrics`] instead.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{Bounds, ElementId, Pixels, Point, Size, point, px};
pub use gpui_component::scroll::Scrollbar;
use gpui_component::scroll::ScrollbarHandle;

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

/// A scrollbar id unique to one view, so each tab keeps its own fade and
/// hover state.
pub fn id(name: &'static str, entity: gpui::EntityId) -> ElementId {
    ElementId::NamedInteger(name.into(), entity.as_u64())
}

// SPDX-License-Identifier: MIT OR Apache-2.0
//! Measure widget bounds as they are drawn: [`Measure`], a widget
//! [`Operation`] that collects the bounds of every identified widget,
//! corrected for the scrollables it sits inside.
//!
//! A widget operation walks the live tree, so the rects reflect the frame
//! on screen, not a reconstruction. Scrolling is the one correction that
//! matters: iced lays a scrollable's content out at its full height and
//! translates it when drawing, so a row inside a scrolled list reports
//! where it is drawn, not where it was laid out. What a host does with the
//! rects (hit testing, pixel checks, driving assistive tech) is not this
//! module's business.
//!
//! A widget that is not built at all (hidden, inside a hidden parent, or a
//! template) simply has no entry in `found`; unlike a retained tree, iced
//! keeps no layout for a widget it does not build.

use std::collections::HashMap;

use iced_core::widget::operation::Scrollable;
use iced_core::widget::{Id, Operation};
use iced_core::{Rectangle, Vector};

/// Every identified widget's bounds, as drawn (scroll offsets applied).
///
/// Run it with [`iced_runtime::shell::shell`]-driven widget operations the
/// usual way (the host's runtime exposes `operate`; `iced`'s
/// `Application::run_with` does it for `Task`s returned from `update`).
///
/// ```no_run
/// use toolkit::measure::Measure;
///
/// let mut measure = Measure::default();
/// // runtime.operate(&mut measure);
/// if let Some(rect) = measure.found.get(&iced_core::widget::Id::new("row-7")) {
///     // rect is where "row-7" is drawn, scroll included
/// }
/// ```
#[derive(Default)]
pub struct Measure {
    /// The accumulated scroll translation of the scrollables we are inside.
    offset: Vector,
    /// A scrollable's translation, applied to the traversal that follows it.
    pending: Option<Vector>,
    /// Bounds by widget id, as drawn.
    pub found: HashMap<Id, Rectangle>,
}

impl Operation for Measure {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        let shift = self.pending.take().unwrap_or(Vector::new(0.0, 0.0));
        self.offset += shift;
        operate(self);
        self.offset -= shift;
    }

    fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
        if let Some(id) = id {
            let shown = Rectangle {
                x: bounds.x - self.offset.x,
                y: bounds.y - self.offset.y,
                ..bounds
            };
            self.found.insert(id.clone(), shown);
        }
    }

    fn scrollable(
        &mut self,
        _id: Option<&Id>,
        _bounds: Rectangle,
        _content_bounds: Rectangle,
        translation: Vector,
        _state: &mut dyn Scrollable,
    ) {
        self.pending = Some(translation);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoScroll;
    impl Scrollable for NoScroll {
        fn snap_to(
            &mut self,
            _offset: iced_core::widget::operation::scrollable::RelativeOffset<Option<f32>>,
        ) {
        }
        fn scroll_to(
            &mut self,
            _offset: iced_core::widget::operation::scrollable::AbsoluteOffset<Option<f32>>,
        ) {
        }
        fn scroll_by(
            &mut self,
            _offset: iced_core::widget::operation::scrollable::AbsoluteOffset,
            _bounds: Rectangle,
            _content_bounds: Rectangle,
        ) {
        }
    }

    #[test]
    fn rects_carry_the_scroll_offset_of_their_scrollable() {
        let mut measure = Measure::default();
        let pane = Id::new("pane");
        measure.container(Some(&pane), Rectangle { x: 0.0, y: 32.0, width: 720.0, height: 488.0 });
        // A scrolled list: the rows inside report where they are drawn.
        measure.scrollable(
            None,
            Rectangle::default(),
            Rectangle::default(),
            Vector::new(0.0, 10.0),
            &mut NoScroll,
        );
        let row = Id::new("row-1");
        measure.traverse(&mut |op| {
            op.container(Some(&row), Rectangle { x: 14.0, y: 100.0, width: 692.0, height: 38.0 });
        });
        // After the traversal the offset is back to zero: a sibling of the
        // scrollable is measured unshifted.
        let footer = Id::new("footer");
        measure.container(Some(&footer), Rectangle { x: 0.0, y: 520.0, width: 720.0, height: 24.0 });
        assert_eq!(
            measure.found.get(&pane),
            Some(&Rectangle { x: 0.0, y: 32.0, width: 720.0, height: 488.0 })
        );
        assert_eq!(
            measure.found.get(&row),
            Some(&Rectangle { x: 14.0, y: 90.0, width: 692.0, height: 38.0 }),
            "the row is reported 10 px higher: the list is scrolled down"
        );
        assert_eq!(
            measure.found.get(&footer),
            Some(&Rectangle { x: 0.0, y: 520.0, width: 720.0, height: 24.0 })
        );
    }

    #[test]
    fn unidentified_widgets_leave_no_entry() {
        let mut measure = Measure::default();
        measure.container(None, Rectangle { x: 0.0, y: 0.0, width: 10.0, height: 10.0 });
        assert!(measure.found.is_empty());
    }
}

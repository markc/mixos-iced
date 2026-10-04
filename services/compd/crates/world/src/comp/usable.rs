//! New-window placement inside the usable area: a new toplevel is placed
//! inside the usable output rectangle, so a panel's exclusive zone never
//! covers a window that opens.
//!
//! With the camera pinned to identity (`camera::pin`), a world position IS an
//! output-logical position, so the default output's usable area (host Space,
//! logical) bounds a new window directly. With no layer zones the usable area
//! is the whole output, and a window that fits stays where the engine put it.

use smithay::utils::{Logical, Rectangle, Size};

/// The space compositor-drawn panels reserve on one output's edges (logical
/// px): a DOCKED Mix Scenes panel edge, as Quoin's layer exclusive zone did.
/// A pinned edge is an overlay and reserves nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Reserved {
    pub top: i32,
    pub bottom: i32,
    pub left: i32,
    pub right: i32,
}

impl Reserved {
    /// Add `px` on `edge` ("top" | "bottom" | "left" | "right"; anything else
    /// is ignored), rounded up to whole logical px so a fractional panel never
    /// overlaps a window by a sub-pixel.
    pub fn add(&mut self, edge: &str, px: f32) {
        let px = if px.is_finite() && px > 0.0 { px.ceil() as i32 } else { 0 };
        let slot = match edge {
            "top" => &mut self.top,
            "bottom" => &mut self.bottom,
            "left" => &mut self.left,
            "right" => &mut self.right,
            _ => return,
        };
        *slot = slot.saturating_add(px);
    }

    /// `area` less these edges, stacked inside what the layer zones already
    /// left (never a negative size).
    pub fn shrink(&self, area: Rectangle<i32, Logical>) -> Rectangle<i32, Logical> {
        let w = area.size.w.saturating_sub(self.left).saturating_sub(self.right).max(0);
        let h = area.size.h.saturating_sub(self.top).saturating_sub(self.bottom).max(0);
        Rectangle::new((area.loc.x + self.left, area.loc.y + self.top).into(), (w, h).into())
    }
}

/// `at` (a window's world position) moved so a window of `size` lies inside
/// the default output's usable area; unchanged when the area is not known
/// yet. A window larger than the area opens at its top-left.
pub fn inside_usable(lp: &crate::state::Loop, at: (f64, f64), size: Size<i32, Logical>) -> (f64, f64) {
    let Some(area) = lp.inner.comp.default_usable() else {
        return at;
    };
    let clamp = |value: f64, low: i32, extent: i32, inside: i32| {
        let (low, high) = (f64::from(low), f64::from(low) + f64::from(extent));
        let inside = f64::from(inside);
        if high - low >= inside {
            value.clamp(low, high - inside)
        } else {
            low
        }
    };
    (
        clamp(at.0, area.loc.x, area.size.w, size.w),
        clamp(at.1, area.loc.y, area.size.h, size.h),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The gate case: the bottom edge docked at 80 px on 1280x800 leaves
    /// 1280x720; fractions round up; edges stack inside what layers left.
    #[test]
    fn reserved_edges_shrink_the_area() {
        let whole = Rectangle::<i32, Logical>::new((0, 0).into(), (1280, 800).into());
        let mut bottom = Reserved::default();
        bottom.add("bottom", 80.0);
        assert_eq!(bottom.shrink(whole), Rectangle::<i32, Logical>::new((0, 0).into(), (1280, 720).into()));
        let mut sides = Reserved::default();
        sides.add("left", 47.2);
        sides.add("top", 30.0);
        sides.add("sideways", 9.0);
        sides.add("right", f32::NAN);
        assert_eq!(sides, Reserved { top: 30, bottom: 0, left: 48, right: 0 });
        let layered = Rectangle::<i32, Logical>::new((0, 24).into(), (1280, 776).into());
        assert_eq!(sides.shrink(layered), Rectangle::<i32, Logical>::new((48, 54).into(), (1232, 746).into()));
        let mut huge = Reserved::default();
        huge.add("top", 900.0);
        assert_eq!(huge.shrink(whole).size, Size::<i32, Logical>::from((1280, 0)));
        assert_eq!(Reserved::default().shrink(whole), whole);
    }
}

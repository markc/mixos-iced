//! X11 initial placement on compd: the host half of
//! `policy::x11::initial_geometry`.
//!
//! The engine's own placement treats an X11 toplevel like any window
//! (parent-relative, else the camera centre); the policy honours the
//! client's own origin when it is inside the
//! usable area (`xmessage -center`), else cascades, pushes the origin in by
//! the chrome, and clamps the size by WM_NORMAL_HINTS and the room left.
//! frames's `_initial_mapped` asks [`initial`] for a managed, unparented
//! X11 window.

use policy::x11::{Area, Extents, InitialPlacement, initial_geometry};
use smithay::desktop::Window;
use smithay::utils::{Logical, Point, Size};

use crate::state::Loop;

/// The content origin and size a newly mapped managed X11 `window` takes
/// (host Space, logical), or `None` for anything else (xdg, override-redirect,
/// no usable area yet), which keeps the engine's own placement.
pub fn initial(lp: &mut Loop, window: &Window) -> Option<(Point<i32, Logical>, Size<i32, Logical>)> {
    let x11 = window.x11_surface()?;
    if x11.is_override_redirect() {
        return None;
    }
    let usable = lp.inner.comp.default_usable()?;
    // The client's own X geometry (CreateWindow, plus any granted configure):
    // `geometry()` is bbox-relative (its origin is only the frame extents), and
    // `shell::stage` has not yet moved the window to `shell::X11_ORIGIN`.
    let geometry = x11.last_configure();
    let placement = InitialPlacement {
        usable: Area {
            x: usable.loc.x as f32,
            y: usable.loc.y as f32,
            width: usable.size.w as f32,
            height: usable.size.h as f32,
        },
        requested_origin: (geometry.loc.x, geometry.loc.y),
        requested_size: (geometry.size.w, geometry.size.h),
        min_hint: x11.min_size().map(|size| (size.w, size.h)),
        max_hint: x11.max_size().map(|size| (size.w, size.h)),
        extents: decor::window::extents(window).map(|extents| Extents {
            top: extents.top,
            left: extents.left,
            right: extents.right,
            bottom: extents.bottom,
        }),
        cascade_index: lp.inner.comp.x11_cascade,
    };
    let placed = initial_geometry(placement);
    if placed.cascaded {
        lp.inner.comp.x11_cascade = lp.inner.comp.x11_cascade.wrapping_add(1);
    }
    Some((Point::from(placed.origin), Size::from(placed.size)))
}

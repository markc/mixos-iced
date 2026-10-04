//! The seat's side of the chrome: hover, press and release, kept per
//! compositor thread. The pointer handlers call in here with what the hit test
//! found ([`crate::window::hit`]) and act on what comes back: a redraw, a
//! cursor override, or an [`Intent`] to turn into the request a client's own
//! move / resize / close / maximise / minimise would make.

use std::cell::RefCell;

use crate::layout::{ChromePart, ResizeEdge};
use smithay::desktop::Window;
use smithay::input::pointer::CursorIcon;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;

use crate::input::{Clicks, Intent};
use crate::window::ChromeHit;

#[derive(Default)]
struct Seat {
    clicks: Clicks<Window>,
    /// The window whose chrome holds hover state, if any.
    hovered: Option<Window>,
    /// The cursor this module put in the seat's override, if any.
    cursor: Option<CursorIcon>,
}

thread_local! {
    static SEAT: RefCell<Seat> = RefCell::new(Seat::default());
}

/// What a pointer motion asks of the host.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hover {
    /// A window's chrome changed (hover or cluster hover): a frame is owed.
    pub redraw: bool,
    /// Put this cursor in the seat's override (over a resize edge).
    pub set_cursor: Option<CursorIcon>,
    /// Take this cursor back out of the override, if it is still the one
    /// there (the pointer left the edge; something else may own it since).
    pub clear_cursor: Option<CursorIcon>,
}

fn resize_cursor(edge: ResizeEdge) -> CursorIcon {
    match edge {
        ResizeEdge::Top => CursorIcon::NResize,
        ResizeEdge::Bottom => CursorIcon::SResize,
        ResizeEdge::Left => CursorIcon::WResize,
        ResizeEdge::Right => CursorIcon::EResize,
        ResizeEdge::TopLeft => CursorIcon::NwResize,
        ResizeEdge::TopRight => CursorIcon::NeResize,
        ResizeEdge::BottomLeft => CursorIcon::SwResize,
        ResizeEdge::BottomRight => CursorIcon::SeResize,
    }
}

pub(crate) fn maximized(window: &Window) -> bool {
    if let Some(x11) = window.x11_surface() {
        return x11.is_maximized();
    }
    window.toplevel().is_some_and(|toplevel| {
        toplevel.with_committed_state(|s| {
            s.is_some_and(|s| s.states.contains(xdg_toplevel::State::Maximized))
        })
    })
}

/// The pointer moved; `target` is the chrome under it, if any. Moves the
/// hover off the previous window's chrome and onto this one's.
pub fn hover(target: Option<(&Window, ChromeHit)>) -> Hover {
    SEAT.with_borrow_mut(|seat| {
        let mut out = Hover::default();
        let pressed = |seat: &Seat, window: &Window| seat.clicks.pressed(window);
        if let Some(previous) = seat.hovered.clone()
            && target.is_none_or(|(window, _)| *window != previous)
        {
            let held = pressed(seat, &previous);
            out.redraw |= crate::render::set_pointer(&previous, None, false, held);
            seat.hovered = None;
        }
        if let Some((window, hit)) = target {
            let button = match hit.part {
                ChromePart::Button(button) => Some(button),
                _ => None,
            };
            let held = pressed(seat, window);
            out.redraw |= crate::render::set_pointer(window, button, hit.in_cluster, held);
            seat.hovered = Some(window.clone());
        }
        let wanted = target.and_then(|(_, hit)| match hit.part {
            ChromePart::Resize(edge) => Some(resize_cursor(edge)),
            _ => None,
        });
        if wanted != seat.cursor {
            out.clear_cursor = seat.cursor.filter(|_| wanted.is_none());
            out.set_cursor = wanted;
            seat.cursor = wanted;
        }
        out
    })
}

/// A primary-button press on `hit` of `window`, at event `time` (ms) and
/// `content_relative` to the slot. Returns the intent to start now: a move or
/// resize grab, or a maximise toggle on a titlebar double-click. A caption
/// button is armed instead (it fires on [`release`]) and shown pressed.
pub fn press(
    window: &Window,
    hit: ChromeHit,
    content_relative: (f64, f64),
    time: u32,
) -> Option<Intent> {
    SEAT.with_borrow_mut(|seat| {
        let position = crate::layout::vec2(content_relative.0 as f32, content_relative.1 as f32);
        let intent = seat
            .clicks
            .press(window, hit.part, position, time, maximized(window));
        if let ChromePart::Button(button) = hit.part {
            let hovered = Some(button);
            crate::render::set_pointer(window, hovered, hit.in_cluster, Some(button));
        }
        intent
    })
}

/// The primary button released over `target` (the chrome under the pointer,
/// if any). Fires an armed caption button only if the release is on the same
/// button of the same window; always clears the pressed look. Returns the
/// window and what to do.
pub fn release(target: Option<(&Window, ChromeHit)>) -> Option<(Window, Intent)> {
    SEAT.with_borrow_mut(|seat| {
        let armed_window = seat.clicks.armed().map(|(window, _)| window.clone());
        let part = target.map_or(ChromePart::Outside, |(_, hit)| hit.part);
        let intent = seat.clicks.release(target.map(|(w, _)| w), part);
        if let Some(window) = &armed_window {
            let hovered = match (target, part) {
                (Some((w, _)), ChromePart::Button(button)) if w == window => Some(button),
                _ => None,
            };
            let cluster = target.is_some_and(|(w, hit)| w == window && hit.in_cluster);
            crate::render::set_pointer(window, hovered, cluster, None);
        }
        armed_window.zip(intent)
    })
}

/// `window` is gone or lost its chrome: forget its hover, armed button and
/// double-click.
pub fn forget(window: &Window) {
    SEAT.with_borrow_mut(|seat| {
        seat.clicks.forget(window);
        if seat.hovered.as_ref() == Some(window) {
            seat.hovered = None;
        }
    });
}

/// The surface behind `handle` went dormant or was destroyed (the registry
/// forgets it): drop whatever the seat holds of its window.
pub fn forget_handle(handle: &dispatcher::wire::trait_::surface_event::SurfaceHandle) {
    let named = |window: &Window| {
        dispatcher::wire::trait_::surface_event::SurfaceHandle::of_window(window).as_ref()
            == Some(handle)
    };
    SEAT.with_borrow_mut(|seat| {
        seat.clicks.forget_if(named);
        if seat.hovered.as_ref().is_some_and(named) {
            seat.hovered = None;
        }
    });
}

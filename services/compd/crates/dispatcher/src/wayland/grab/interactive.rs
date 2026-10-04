//! The interactive move/resize grab (xdg `move` / `resize`).
//!
//! World-free like every handler here: the grab only reports where the pointer
//! is relative to where it began, as [`InteractiveOp`] registry events. The host
//! (policy-host `control::apply_interactive`, after the drain) moves or
//! resizes the window and sends the configures, so the grab never touches a
//! Space.

use smithay::input::pointer::{
    AxisFrame, ButtonEvent, Focus, GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent,
    GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent,
    GestureSwipeUpdateEvent, GrabStartData, MotionEvent, PointerGrab, PointerInnerHandle,
    RelativeMotionEvent,
};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, SERIAL_COUNTER, Serial};
use smithay::wayland::compositor::get_parent;

use crate::state::state::Dispatch;
use crate::wire::trait_::surface_event::{InteractiveOp, SurfaceEvent, SurfaceHandle};

pub struct InteractiveGrab {
    start_data: GrabStartData<Dispatch>,
    handle: SurfaceHandle,
    ended: bool,
}

impl InteractiveGrab {
    fn end(&mut self, data: &mut Dispatch) {
        if !self.ended {
            self.ended = true;
            data.push_surface_event(SurfaceEvent::Interactive {
                handle: self.handle.clone(),
                op: InteractiveOp::End,
            });
        }
    }
}

/// The root of a surface tree (a subsurface's toplevel).
fn root_of(surface: &WlSurface) -> WlSurface {
    let mut root = surface.clone();
    while let Some(parent) = get_parent(&root) {
        root = parent;
    }
    root
}

/// Start a grab on `surface` (the window's own surface; `handle` its record)
/// if the primary seat's pointer holds the implicit grab that `serial` names
/// (xdg) or any implicit grab (X11 `_NET_WM_MOVERESIZE`, which carries no
/// serial), and that grab began over this window. Returns whether it started.
pub fn start(
    dispatch: &mut Dispatch,
    surface: &WlSurface,
    handle: SurfaceHandle,
    serial: Option<Serial>,
    edges: u32,
) -> bool {
    let Some(pointer) = dispatch.seat.seat.get_pointer() else {
        return false;
    };
    if serial.is_some_and(|serial| !pointer.has_grab(serial)) {
        return false;
    }
    let Some(start_data) = pointer.grab_start_data() else {
        return false;
    };
    let over_window = start_data
        .focus
        .as_ref()
        .is_some_and(|(focus, _)| root_of(focus) == *surface);
    if !over_window {
        return false;
    }
    dispatch.push_surface_event(SurfaceEvent::Interactive {
        handle: handle.clone(),
        op: InteractiveOp::Begin { edges },
    });
    let grab = InteractiveGrab {
        start_data,
        handle,
        ended: false,
    };
    let serial = serial.unwrap_or_else(|| SERIAL_COUNTER.next_serial());
    pointer.set_grab(dispatch, grab, serial, Focus::Clear);
    true
}

/// Start the same grab from compd's own chrome (decor): a press on a
/// window's titlebar (`edges` 0, a move) or resize band (the edge bits). The
/// press hit compositor pixels, so the implicit click grab it opened has no
/// client focus and the `over_window` check of [`start`] cannot apply; the
/// chrome's own hit test already named the window. Any implicit grab will do
/// (a button is down: the chrome press just went to the seat). Returns
/// whether it started.
pub fn start_chrome(dispatch: &mut Dispatch, handle: SurfaceHandle, edges: u32) -> bool {
    let Some(pointer) = dispatch.seat.seat.get_pointer() else {
        return false;
    };
    let Some(start_data) = pointer.grab_start_data() else {
        return false;
    };
    dispatch.push_surface_event(SurfaceEvent::Interactive {
        handle: handle.clone(),
        op: InteractiveOp::Begin { edges },
    });
    let grab = InteractiveGrab {
        start_data,
        handle,
        ended: false,
    };
    pointer.set_grab(dispatch, grab, SERIAL_COUNTER.next_serial(), Focus::Clear);
    true
}

impl PointerGrab<Dispatch> for InteractiveGrab {
    fn motion(
        &mut self,
        data: &mut Dispatch,
        handle: &mut PointerInnerHandle<'_, Dispatch>,
        _focus: Option<(WlSurface, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        // No client focus while the window is carried.
        handle.motion(data, None, event);
        let delta = event.location - self.start_data.location;
        data.push_surface_event(SurfaceEvent::Interactive {
            handle: self.handle.clone(),
            op: InteractiveOp::Update {
                dx: delta.x,
                dy: delta.y,
            },
        });
    }
    fn relative_motion(
        &mut self,
        data: &mut Dispatch,
        handle: &mut PointerInnerHandle<'_, Dispatch>,
        _focus: Option<(WlSurface, Point<f64, Logical>)>,
        event: &RelativeMotionEvent,
    ) {
        handle.relative_motion(data, None, event);
    }
    fn button(&mut self, data: &mut Dispatch, handle: &mut PointerInnerHandle<'_, Dispatch>, event: &ButtonEvent) {
        handle.button(data, event);
        if handle.current_pressed().is_empty() {
            self.end(data);
            handle.unset_grab(self, data, event.serial, event.time, true);
        }
    }
    fn axis(&mut self, data: &mut Dispatch, handle: &mut PointerInnerHandle<'_, Dispatch>, details: AxisFrame) {
        handle.axis(data, details)
    }
    fn frame(&mut self, data: &mut Dispatch, handle: &mut PointerInnerHandle<'_, Dispatch>) {
        handle.frame(data);
    }
    fn gesture_swipe_begin(&mut self, data: &mut Dispatch, handle: &mut PointerInnerHandle<'_, Dispatch>, event: &GestureSwipeBeginEvent) { handle.gesture_swipe_begin(data, event) }
    fn gesture_swipe_update(&mut self, data: &mut Dispatch, handle: &mut PointerInnerHandle<'_, Dispatch>, event: &GestureSwipeUpdateEvent) { handle.gesture_swipe_update(data, event) }
    fn gesture_swipe_end(&mut self, data: &mut Dispatch, handle: &mut PointerInnerHandle<'_, Dispatch>, event: &GestureSwipeEndEvent) { handle.gesture_swipe_end(data, event) }
    fn gesture_pinch_begin(&mut self, data: &mut Dispatch, handle: &mut PointerInnerHandle<'_, Dispatch>, event: &GesturePinchBeginEvent) { handle.gesture_pinch_begin(data, event) }
    fn gesture_pinch_update(&mut self, data: &mut Dispatch, handle: &mut PointerInnerHandle<'_, Dispatch>, event: &GesturePinchUpdateEvent) { handle.gesture_pinch_update(data, event) }
    fn gesture_pinch_end(&mut self, data: &mut Dispatch, handle: &mut PointerInnerHandle<'_, Dispatch>, event: &GesturePinchEndEvent) { handle.gesture_pinch_end(data, event) }
    fn gesture_hold_begin(&mut self, data: &mut Dispatch, handle: &mut PointerInnerHandle<'_, Dispatch>, event: &GestureHoldBeginEvent) { handle.gesture_hold_begin(data, event) }
    fn gesture_hold_end(&mut self, data: &mut Dispatch, handle: &mut PointerInnerHandle<'_, Dispatch>, event: &GestureHoldEndEvent) { handle.gesture_hold_end(data, event) }
    fn start_data(&self) -> &GrabStartData<Dispatch> {
        &self.start_data
    }
    fn unset(&mut self, data: &mut Dispatch) {
        self.end(data);
    }
}

//! Frame callbacks while compd cannot present: the session is paused (its VT is
//! in the background), the panel is powered down, or no output is live.
//!
//! Every presenting path answers frame callbacks after a flip. With no flip,
//! nothing answers them, and a client that paces on callbacks stops. A client
//! started while the VT is hidden commits one buffer and its UI loop never runs
//! again, so anything it does in that loop (control commands, screenshots, its
//! own GPU-start watchdog) stalls until the VT comes back — and a watchdog that
//! times out reads the stall as a crashed GPU start.
//!
//! A parked desktop is treated as fully occluded: every surface still waiting on
//! a callback gets one at most every [`PARKED_TRICKLE`], the rate the occluded
//! trickle gives a covered window. The timer is armed from the main loop pass
//! that every client request wakes (not from the skipped frame: a VT switch
//! that leaves pipes in flight swallows the redraw pings that would reach it),
//! fires once, and re-arms only while something still waits. It never
//! schedules a redraw, so an idle parked desktop costs nothing.

use crate::draw::present::callbacks::callbacks::{surface_awaits_frame, tree_awaits_frame, OCCLUDED_TRICKLE};
use smithay::desktop::layer_map_for_output;
use smithay::desktop::utils::send_frames_surface_tree;
use smithay::output::Output;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::seat::WaylandFocus;
use std::time::Duration;
use world::state::Loop;

/// One callback per waiting surface at most this often while frames are parked.
pub const PARKED_TRICKLE: Duration = OCCLUDED_TRICKLE;

thread_local! {
    /// A parked-trickle timer is armed (one at a time).
    static ARMED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The output callbacks are attributed to: the active one, else any mapped one.
fn target_output(state: &Loop) -> Option<Output> {
    let space = &state.inner.space_state().state;
    let active = state.inner.active_output_key();
    space
        .outputs()
        .find(|o| world::state::state::output_key(o) == active)
        .or_else(|| space.outputs().next())
        .cloned()
}

/// The compositor-drawn surfaces (cursor, drag icon) that hold a callback.
fn drawn_surfaces(state: &Loop) -> Vec<WlSurface> {
    let mut out = Vec::new();
    if let smithay::input::pointer::CursorImageStatus::Surface(surface) = &state.state.seat.pointer_status {
        out.push(surface.clone());
    }
    if let Some(icon) = &state.state.dnd.icon {
        out.push(icon.clone());
    }
    out.retain(surface_awaits_frame);
    out
}

/// Whether any surface would get a callback from [`send`].
fn waiting(state: &Loop) -> bool {
    if !drawn_surfaces(state).is_empty() {
        return true;
    }
    // Under a session lock only the lock surface gets callbacks; on a parked
    // desktop it redraws on resume, so windows and layers wait with it.
    if world::comp::session_lock::active(state) {
        return false;
    }
    let space = &state.inner.space_state().state;
    space.elements().any(|w| w.wl_surface().is_some_and(|s| tree_awaits_frame(&s)))
        || space.outputs().any(|o| {
            layer_map_for_output(o)
                .layers()
                .any(|l| !world::comp::panels::concealed(l.wl_surface()) && tree_awaits_frame(l.wl_surface()))
        })
}

/// Answer every waiting callback once. The timer is the rate limit, so no
/// smithay throttle: a throttle equal to the period would drop alternate
/// sends to timer jitter.
fn send(state: &Loop) {
    let Some(output) = target_output(state) else { return };
    let frame_time = state.inner.start_time.elapsed();
    for surface in drawn_surfaces(state) {
        send_frames_surface_tree(&surface, &output, frame_time, None, |_, _| Some(output.clone()));
    }
    if world::comp::session_lock::active(state) {
        return;
    }
    let space = &state.inner.space_state().state;
    for window in space.elements() {
        window.send_frame(&output, frame_time, None, |_, _| Some(output.clone()));
    }
    for o in space.outputs() {
        for layer in layer_map_for_output(o).layers() {
            if world::comp::panels::concealed(layer.wl_surface()) {
                continue;
            }
            layer.send_frame(o, frame_time, None, |_, _| Some(o.clone()));
        }
    }
}

/// Arm the parked trickle if frames are parked and some surface waits on a
/// callback. Called on every main-loop pass while parked. `parked` is the backend's own
/// answer (`native.render.execute.frames_parked`), read again when the timer
/// fires: once presentation resumes, the flip answers callbacks and the timer
/// drops without sending.
pub fn arm(state: &Loop, parked: fn(&Loop) -> bool) {
    if ARMED.get() || !parked(state) || !waiting(state) {
        return;
    }
    let armed = state.loop_handle.insert_source(
        Timer::from_duration(PARKED_TRICKLE),
        move |_, _, state: &mut Loop| {
            ARMED.set(false);
            if parked(state) {
                send(state);
                let _ = state.inner.loader.display_handle.flush_clients();
                arm(state, parked);
            }
            TimeoutAction::Drop
        },
    );
    match armed {
        Ok(_) => ARMED.set(true),
        Err(error) => warn!("parked frame trickle not armed: {error}"),
    }
}

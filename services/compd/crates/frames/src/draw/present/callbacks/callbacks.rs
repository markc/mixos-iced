//! Presentation-feedback collection, frame callbacks, and post-frame
//! housekeeping. Replaces the `refresh()` blocks duplicated in both backends.

use smithay::backend::renderer::element::RenderElementStates;
use smithay::desktop::utils::{surface_presentation_feedback_flags_from_states, OutputPresentationFeedback};
use smithay::desktop::{layer_map_for_output, Window};
use smithay::output::Output;
use smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback;
use std::time::Duration;
use world::state::Loop;

/// The presentation Kind flags this compositor reports for a hardware flip.
///
/// `Vsync` asserts synchronization to the vertical retrace, so it MUST be dropped
/// for an async-flipped frame — clients (Mesa's WSI, engine frame pacers) read it
/// to detect tearing and adapt their pacing, and reporting it on a torn frame
/// feeds them a false signal.
///
/// `HwClock` and `HwCompletion` stay set for BOTH, deliberately. Neither claims
/// anything about retrace: they say the timestamp came from the display hardware
/// and that the hardware signalled the presentation, and an async flip satisfies
/// both — the kernel's page-flip event IS the hardware saying the new buffer
/// began scanning out. Dropping them on a torn frame would replace one true
/// statement with a vaguer one; `Vsync` is the only flag that was ever wrong. The
/// other half of the same honesty is `Refresh::Unknown` at `wire.frame`: a torn
/// frame genuinely has no predictable next presentation.
pub fn hw_flip_kind(tearing: bool) -> wp_presentation_feedback::Kind {
    let hw = wp_presentation_feedback::Kind::HwClock | wp_presentation_feedback::Kind::HwCompletion;
    if tearing { hw } else { wp_presentation_feedback::Kind::Vsync | hw }
}

/// Collect presentation feedback for the windows visible in the frame about to be
/// queued. `states` are THIS frame's `RenderFrameResult::states`, threaded here
/// for one reason: `ZeroCopy` is per-SURFACE and unknowable at `presented()`
/// time, so it is the one flag that must be stored rather than passed.
pub fn collect_feedback(
    output: &Output,
    visible: &[Window],
    states: Option<&RenderElementStates>,
) -> OutputPresentationFeedback {
    let mut feedback = OutputPresentationFeedback::new(output);
    for window in visible {
        window.take_presentation_feedback(
            &mut feedback,
            |_, _| Some(output.clone()),
            // `ZeroCopy` AND NOTHING ELSE: smithay ORs what is stored here into what
            // `presented()` passes (`utils.rs`: `callback.presented(..., flags |
            // self.flags)`) rather than replacing it, so a flag stored here can never
            // be cleared later. Storing `hw_flip_kind(false)` once pinned `Vsync` on
            // permanently — a torn frame passing `hw_flip_kind(true)` had it ORed
            // straight back in. `ZeroCopy` belongs here because it is a fact about
            // THIS surface in THIS frame that per-output `presented()` cannot know.
            |s, _| states.map_or(wp_presentation_feedback::Kind::empty(), |st| {
                surface_presentation_feedback_flags_from_states(s, None, st)
            }),
        );
    }
    feedback
}

/// Send frame callbacks to the visible windows. `throttle` follows the
/// caller's existing behavior (`Some(Duration::ZERO)` in both backends today).
///
/// An occluded window (on a pane, covered, not drawn: F6) is not in `visible`.
/// It is counted as a withheld opportunity here and gets the trickle
/// instead ([`trickle`]): one callback at most every [`OCCLUDED_TRICKLE`].
pub fn send_window_frames(state: &Loop, output: &Output, visible: &[Window]) {
    let frame_time = state.inner.start_time.elapsed();
    for window in visible {
        window.send_frame(output, frame_time, Some(Duration::ZERO), |_, _| {
            Some(output.clone())
        });
    }
    let occluded = world::window::draw::occlude::record::occluded();
    world::window::draw::occlude::record::note_withheld(occluded.len());
    trickle(state);
}

/// The occluded trickle: a covered window still gets a frame callback
/// this often, so a client that paces on callbacks (video, games, anything on
/// `wp_presentation`) keeps running instead of stalling until it is uncovered.
pub const OCCLUDED_TRICKLE: Duration = Duration::from_secs(1);

thread_local! {
    /// A trickle timer is armed (one at a time).
    static TRICKLE_ARMED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether any surface in `window`'s tree, or its popups' trees, holds a frame
/// callback the client is waiting on.
fn awaits_frame(window: &Window) -> bool {
    use smithay::wayland::seat::WaylandFocus;
    window.wl_surface().is_some_and(|surface| tree_awaits_frame(&surface))
}

/// [`surface_awaits_frame`] for `surface` and each of its popups: the trees
/// `Window::send_frame` and `LayerSurface::send_frame` answer. A popup is its
/// own tree, so a popup-only wait is invisible to the root check alone.
pub fn tree_awaits_frame(surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface) -> bool {
    surface_awaits_frame(surface)
        || smithay::desktop::PopupManager::popups_for_surface(surface)
            .any(|(popup, _)| surface_awaits_frame(popup.wl_surface()))
}

/// Whether any surface in `surface`'s tree holds a frame callback the client is
/// waiting on.
pub fn surface_awaits_frame(surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface) -> bool {
    use smithay::wayland::compositor::{SurfaceAttributes, TraversalAction, with_surface_tree_downward};
    // A Cell: the visit closure sets it and the continue predicate reads it.
    let pending = std::cell::Cell::new(false);
    with_surface_tree_downward(
        surface,
        (),
        |_, _, _| TraversalAction::DoChildren(()),
        |_, states, _| {
            if !states.cached_state.get::<SurfaceAttributes>().current().frame_callbacks.is_empty() {
                pending.set(true);
            }
        },
        |_, _, _| !pending.get(),
    );
    pending.get()
}

/// The occluded windows still waiting on a callback, with an output each was
/// occluded on.
fn trickle_targets(state: &Loop) -> Vec<(Window, Output)> {
    use world::window::interface::record::window::LoopWindow;
    let occluded = world::window::draw::occlude::record::occluded();
    // Under a session lock only the lock surface gets callbacks (batch C), and
    // the cull is not run, so the record's decisions are stale: no trickle.
    if occluded.is_empty() || world::comp::session_lock::active(state) {
        return Vec::new();
    }
    let space = &state.inner.space_state().state;
    occluded
        .into_iter()
        .filter_map(|(uuid, outputs)| {
            let window = space.elements().find(|w| w.uuid() == Some(uuid))?.clone();
            let output = space
                .outputs()
                .find(|o| outputs.iter().any(|key| *key == world::state::state::output_key(o)))?
                .clone();
            awaits_frame(&window).then_some((window, output))
        })
        .collect()
}

/// Arm the trickle when an occluded window waits on a callback: a one-shot
/// timer sends it (throttled by smithay to once per [`OCCLUDED_TRICKLE`] per
/// surface) and re-arms only while some covered window still waits. Never
/// schedules a redraw: a covered window that stays idle costs no frame and,
/// once its callback is answered, no further wake.
pub fn trickle(state: &Loop) {
    if TRICKLE_ARMED.get() || trickle_targets(state).is_empty() {
        return;
    }
    let armed = state.loop_handle.insert_source(
        smithay::reexports::calloop::timer::Timer::from_duration(OCCLUDED_TRICKLE),
        |_, _, state: &mut Loop| {
            TRICKLE_ARMED.set(false);
            let frame_time = state.inner.start_time.elapsed();
            for (window, output) in trickle_targets(state) {
                window.send_frame(&output, frame_time, Some(OCCLUDED_TRICKLE), |_, _| Some(output.clone()));
            }
            let _ = state.inner.loader.display_handle.flush_clients();
            trickle(state);
            smithay::reexports::calloop::timer::TimeoutAction::Drop
        },
    );
    match armed {
        Ok(_) => TRICKLE_ARMED.set(true),
        Err(error) => warn!("occluded-window frame trickle not armed: {error}"),
    }
}

/// Send frame callbacks to the layers of ONE output — the one that just
/// presented. Multi-output: firing every output's layers on any output's flip
/// would pace a slow monitor's bar/panel at a fast neighbour's refresh; layer
/// surfaces are per-output (smithay keys layer maps by `Output`), so each
/// output drives only its own layers on its own vblank.
pub fn send_layer_frames(state: &Loop, output: &Output) {
    // Under a session lock only the lock surface
    // gets frame callbacks, and this presented lock frame counts toward `locked`.
    if world::comp::session_lock::presented(state, output) {
        return;
    }
    let frame_time = state.inner.start_time.elapsed();
    let layer_map = layer_map_for_output(output);
    for layer in layer_map.layers() {
        // A layer comp conceals gets no frame callbacks.
        if world::comp::panels::concealed(layer.wl_surface()) {
            continue;
        }
        layer.send_frame(output, frame_time, None, |_surface, _states| {
            Some(output.clone())
        });
    }
}

/// Post-frame housekeeping: space refresh, popup cleanup, client flush.
/// Runs every frame, damage or no damage.
///
/// `refresh_space` rather than smithay's `Space::refresh()`: the latter derives
/// `wl_output` enter/leave from the window's stored position, which here is a WORLD
/// coordinate and not a screen one — and no window belongs to one output anyway, since
/// every monitor renders the same world through its own camera.
pub fn housekeeping(state: &mut Loop) {
    state.inner.refresh_space();
    // After the frame, so it reads the presence stamps this frame just wrote.
    state.inner.refresh_suspended();
    state.state.popup.state.cleanup();
    let _ = state.inner.loader.display_handle.flush_clients();
}

//! Presentation feedback for a backend with NO hardware page-flip — the nested
//! (winit) path. The native counterpart lives in `present.callbacks`
//! (`hw_flip_kind`) and is driven by the kernel's vblank event; nested there is no
//! event to wait for, so the frame is reported at submit from a software clock.

use smithay::desktop::utils::OutputPresentationFeedback;
use smithay::output::Output;
use smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback;
use smithay::utils::{Clock, Monotonic};
use smithay::wayland::presentation::Refresh;
use std::time::Duration;

/// The Kind flags for a presentation performed without a page-flip: NONE.
/// The nested submit is not a vblank
/// we observed: the host composites when it likes, or drops the frame, and our
/// timestamp is our own `clock_gettime`. `Vsync` would claim a retrace we never
/// saw; `HwClock`/`HwCompletion` would dress a software estimate up as a
/// hardware measurement. A frame pacer reads empty flags as exactly that.
pub fn software_present_kind() -> wp_presentation_feedback::Kind {
    wp_presentation_feedback::Kind::empty()
}

/// Mark a collected feedback presented NOW.
///
/// Not reporting at all is the WORSE answer, and was the previous behaviour: the
/// `wp_presentation` global is advertised on both backends, so clients request feedback
/// either way, and a feedback destroyed without an answer reaches the client as
/// `discarded` — "your content never reached the screen". For a frame that did reach the
/// screen that is not a missing reply, it is a false one.
///
/// CLOCK_MONOTONIC because that is what the compositor advertises in
/// `state.presentation/presentation.factory` (`PresentationState::new(dh, 1)`); a
/// timestamp from another clock would be silently misread by every client.
///
/// The msc sequence is 0: nested has no vblank counter, and inventing a frame counter
/// would claim a hardware meaning it does not have.
pub fn presented_now(feedback: &mut OutputPresentationFeedback, output: &Output) {
    let now: Duration = Clock::<Monotonic>::new().now().into();
    // Refresh unknown (0 on the wire): the host gives no vblank, so a mode
    // rate here would be a guess the client then paces against (the same
    // reason the stats read null).
    let _ = output;
    feedback.presented::<Duration, Monotonic>(now, Refresh::Unknown, 0, software_present_kind());
}

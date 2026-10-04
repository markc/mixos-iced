//! The exclusive-pacing floor watchdog: registered only while the redraw gate is
//! engaged.
//!
//! Under exclusivity the flip cadence belongs to the tagged client, so a client
//! that stops committing would otherwise stop the compositor with it — a loading
//! screen or a shader hitch freezing the desktop, cursor and UI included. This
//! guarantees a frame whenever a pipe has gone longer than its own measured
//! cadence without producing one (see `tearing.liveness`).
//!
//! Armed on the transition INTO engagement and dropped on the way out, rather
//! than run for the whole session: outside engagement nothing is gated, every
//! source schedules normally, and a 30Hz timer asking "has anything composited"
//! is answering a question no one asked.

use world::state::Loop;
use protocols::tearing::floor::floor;
use protocols::tearing::liveness::liveness;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::{LoopHandle, RegistrationToken};

/// Register the watchdog if it is not already running. Idempotent.
pub fn arm(loop_handle: &LoopHandle<'static, Loop>, slot: &mut Option<RegistrationToken>) {
    if slot.is_some() {
        return;
    }
    // A DEADLINE, not a poll: not "stalled?" every floor interval (16–33 ms) for
    // as long as the gate is engaged. Each wake
    // sleeps exactly until the earliest pipe would become due given its latest
    // composite (`liveness::until_due`): while the pacer is alive that is ~2x its
    // cadence away and moves on with every composite, so the timer only ever
    // fires to find a pipe genuinely due. One source for the whole engagement —
    // not a per-frame registration, which at 300fps would churn 300 a second.
    match loop_handle.insert_source(Timer::from_duration(floor::get()), |_, _, state: &mut Loop| {
        if !protocols::tearing::gate::gate::engaged() {
            return TimeoutAction::ToDuration(floor::get());
        }
        if let Some(remaining) = liveness::until_due() {
            // Not due yet: the last composite moved the deadline. Sleep to it.
            // Clearing the rescue flag: frames arriving now are the pacer's.
            liveness::set_rescue(false);
            return TimeoutAction::ToDuration(remaining);
        }
        // Due (or nothing recorded yet). Tell liveness whose frames the next ones
        // are BEFORE asking for them: rescue frames must not be measured as the
        // pacer's cadence, or each would space the next further out and the
        // watchdog would talk itself down to 4fps.
        liveness::set_rescue(true);
        state.state.redraw.force_for(protocols::redraw::schedule::schedule::RedrawReason::Watchdog);
        TimeoutAction::ToDuration(floor::get())
    }) {
        Ok(token) => {
            *slot = Some(token);
            info!("tearing: pacing floor deadline armed");
        }
        // Not fatal, but say so: without it a stalled pacer freezes the desktop
        // until unrelated input schedules a redraw.
        Err(e) => warn!("tearing: pacing floor watchdog registration failed: {e}"),
    }
}

/// Drop the watchdog and forget the measured cadences, so the next engagement
/// starts from its own pacer rather than inheriting the previous one's rate.
pub fn disarm(loop_handle: &LoopHandle<'static, Loop>, slot: &mut Option<RegistrationToken>) {
    if let Some(token) = slot.take() {
        loop_handle.remove(token);
        liveness::reset();
        info!("tearing: pacing floor watchdog disarmed");
    }
}

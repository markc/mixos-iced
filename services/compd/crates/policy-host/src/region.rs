//! `comp.region.select`.
//!
//! [`start`] admits a run and hands the human seat to it (world
//! `comp::region`); input then drives it; [`service`] answers it once it is
//! decided: a cancel, a timeout or a refusal at once, a selection only after a
//! frame was built without the overlay (a selection is never claimed without
//! a clean frame) or, failing that, `busy` at the cleanup budget. The host arms
//! one timer at [`next_deadline`]; nothing polls.
//!
//! One output: the requested one, else the output the cursor is on. The
//! reply's `output_generation` is that output's generation (CompState): a
//! caller capturing the region checks the output is still the one selected
//! on. A run whose output changed generation is refused `output_changed`.

use std::time::{Duration, Instant};

use serde_json::json;

use comp_model::reply::ControlReply;
use world::comp::region::{Outcome, Run, begin, finish};
use world::state::Loop;

/// How long a selected run may wait for its
/// clean frame.
pub const CLEANUP_BUDGET: Duration = Duration::from_secs(3);

/// Admit a selection. `Err` is the immediate reply (busy, unknown output,
/// already timed out).
pub fn start(
    lp: &mut Loop,
    output: Option<&str>,
    timeout: Duration,
    admitted: Instant,
    sequences_running: bool,
) -> Result<(), ControlReply> {
    // A selection under a session lock is refused `locked`.
    if world::comp::session_lock::active(lp) {
        return Err(ControlReply::Locked);
    }
    let seat = lp.state.seat.seat.clone();
    let pointer = seat.get_pointer();
    let keyboard = seat.get_keyboard();
    use smithay::wayland::input_method::InputMethodSeat as _;
    let busy = !lp.inner.comp.region.idle()
        || sequences_running
        || lp.inner.comp.interactive.is_some()
        || pointer.as_ref().is_some_and(|pointer| pointer.is_grabbed() || !pointer.current_pressed().is_empty())
        || keyboard.as_ref().is_some_and(|keyboard| keyboard.is_grabbed())
        || seat.input_method().keyboard_grabbed();
    if busy {
        return Err(ControlReply::Busy);
    }
    let space = &lp.inner.space_state().state;
    let chosen = match output {
        Some(name) => space.outputs().find(|candidate| candidate.name() == name).cloned(),
        None => {
            let key = lp.inner.cursor_output.clone();
            space
                .outputs()
                .find(|candidate| key.as_ref() == Some(&world::state::state::output_key(candidate)))
                .or_else(|| space.outputs().next())
                .cloned()
        }
    };
    let Some(chosen) = chosen else {
        return Err(ControlReply::refused("unknown_output", json!({})));
    };
    if Instant::now() >= admitted + timeout {
        return Err(ControlReply::Body(json!({"version": 1, "status": "timeout"})));
    }
    let scale = chosen.current_scale().fractional_scale();
    let (width, height) = chosen.current_mode().map_or((0.0, 0.0), |mode| {
        (f64::from(mode.size.w) / scale, f64::from(mode.size.h) / scale)
    });
    let motion = lp.inner.pointer().motion;
    let pointer_at = (motion.x / scale, motion.y / scale);
    let id = lp.inner.comp.region.next_id();
    let generation = lp.inner.comp.output_generation(&chosen.name());
    let run = Run::new(
        id,
        generation,
        chosen.name(),
        output.map(str::to_string),
        (width, height),
        scale,
        pointer_at,
        admitted + timeout,
        admitted + timeout + CLEANUP_BUDGET,
    );
    begin(lp, run);
    Ok(())
}

/// The reply owed, once the run is decided (and the run is then dropped).
pub fn service(lp: &mut Loop, now: Instant) -> Option<ControlReply> {
    let run = lp.inner.comp.region.run.as_ref()?;
    // The output selected on changed.
    if run.result.is_none() && lp.inner.comp.output_generation(&run.output) != run.generation {
        finish(lp, Outcome::Refused("output_changed"));
    }
    let run = lp.inner.comp.region.run.as_ref()?;
    if run.result.is_none() && now >= run.deadline {
        finish(lp, Outcome::Timeout);
    }
    let run = lp.inner.comp.region.run.as_ref()?;
    let reply = match run.result? {
        Outcome::Selected(_) if !run.clean && now < run.reply_deadline => return None,
        // Never claim a selection without a frame free of the overlay.
        Outcome::Selected(_) if !run.clean => ControlReply::Busy,
        Outcome::Selected([x, y, width, height]) => ControlReply::Body(json!({
            "version": 1,
            "status": "selected",
            "output": run.output,
            "output_generation": run.generation,
            "coordinate_space": "output-local-logical",
            "region": {"x": x, "y": y, "width": width, "height": height},
        })),
        Outcome::Cancelled(reason) => ControlReply::Body(json!({"version": 1, "status": "cancelled", "reason": reason})),
        Outcome::Timeout => ControlReply::Body(json!({"version": 1, "status": "timeout"})),
        Outcome::Busy => ControlReply::Busy,
        Outcome::Refused(error) => ControlReply::refused(error, json!({})),
        Outcome::Locked => ControlReply::Locked,
    };
    lp.inner.comp.region.run = None;
    Some(reply)
}

/// The caller stopped waiting: end the run, nothing to reply.
pub fn abandon(lp: &mut Loop) {
    finish(lp, Outcome::Busy);
    lp.inner.comp.region.run = None;
}

/// When the host must look again: the selection's deadline while it runs,
/// the cleanup budget while a selected reply waits for its clean frame.
pub fn next_deadline(lp: &Loop) -> Option<Instant> {
    let run = lp.inner.comp.region.run.as_ref()?;
    match run.result {
        None => Some(run.deadline),
        Some(Outcome::Selected(_)) if !run.clean => Some(run.reply_deadline),
        Some(_) => None,
    }
}

//! `comp.region.select` and `comp.region.cancel`.
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
//!
//! An identified select reserves its owner generation (world
//! `region::reserve`) before the run begins, and every terminal reply of an
//! identified run carries its selection identity — `selected`, `cancelled`,
//! `timeout`, and the refusal shapes (`output_changed`, `locked`, the
//! cleanup-budget `busy`) alike; a legacy run keeps its legacy reply shape
//! byte for byte. [`cancel`] cancels only the exact active selection
//! through the ordinary finish path, so overlay removal, focus restore and
//! the original select reply all happen on the existing path; any other
//! identity is retired and can never start later. Legacy selections without
//! an identity keep their legacy shape and cannot be cancelled here.

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use comp_model::reply::ControlReply;
use comp_model::request::SelectionIdentity;
use world::comp::region::{OWNER_LIMIT, Outcome, Reserve, Retire, Run, begin, finish};
use world::state::Loop;

/// How long a selected run may wait for its
/// clean frame.
pub const CLEANUP_BUDGET: Duration = Duration::from_secs(3);

/// Admit a selection. `Err` is the immediate reply (busy, unknown output,
/// already timed out, retired identity, owner capacity).
#[allow(clippy::too_many_arguments)]
pub fn start(
    lp: &mut Loop,
    output: Option<&str>,
    timeout: Duration,
    admitted: Instant,
    sequences_running: bool,
    selection: Option<SelectionIdentity>,
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
    // An accepted select reserves its generation before the run begins: a
    // cancel that raced ahead (or a reordered mesh delivery) retired it, and
    // a new owner at the limit is refused before anything changes.
    if let Some(identity) = &selection {
        match lp.inner.comp.region.reserve(&identity.owner, identity.generation) {
            Reserve::Accepted => {}
            Reserve::Retired => {
                return Err(ControlReply::refused(
                    "retired_selection",
                    json!({"selection": identity.wire_value()}),
                ));
            }
            Reserve::Capacity => {
                return Err(ControlReply::refused(
                    "region_owner_capacity",
                    json!({"limit": OWNER_LIMIT}),
                ));
            }
        }
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
        selection.map(|identity| world::comp::region::Identity {
            instance: identity.instance,
            owner: identity.owner,
            generation: identity.generation,
        }),
    );
    begin(lp, run);
    Ok(())
}

/// The reply owed, once the run is decided (and the run is then dropped).
/// Every terminal reply of an identified run carries its selection
/// identity; a legacy run keeps its legacy shape byte for byte.
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
    let echo = run.identity.as_ref().map(|identity| {
        json!({
            "instance": identity.instance,
            "owner": identity.owner,
            "generation": identity.generation,
        })
    });
    let reply = match run.result? {
        Outcome::Selected(_) if !run.clean && now < run.reply_deadline => return None,
        // Never claim a selection without a frame free of the overlay.
        Outcome::Selected(_) if !run.clean => terminal_refusal("busy", echo),
        Outcome::Selected([x, y, width, height]) => {
            let mut body = json!({
                "version": 1,
                "status": "selected",
                "output": run.output,
                "output_generation": run.generation,
                "coordinate_space": "output-local-logical",
                "region": {"x": x, "y": y, "width": width, "height": height},
            });
            if let Some(echo) = echo {
                body["selection"] = echo;
            }
            ControlReply::Body(body)
        }
        Outcome::Cancelled(reason) => {
            let mut body = json!({"version": 1, "status": "cancelled", "reason": reason});
            if let Some(echo) = echo {
                body["selection"] = echo;
            }
            ControlReply::Body(body)
        }
        Outcome::Timeout => {
            let mut body = json!({"version": 1, "status": "timeout"});
            if let Some(echo) = echo {
                body["selection"] = echo;
            }
            ControlReply::Body(body)
        }
        Outcome::Busy => terminal_refusal("busy", echo),
        Outcome::Refused(error) => terminal_refusal(error, echo),
        Outcome::Locked => terminal_refusal("locked", echo),
    };
    let identity = run
        .identity
        .as_ref()
        .map(|identity| (identity.owner.clone(), identity.generation));
    lp.inner.comp.region.run = None;
    if let Some((owner, generation)) = identity {
        lp.inner.comp.region.release(&owner, generation);
    }
    Some(reply)
}

/// A decided run's terminal refusal: a legacy run keeps its bare
/// `{"error": ...}` shape byte for byte, an identified one carries its
/// selection echo.
fn terminal_refusal(error: &'static str, echo: Option<Value>) -> ControlReply {
    match echo {
        None => ControlReply::refused(error, json!({})),
        Some(echo) => ControlReply::refused(error, json!({"selection": echo})),
    }
}

/// `comp.region.cancel {selection}`: cancel the exact active run through
/// the ordinary finish path (focus restore, overlay removal and the
/// original select reply all happen on the existing path). Any other
/// identity is retired — it can never start later — and never touches a
/// different run. The compositor instance is fenced by the caller before
/// this runs.
pub fn cancel(lp: &mut Loop, selection: &SelectionIdentity) -> ControlReply {
    let matched = lp
        .inner
        .comp
        .region
        .run_is(&selection.owner, selection.generation);
    if matched {
        let suspended = lp
            .inner
            .comp
            .region
            .run
            .as_ref()
            .is_some_and(|run| run.result.is_none());
        if suspended {
            finish(lp, Outcome::Cancelled("requested"));
        }
        return cancel_reply(
            if suspended { "cancelled" } else { "already_finished" },
            selection,
        );
    }
    match lp.inner.comp.region.retire(&selection.owner, selection.generation) {
        Retire::AlreadyFinished => cancel_reply("already_finished", selection),
        Retire::Retired => cancel_reply("retired", selection),
        Retire::Capacity => ControlReply::refused(
            "region_owner_capacity",
            json!({"limit": OWNER_LIMIT}),
        ),
    }
}

/// The cancellation acknowledgement: applied, not a claim that a clean
/// frame has already been scanned out.
fn cancel_reply(status: &'static str, selection: &SelectionIdentity) -> ControlReply {
    ControlReply::Body(json!({
        "version": 1,
        "status": status,
        "selection": selection.wire_value(),
    }))
}

/// The caller stopped waiting: end the run, nothing to reply.
pub fn abandon(lp: &mut Loop) {
    finish(lp, Outcome::Busy);
    let identity = lp
        .inner
        .comp
        .region
        .run
        .as_ref()
        .and_then(|run| run.identity.as_ref())
        .map(|identity| (identity.owner.clone(), identity.generation));
    lp.inner.comp.region.run = None;
    if let Some((owner, generation)) = identity {
        lp.inner.comp.region.release(&owner, generation);
    }
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

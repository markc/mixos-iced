//! The stall rescue: a one-shot deadline tied to outstanding work, per pipe.
//!
//! A repeating timer that forces a full-damage redraw whenever no flip has
//! happened in its window would, on a static screen, fire in exactly the state
//! it should leave alone.
//!
//! So NO timer runs while nothing is owed. [`IdleRescue::observe`], called after
//! every render attempt, arms a deadline when some live pipe owes work — it is
//! behind the redraw epoch, or has a flip in flight — and drops it when none
//! does. Everything is PER PIPE: progress is the pipe's own flip count
//! ([`Schedule::flips`]), and so is its rescue and fault state, so one output can
//! neither mask nor spend another's. When the deadline expires, a pipe still
//! owing work with no flip of its own since it was last seen progressing is
//! stuck:
//! - first time: log what is owed, UNWEDGE it (below), give it full damage and
//!   force one redraw (reason `Rescue`);
//! - again after that rescue: a fault for that pipe. Logged once as an error and
//!   not repeated; only that pipe's own next flip clears it.
//!
//! Unwedging a pipe stuck IN FLIGHT. A page flip whose completion event never
//! came leaves two things wedged:
//! - the schedule marks the pipe in flight, and the executor skips in-flight
//!   pipes, so a forced redraw renders nothing;
//! - smithay's `DrmCompositor` still holds the flip as `pending_frame`, and while
//!   it does `queue_frame` only parks the next frame and submits NOTHING (no
//!   commit is issued at all).
//!
//! The rescue therefore completes the pending frame first (stock smithay
//! `DrmCompositor::frame_submitted`: the flip is treated as done, so the on-screen
//! buffer stays current and is not reused; see `vendor/smithay/PATCHES.md`), then
//! marks the pipe completed, then forces. The trade-off: if the flip was merely
//! very late rather than lost, the rescue frame's commit reaches the kernel while
//! that flip is still pending, the kernel refuses it (EBUSY), and the queue path
//! handles that as one failed frame. After two seconds with no completion, a
//! lost event is far likelier than a late one.
//!
//! Known residual risk: the other wrong case. If the flip NEVER happened (the
//! commit was accepted but the kernel never latched it, so the old buffer is
//! still the one being scanned out), `frame_submitted` promotes the pending
//! frame to current and returns the previous current buffer, which is the one
//! actually on screen, to the swapchain. The next render may then draw into
//! the scanned-out buffer: visible tearing or a torn frame until the next flip
//! lands, not a hang. Both cases err in one direction (rescue assumes the flip
//! completed); the lost-event case is the one this rescue exists for, and the
//! never-latched case is accepted until smithay can tell the two apart.
//!
//! It cannot influence pacing: while frames flow it only ever observes.

use protocols::redraw::schedule::schedule::Schedule;
use world::state::Loop;
use world::state::state::StatusSession;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::{LoopHandle, RegistrationToken};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

/// How long outstanding work may make no progress (no flip of its own) before
/// it is rescued.
pub const NO_PROGRESS: Duration = Duration::from_secs(2);

/// One owing pipe's rescue state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct PipeRescue {
    /// Its flip count when last seen progressing (or first seen owing).
    flips_at: u64,
    /// Its one rescue for the current stall has been spent.
    rescued: bool,
    /// Faulted at this flip count; only its own next flip clears it.
    faulted_at: Option<u64>,
}

/// What an expiry does to one pipe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Act {
    Rescue,
    Fault,
}

/// Bring `records` in line with the pipes owing work now: forget pipes that no
/// longer owe (their stall, rescue and fault are over), start new ones at their
/// current flip count, and clear a fault whose pipe has flipped since.
fn sync(records: &mut Vec<(String, PipeRescue)>, owed: &[String], flips: &dyn Fn(&str) -> u64) {
    records.retain(|(key, _)| owed.contains(key));
    for key in owed {
        let now = flips(key);
        match records.iter_mut().find(|(k, _)| k == key) {
            None => records.push((key.clone(), PipeRescue { flips_at: now, ..PipeRescue::default() })),
            Some((_, record)) => {
                if record.faulted_at.is_some_and(|at| at != now) {
                    *record = PipeRescue { flips_at: now, ..PipeRescue::default() };
                }
            }
        }
    }
}

/// One expiry: which owing pipes are stuck, and what each gets. A pipe that
/// flipped since it was last seen is progressing and starts over; a faulted
/// pipe is left alone.
fn step(
    records: &mut Vec<(String, PipeRescue)>,
    owed: &[String],
    flips: &dyn Fn(&str) -> u64,
) -> Vec<(String, Act)> {
    sync(records, owed, flips);
    let mut acts = Vec::new();
    for (key, record) in records.iter_mut() {
        if record.faulted_at.is_some() {
            continue;
        }
        let now = flips(key);
        if now != record.flips_at {
            *record = PipeRescue { flips_at: now, ..PipeRescue::default() };
        } else if !record.rescued {
            record.rescued = true;
            acts.push((key.clone(), Act::Rescue));
        } else {
            record.faulted_at = Some(now);
            acts.push((key.clone(), Act::Fault));
        }
    }
    acts
}

/// Whether any owing pipe is still being watched (not faulted).
fn watching(records: &[(String, PipeRescue)]) -> bool {
    records.iter().any(|(_, record)| record.faulted_at.is_none())
}

struct Inner {
    token: Option<RegistrationToken>,
    records: Vec<(String, PipeRescue)>,
    /// Unwedge and fully damage one pipe: drop a lost pending flip (when
    /// `in_flight`) and reset its buffers.
    rescue_pipe: Box<dyn FnMut(&str, bool)>,
    /// The live pipes owing work right now (behind the epoch or in flight).
    outstanding: Box<dyn Fn(&Loop) -> Vec<String>>,
}

/// Cloned into every source that renders; one deadline between them.
#[derive(Clone)]
pub struct IdleRescue(Rc<RefCell<Inner>>);

fn flips_of(schedule: &Schedule) -> impl Fn(&str) -> u64 + '_ {
    move |key| schedule.flips(key).unwrap_or(0)
}

impl IdleRescue {
    pub fn new(
        rescue_pipe: impl FnMut(&str, bool) + 'static,
        outstanding: impl Fn(&Loop) -> Vec<String> + 'static,
    ) -> Self {
        Self(Rc::new(RefCell::new(Inner {
            token: None,
            records: Vec::new(),
            rescue_pipe: Box::new(rescue_pipe),
            outstanding: Box::new(outstanding),
        })))
    }

    fn owed(inner: &Inner, state: &Loop) -> Vec<String> {
        if matches!(state.inner.status_session, StatusSession::Paused) {
            return Vec::new();
        }
        (inner.outstanding)(state)
    }

    /// After every render attempt: keep the per-pipe records current, arm the
    /// deadline while a pipe owes work and is not faulted, drop it otherwise.
    pub fn observe(&self, handle: &LoopHandle<'static, Loop>, state: &Loop) {
        let Ok(mut inner) = self.0.try_borrow_mut() else {
            return;
        };
        let owed = Self::owed(&inner, state);
        sync(&mut inner.records, &owed, &flips_of(&state.state.redraw));
        if !watching(&inner.records) {
            if let Some(token) = inner.token.take() {
                handle.remove(token);
            }
            return;
        }
        if inner.token.is_some() {
            return;
        }
        let this = self.clone();
        match handle.insert_source(
            Timer::from_duration(NO_PROGRESS),
            move |_, _, state: &mut Loop| this.expired(state),
        ) {
            Ok(token) => inner.token = Some(token),
            Err(e) => warn!("native: stall deadline not armed: {e}"),
        }
    }

    fn expired(&self, state: &mut Loop) -> TimeoutAction {
        let Ok(mut inner) = self.0.try_borrow_mut() else {
            return TimeoutAction::ToDuration(NO_PROGRESS);
        };
        let owed = Self::owed(&inner, state);
        let acts = step(&mut inner.records, &owed, &flips_of(&state.state.redraw));
        let reasons = if acts.is_empty() { String::new() } else { pending_reasons(state) };
        let mut forced = false;
        for (key, act) in &acts {
            match act {
                Act::Rescue => {
                    let in_flight = state.state.redraw.in_flight(key);
                    warn!(
                        "native: {key} made no progress in {}s{} — rescuing once (owed: {reasons})",
                        NO_PROGRESS.as_secs(),
                        if in_flight { " with a flip in flight (treating it as lost)" } else { "" },
                    );
                    // Smithay's pending flip first, then the schedule's flight:
                    // see the module note for why both, and the trade-off.
                    (inner.rescue_pipe)(key, in_flight);
                    if in_flight {
                        state.state.redraw.completed(key);
                    }
                    forced = true;
                }
                Act::Fault => error!(
                    "native: stall fault on {key} — the rescue did not restart it (owed: {reasons}); \
                     not forcing again until it flips"
                ),
            }
        }
        let keep = watching(&inner.records);
        if !keep {
            inner.token = None;
        }
        drop(inner);
        if forced {
            state
                .state
                .redraw
                .force_for(protocols::redraw::schedule::schedule::RedrawReason::Rescue);
        }
        if keep { TimeoutAction::ToDuration(NO_PROGRESS) } else { TimeoutAction::Drop }
    }
}

/// What each pipe still owes a frame for, from the ledger: `pipe=[reasons]`.
fn pending_reasons(state: &Loop) -> String {
    let snapshot = state.state.redraw.ledger().snapshot();
    let owed: Vec<String> = snapshot
        .pipes
        .iter()
        .filter(|(_, pipe)| !pipe.pending.is_empty())
        .map(|(key, pipe)| format!("{key}={:?}", pipe.pending))
        .collect();
    if owed.is_empty() {
        "a flip in flight, no reasons pending".to_string()
    } else {
        owed.join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn keys(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    /// Flip counts the tests move by hand.
    struct Flips(RefCell<HashMap<String, u64>>);
    impl Flips {
        fn new() -> Self { Self(RefCell::new(HashMap::new())) }
        fn flip(&self, key: &str) { *self.0.borrow_mut().entry(key.to_string()).or_default() += 1; }
        fn get(&self, key: &str) -> u64 { self.0.borrow().get(key).copied().unwrap_or(0) }
    }

    fn acts_for(acts: &[(String, Act)], key: &str) -> Vec<Act> {
        acts.iter().filter(|(k, _)| k == key).map(|(_, a)| *a).collect()
    }

    /// Round-1 A: a sibling's flips do not hide a pipe that is stuck.
    #[test]
    fn a_stuck_pipe_is_found_beside_a_busy_sibling() {
        let flips = Flips::new();
        let f = |k: &str| flips.get(k);
        let mut records = Vec::new();
        let owed = keys(&["a", "b"]);
        sync(&mut records, &owed, &f);
        flips.flip("b");
        let acts = step(&mut records, &owed, &f);
        assert_eq!(acts_for(&acts, "a"), [Act::Rescue]);
        assert!(acts_for(&acts, "b").is_empty(), "b progressed");
    }

    #[test]
    fn rescue_then_fault_for_the_same_pipe() {
        let flips = Flips::new();
        let f = |k: &str| flips.get(k);
        let mut records = Vec::new();
        let owed = keys(&["a"]);
        sync(&mut records, &owed, &f);
        assert_eq!(acts_for(&step(&mut records, &owed, &f), "a"), [Act::Rescue]);
        assert_eq!(acts_for(&step(&mut records, &owed, &f), "a"), [Act::Fault]);
        assert!(step(&mut records, &owed, &f).is_empty(), "a fault is not repeated");
        assert!(!watching(&records));
    }

    /// Round-2 finding 3 (codex): a later stall on B gets B's OWN rescue, after
    /// A was rescued and recovered.
    #[test]
    fn each_pipe_gets_its_own_rescue() {
        let flips = Flips::new();
        let f = |k: &str| flips.get(k);
        let mut records = Vec::new();
        let owed = keys(&["a", "b"]);
        sync(&mut records, &owed, &f);
        flips.flip("b");
        assert_eq!(acts_for(&step(&mut records, &owed, &f), "a"), [Act::Rescue]);
        flips.flip("a"); // the rescue worked
        flips.flip("b");
        assert!(step(&mut records, &owed, &f).is_empty());
        flips.flip("a"); // a keeps moving; b stalls
        let acts = step(&mut records, &owed, &f);
        assert_eq!(acts_for(&acts, "b"), [Act::Rescue], "b's first stall is a rescue, not a fault");
    }

    /// Round-2 finding 3 (fable): a sibling starting to flip does not clear a
    /// faulted pipe's fault; only its own flip does.
    #[test]
    fn a_fault_clears_only_on_that_pipes_own_flip() {
        let flips = Flips::new();
        let f = |k: &str| flips.get(k);
        let mut records = Vec::new();
        let owed = keys(&["a", "b"]);
        sync(&mut records, &owed, &f);
        flips.flip("b");
        step(&mut records, &owed, &f); // a: rescue
        flips.flip("b");
        step(&mut records, &owed, &f); // a: fault
        for _ in 0..3 {
            flips.flip("b");
            sync(&mut records, &owed, &f);
            assert!(acts_for(&step(&mut records, &owed, &f), "a").is_empty(), "still faulted");
        }
        flips.flip("a");
        sync(&mut records, &owed, &f);
        let record = &records.iter().find(|(k, _)| k == "a").unwrap().1;
        assert_eq!(record.faulted_at, None, "its own flip cleared it");
    }

    #[test]
    fn a_pipe_that_stops_owing_is_forgotten() {
        let flips = Flips::new();
        let f = |k: &str| flips.get(k);
        let mut records = Vec::new();
        sync(&mut records, &keys(&["a"]), &f);
        step(&mut records, &keys(&["a"]), &f); // a: rescue
        sync(&mut records, &keys(&[]), &f);
        assert!(records.is_empty());
        sync(&mut records, &keys(&["a"]), &f);
        assert_eq!(acts_for(&step(&mut records, &keys(&["a"]), &f), "a"), [Act::Rescue], "a new stall, a new rescue");
    }
}

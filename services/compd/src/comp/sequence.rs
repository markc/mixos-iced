//! `comp.input.sequence` on the compd host.
//!
//! policy's `SequenceRun` is the run model (steps, delays, replies); this
//! drives it on the loop:
//! - a step's delay arms ONE calloop timer, and the run resumes from it;
//! - a long zero-delay stretch yields to the loop every
//!   `SEQUENCE_YIELD_EVENTS` injected events (or steps): the run is queued
//!   ready and the loop is woken through the Bus waker, so clients get their
//!   events flushed and nothing polls;
//! - adjacent agent moves that land on the same surface coalesce into one
//!   (policy-host `coalesce_agent_motion`), each counted in the reply;
//! - a refused step ends the run `step_failed` and releases what the run
//!   holds; a caller that stopped waiting ends it silently;
//! - losing input authority ends the agent's runs `input_cleared`.
//!
//! Every step runs through policy-host `input::run_step`, the single-verb
//! path, with the run as the owner of what it presses.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;
use std::time::Instant;

use smithay::reexports::calloop::LoopHandle;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};

use comp_service::LongReply;
use comp_model::reply::ControlReply;
use comp_model::request::LongOp;
use policy::agent::{SEQUENCE_YIELD_EVENTS, SequenceNext, SequenceRun};
use world::state::Loop;

struct Run {
    run: SequenceRun,
    reply: LongReply,
    started: Instant,
}

pub(super) struct Sequences {
    runs: BTreeMap<u64, Run>,
    next: u64,
    /// Runs ready to continue, with whether a delay just elapsed for them.
    /// Filled by delay timers and yields, drained by [`Self::service`].
    ready: Rc<RefCell<VecDeque<(u64, bool)>>>,
    /// Wakes the loop for a yielded run (the Bus waker: a ping, not a timer).
    wake: comp_service::Waker,
}

impl Sequences {
    pub(super) fn new(wake: comp_service::Waker) -> Self {
        Self {
            runs: BTreeMap::new(),
            next: 0,
            ready: Rc::new(RefCell::new(VecDeque::new())),
            wake,
        }
    }

    /// Admit a `Sequence` / `SeatedSequence`; any other op is handed back.
    pub(super) fn start(
        &mut self,
        lp: &mut Loop,
        handle: &LoopHandle<'static, Loop>,
        op: LongOp,
        reply: LongReply,
        admitted: Instant,
    ) -> Option<(LongOp, LongReply)> {
        if !matches!(op, LongOp::Sequence(_) | LongOp::SeatedSequence { .. }) {
            return Some((op, reply));
        }
        let run = SequenceRun::new(op).expect("a sequence op");
        let id = self.next;
        self.next = self.next.wrapping_add(1);
        self.runs.insert(id, Run { run, reply, started: admitted });
        self.advance(lp, handle, id);
        None
    }

    pub(super) fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }

    /// Continue every run a timer or a yield made ready.
    pub(super) fn service(&mut self, lp: &mut Loop, handle: &LoopHandle<'static, Loop>) {
        let ready: Vec<(u64, bool)> = self.ready.borrow_mut().drain(..).collect();
        for (id, elapsed) in ready {
            if elapsed && let Some(entry) = self.runs.get_mut(&id) {
                entry.run.delay_elapsed();
            }
            self.advance(lp, handle, id);
        }
    }

    /// Input authority was lost: every run that drives the agent seat ends
    /// `input_cleared`, its holds released first.
    pub(super) fn clear_agent(&mut self, lp: &mut Loop) {
        let agent: Vec<u64> = self
            .runs
            .iter()
            .filter_map(|(id, entry)| entry.run.uses_agent().then_some(*id))
            .collect();
        for id in agent {
            if let Some(entry) = self.runs.remove(&id) {
                policy_host::input::release_run(lp, id);
                entry.reply.send(entry.run.cleared());
            }
        }
    }

    fn advance(&mut self, lp: &mut Loop, handle: &LoopHandle<'static, Loop>, id: u64) {
        let events_at_start = lp.inner.comp.injection.events;
        let mut steps = 0_u64;
        loop {
            let Some(entry) = self.runs.get_mut(&id) else { return };
            if entry.reply.is_closed() {
                // The caller stopped waiting: stop driving the seat for nobody.
                self.runs.remove(&id);
                policy_host::input::release_run(lp, id);
                return;
            }
            let ran = lp.inner.comp.injection.events.wrapping_sub(events_at_start);
            if ran >= SEQUENCE_YIELD_EVENTS || steps >= SEQUENCE_YIELD_EVENTS {
                self.ready.borrow_mut().push_back((id, false));
                (self.wake)();
                return;
            }
            match entry.run.next_action() {
                SequenceNext::Done => {
                    let entry = self.runs.remove(&id).expect("the run is present");
                    let elapsed_ms = u64::try_from(entry.started.elapsed().as_millis()).unwrap_or(u64::MAX);
                    entry.reply.send(entry.run.finish(elapsed_ms));
                    return;
                }
                SequenceNext::Wait(delay) => {
                    let ready = Rc::clone(&self.ready);
                    if let Err(error) = handle.insert_source(Timer::from_duration(delay), move |_, _, _| {
                        ready.borrow_mut().push_back((id, true));
                        TimeoutAction::Drop
                    }) {
                        model::warn!("comp.input.sequence: no delay timer: {error}");
                        let entry = self.runs.remove(&id).expect("the run is present");
                        policy_host::input::release_run(lp, id);
                        entry.reply.send(ControlReply::Busy);
                    }
                    return;
                }
                SequenceNext::Run { index, mut step } => {
                    let mut coalesced = 1_usize;
                    while steps + (coalesced as u64) < SEQUENCE_YIELD_EVENTS {
                        let Some(next) = entry.run.peek_undelayed() else { break };
                        let Some(combined) = policy_host::input::coalesce_agent_motion(lp, &step.op, &next.op) else {
                            break;
                        };
                        step.op = combined;
                        entry.run.absorb_front();
                        coalesced += 1;
                    }
                    steps += coalesced as u64;
                    let reply = policy_host::input::run_step(lp, id, &step.op);
                    let Some(entry) = self.runs.get_mut(&id) else { return };
                    if let Some(failed) = entry.run.record(index, step.verb, reply, coalesced) {
                        let entry = self.runs.remove(&id).expect("the run is present");
                        policy_host::input::release_run(lp, id);
                        entry.reply.send(failed);
                        return;
                    }
                }
            }
        }
    }
}

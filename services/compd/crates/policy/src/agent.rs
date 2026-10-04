// Every Smithay query (grabs, bound devices, focus, popup provenance,
// mapped trees) is a field of a facts struct the engine fills in; the
// sequence run is a state machine the engine drives (timers, the event-loop
// yield and agent-motion coalescing stay with it).

//! The engine-neutral half of Bus-injected input: which refusal an op gets
//! before anything is injected, which holds injection owns, and how a
//! `comp.input.sequence` run advances and replies.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::hash::Hash;
use std::time::Duration;

use serde_json::{Value, json};

use comp_model::reply::ControlReply;
use comp_model::request::{InputOp, LongOp, PressAction, SequenceStep};
use surfaces::{Registry, SeatKind, WindowTargetError};

/// A sequence yields to the event loop after this many injected events.
pub const SEQUENCE_YIELD_EVENTS: u64 = 256;
pub const SEQUENCE_YIELD: Duration = Duration::from_millis(1);

/// The agent seat's refusal body: the hint names the seat that would work.
pub fn agent_refusal(reason: &'static str) -> ControlReply {
    let mut detail = json!({});
    if matches!(
        reason,
        "agent_seat_unbound" | "x11_unsupported" | "chrome_target"
    ) {
        detail["hint"] = json!({"seat":"human"});
    }
    if reason == "agent_seat_unbound" {
        detail["message"] = json!("target client is currently unbound on the agent seat");
    }
    ControlReply::refused(reason, detail)
}

/// What the engine knows about the surface agent input would reach (its
/// canonical root's facts, plus the surface's own tree).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AgentTarget {
    /// The root is an X11 window.
    pub x11: bool,
    pub root_mapped: bool,
    /// The surface itself is in a mapped tree.
    pub tree_mapped: bool,
    pub input_presentable: bool,
    /// The surface's Wayland client is still known.
    pub has_client: bool,
    /// The client bound a keyboard / pointer on the agent seat.
    pub keyboard_bound: bool,
    pub pointer_bound: bool,
}

/// Validate the agent target. `target` is `None` when the root has no
/// record. The checks run in this order.
pub fn validate_agent_surface(
    target: Option<AgentTarget>,
    keyboard: bool,
    session_lock: bool,
) -> Result<(), ControlReply> {
    if session_lock {
        return Err(agent_refusal("session_lock"));
    }
    let target = target.ok_or_else(|| agent_refusal("unmapped"))?;
    if target.x11 {
        return Err(agent_refusal("x11_unsupported"));
    }
    if !target.root_mapped || !target.tree_mapped {
        return Err(agent_refusal("unmapped"));
    }
    if !target.input_presentable {
        return Err(agent_refusal("not_presentable"));
    }
    if !target.has_client {
        return Err(agent_refusal("unmapped"));
    }
    let bound = if keyboard {
        target.keyboard_bound
    } else {
        target.pointer_bound
    };
    if !bound {
        return Err(agent_refusal("agent_seat_unbound"));
    }
    Ok(())
}

/// The agent seat's state for an untargeted op.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AgentSeatFacts {
    pub session_lock: bool,
    /// A keyboard grab other than a popup's.
    pub keyboard_grab: bool,
    /// A pointer grab other than a popup's or the implicit click grab.
    pub pointer_grab: bool,
    /// A popup pointer grab (a press outside the popup dismisses it).
    pub popup_pointer_grab: bool,
    /// Where an agent key would go: `None` = no keyboard target;
    /// `Some(None)` = a target whose root has no record.
    pub keyboard_target: Option<Option<AgentTarget>>,
    /// The agent pointer's focus, as above.
    pub pointer_target: Option<Option<AgentTarget>>,
}

/// The refusal an agent op gets before anything is
/// injected. Releases and `release_all` always pass (cleanup must never be
/// refused).
pub fn agent_preflight(op: &InputOp, facts: &AgentSeatFacts) -> Result<(), ControlReply> {
    if facts.session_lock {
        return Err(agent_refusal("session_lock"));
    }
    if matches!(
        op,
        InputOp::ReleaseAll
            | InputOp::Key {
                action: PressAction::Release,
                ..
            }
            | InputOp::PointerButton {
                action: PressAction::Release,
                ..
            }
    ) {
        return Ok(());
    }
    if matches!(op, InputOp::Key { .. } | InputOp::Text(_)) && facts.keyboard_grab {
        return Err(agent_refusal("keyboard_grab"));
    }
    if matches!(
        op,
        InputOp::PointerButton { .. } | InputOp::PointerScroll { .. }
    ) && facts.pointer_grab
    {
        return Err(agent_refusal("pointer_grab"));
    }
    match op {
        InputOp::Key { .. } | InputOp::Text(_) => {
            let target = facts
                .keyboard_target
                .ok_or_else(|| agent_refusal("no_keyboard_target"))?;
            validate_agent_surface(target, true, facts.session_lock)
        }
        InputOp::PointerButton { .. } | InputOp::PointerScroll { .. } => {
            if let Some(target) = facts.pointer_target {
                validate_agent_surface(target, false, facts.session_lock)
            } else if facts.popup_pointer_grab {
                // A press outside a popup is delivered to its grab to dismiss it.
                Ok(())
            } else {
                Err(agent_refusal("no_pointer_target"))
            }
        }
        _ => Ok(()),
    }
}

/// The agent seat's state for a `{window}`-targeted op.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AgentTargetedFacts {
    pub session_lock: bool,
    /// The named window's surface.
    pub target: Option<AgentTarget>,
    /// Any agent keyboard grab.
    pub keyboard_grabbed: bool,
    /// The grab is a popup's whose delivery target is the named window.
    pub matching_popup: bool,
    /// Any agent pointer grab.
    pub pointer_grabbed: bool,
    /// The agent pointer already delivers to the named window.
    pub pointer_on_target: bool,
    /// What the pointer would hit at its position on the window: `None`
    /// is compositor chrome (SSD titlebar, embedded panel).
    pub hit: Option<AgentTarget>,
}

/// What a targeted agent op does once admitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentTargetedPlan {
    /// A release retires the seat's hold; its target is never resolved or
    /// refocused.
    Release,
    /// Set the agent keyboard focus (unless a matching popup keeps it),
    /// move the agent pointer onto the hit (pointer ops), then deliver.
    Deliver { focus_keyboard: bool, move_pointer: bool },
}

/// The checks a targeted agent op passes, in order.
pub fn agent_targeted_preflight<H: Clone + Eq + Hash>(
    registry: &Registry<H>,
    id: u64,
    generation: u64,
    raise: bool,
    op: &InputOp,
    facts: &AgentTargetedFacts,
) -> Result<AgentTargetedPlan, ControlReply> {
    if raise {
        return Err(agent_refusal("invalid_argument"));
    }
    if facts.session_lock {
        return Err(agent_refusal("session_lock"));
    }
    if matches!(
        op,
        InputOp::Key {
            action: PressAction::Release,
            ..
        } | InputOp::PointerButton {
            action: PressAction::Release,
            ..
        }
    ) {
        return Ok(AgentTargetedPlan::Release);
    }
    if let Err(error) = registry.resolve_window_target(id, Some(generation)) {
        return Err(ControlReply::WindowTarget { id, error });
    }
    let keyboard_op = matches!(op, InputOp::Key { .. } | InputOp::Text(_));
    validate_agent_surface(facts.target, keyboard_op, facts.session_lock)?;
    if facts.keyboard_grabbed && !facts.matching_popup {
        return Err(agent_refusal("keyboard_grab"));
    }
    if facts.pointer_grabbed && !facts.pointer_on_target {
        return Err(agent_refusal("pointer_grab"));
    }
    if !keyboard_op {
        let hit = facts.hit.ok_or_else(|| agent_refusal("chrome_target"))?;
        validate_agent_surface(Some(hit), false, facts.session_lock)?;
    }
    Ok(AgentTargetedPlan::Deliver {
        focus_keyboard: !facts.matching_popup,
        move_pointer: !keyboard_op,
    })
}

/// The human seat's state for a `{window}`-targeted op.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HumanTargetedFacts {
    pub region_select: bool,
    pub session_lock: bool,
    pub exclusive_layer: bool,
    /// The default output's current workspace.
    pub current_workspace: u32,
    pub input_presentable: bool,
    /// The window's effective visibility.
    pub visible: bool,
    /// A human keyboard grab (incl. an input method's).
    pub keyboard_grab: bool,
    /// For buttons: a chrome or interactive grab, or a pointer grab that
    /// does not deliver to the named window.
    pub pointer_grab: bool,
}

/// What a targeted human op does once admitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HumanTargetedPlan {
    /// A key release reconciles the seat that owns the hold, never
    /// refocusing its window.
    Release,
    /// Focus the window (`comp.window.focus` with `raise`), refuse
    /// `focus_refused` unless it took the keyboard, then deliver.
    FocusThenDeliver,
}

/// `target_unfocusable {id, generation, reason}`.
pub fn target_unfocusable(id: u64, generation: u64, reason: &'static str) -> ControlReply {
    ControlReply::refused(
        "target_unfocusable",
        json!({"id": id, "generation": generation, "reason": reason}),
    )
}

/// The refusal ladder of a targeted human-seat op, in order. A refusal
/// injects nothing and switches no workspace.
pub fn human_targeted_preflight<H: Clone + Eq + Hash>(
    registry: &Registry<H>,
    id: u64,
    generation: u64,
    op: &InputOp,
    facts: &HumanTargetedFacts,
) -> Result<HumanTargetedPlan, ControlReply> {
    let record = match registry.resolve_window_target(id, Some(generation)) {
        Ok(record) => record,
        Err(WindowTargetError::NotMapped) => {
            return Err(target_unfocusable(id, generation, "unmapped"));
        }
        Err(error) => return Err(ControlReply::WindowTarget { id, error }),
    };
    if matches!(
        op,
        InputOp::Key {
            action: PressAction::Release,
            ..
        }
    ) {
        return Ok(HumanTargetedPlan::Release);
    }
    let reason = if facts.region_select {
        Some("region_select")
    } else if facts.session_lock {
        Some("session_lock")
    } else if facts.exclusive_layer {
        Some("exclusive_layer")
    } else if record.minimized() {
        Some("minimized")
    } else if !crate::workspaces::on_workspace(record, facts.current_workspace) {
        Some("other_workspace")
    } else if !facts.input_presentable {
        Some("not_presentable")
    } else if !facts.visible {
        Some("not_visible")
    } else if facts.keyboard_grab {
        Some("keyboard_grab")
    } else if matches!(op, InputOp::PointerButton { .. }) && facts.pointer_grab {
        Some("pointer_grab")
    } else {
        None
    };
    match reason {
        Some(reason) => Err(target_unfocusable(id, generation, reason)),
        None => Ok(HumanTargetedPlan::FocusThenDeliver),
    }
}

/// Which seats a `release_all` cleans.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReleaseScope {
    /// Bare `release_all`: both seats' injected holds (and the agent's
    /// popups); the reply says `"seat":"both"`.
    Both,
    /// Scoped to one seat; the other keeps its holds.
    Seat(SeatKind),
}

impl ReleaseScope {
    /// The scope of a parsed `release_all`, or `None` for any other op.
    pub fn of(op: &InputOp) -> Option<Self> {
        match op {
            InputOp::ReleaseAll => Some(Self::Both),
            InputOp::OnSeat { seat, op } if matches!(op.as_ref(), InputOp::ReleaseAll) => {
                Some(Self::Seat(*seat))
            }
            _ => None,
        }
    }

    /// The reply's `seat`.
    pub fn reply_seat(self) -> &'static str {
        match self {
            Self::Both => "both",
            Self::Seat(seat) => seat.name(),
        }
    }

    pub fn includes(self, seat: SeatKind) -> bool {
        match self {
            Self::Both => true,
            Self::Seat(scoped) => scoped == seat,
        }
    }
}

/// A key (raw XKB code) or button that injection pressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Hold {
    Key(u32),
    Button(u32),
}

/// Who pressed a hold: a sequence run, or `None` for single verbs.
pub type HoldOwner = Option<u64>;

/// Every injected hold of one seat and the owners that pressed it. A hold
/// is released on an owner's abort only when no other owner still holds
/// it; an explicit release (any caller) really lets the key go, so it
/// clears every owner. Physical holds are never here.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Holds {
    owners: BTreeMap<Hold, BTreeSet<HoldOwner>>,
}

impl Holds {
    /// An injected press or release went through the seat.
    pub fn note(&mut self, owner: HoldOwner, hold: Hold, pressed: bool) {
        if pressed {
            self.owners.entry(hold).or_default().insert(owner);
        } else {
            self.owners.remove(&hold);
        }
    }

    /// Drop one owner; returns the holds nobody holds any more (to release,
    /// in [`release_order`]).
    pub fn drop_owner(&mut self, owner: HoldOwner) -> Vec<Hold> {
        let mut orphaned = Vec::new();
        self.owners.retain(|hold, owners| {
            if owners.remove(&owner) && owners.is_empty() {
                orphaned.push(*hold);
                return false;
            }
            !owners.is_empty()
        });
        orphaned
    }

    /// `release_all`: every injected hold, and only those.
    pub fn take_all(&mut self) -> Vec<Hold> {
        std::mem::take(&mut self.owners).into_keys().collect()
    }

    /// The agent's device holds of one kind (a VT switch or unmap of the
    /// keyboard or pointer target).
    pub fn take_kind(&mut self, keyboard: bool) -> Vec<Hold> {
        let taken = self
            .owners
            .keys()
            .copied()
            .filter(|hold| matches!(hold, Hold::Key(_)) == keyboard)
            .collect();
        self.owners
            .retain(|hold, _| matches!(hold, Hold::Key(_)) != keyboard);
        taken
    }

    pub fn is_empty(&self) -> bool {
        self.owners.is_empty()
    }

    pub fn owners_of(&self, hold: Hold) -> usize {
        self.owners.get(&hold).map_or(0, BTreeSet::len)
    }
}

/// The order holds are released in: keys newest code first, then buttons.
/// `physically_held` filters what the human still holds on a real device
/// (cleanup must preserve physical holds; pass `|_| false` for the agent).
pub fn release_order(holds: &[Hold], physically_held: impl Fn(Hold) -> bool) -> Vec<Hold> {
    holds
        .iter()
        .rev()
        .filter(|hold| matches!(hold, Hold::Key(_)))
        .chain(holds.iter().filter(|hold| matches!(hold, Hold::Button(_))))
        .copied()
        .filter(|hold| !physically_held(*hold))
        .collect()
}

/// One running `comp.input.sequence`.
#[derive(Clone, Debug, PartialEq)]
pub struct SequenceRun {
    seat: Option<SeatKind>,
    driven_seat: Option<&'static str>,
    uses_agent: bool,
    steps: VecDeque<SequenceStep>,
    index: usize,
    delay_elapsed: bool,
    replies: Vec<Value>,
}

/// What the engine does next for a run.
#[derive(Clone, Debug, PartialEq)]
pub enum SequenceNext {
    /// Arm a one-shot timer for the front step's delay, then call
    /// [`SequenceRun::delay_elapsed`] and ask again.
    Wait(Duration),
    /// Run this step through the single-verb path (`service_input_op`),
    /// with the run as the hold owner, and report its reply.
    Run { index: usize, step: SequenceStep },
    /// Every step ran: reply with [`SequenceRun::finish`].
    Done,
}

impl SequenceRun {
    /// A run for a parsed `Sequence` / `SeatedSequence`; `None` for any
    /// other long op.
    pub fn new(op: LongOp) -> Option<Self> {
        let (steps, seat) = match op {
            LongOp::Sequence(steps) => (steps, None),
            LongOp::SeatedSequence { seat, steps } => (steps, Some(seat)),
            _ => return None,
        };
        Some(Self {
            seat,
            driven_seat: None,
            uses_agent: steps.iter().any(|step| step.op.uses_agent()),
            steps: steps.into(),
            index: 0,
            delay_elapsed: false,
            replies: Vec::new(),
        })
    }

    /// The run drives the agent seat: human input cancels it.
    pub fn uses_agent(&self) -> bool {
        self.uses_agent
    }

    /// The front step's delay has elapsed.
    pub fn delay_elapsed(&mut self) {
        self.delay_elapsed = true;
    }

    /// The next step, when it runs with no delay: the engine's candidate for
    /// coalescing into the step [`Self::next_action`] just handed out (the
    /// agent-motion loop).
    pub fn peek_undelayed(&self) -> Option<&SequenceStep> {
        self.steps.front().filter(|step| step.delay.is_zero())
    }

    /// The engine coalesced the front step into the one it is running: it is
    /// consumed (its index too); [`Self::record`]'s `coalesced` counts it.
    pub fn absorb_front(&mut self) {
        if self.steps.pop_front().is_some() {
            self.index += 1;
        }
    }

    pub fn next_action(&mut self) -> SequenceNext {
        let Some(step) = self.steps.front() else {
            return SequenceNext::Done;
        };
        if !step.delay.is_zero() && !self.delay_elapsed {
            return SequenceNext::Wait(step.delay);
        }
        let step = self.steps.pop_front().expect("front step present");
        let index = self.index;
        self.index += 1;
        self.delay_elapsed = false;
        let driven = match &step.op {
            InputOp::OnSeat { seat, .. } => seat.name(),
            InputOp::ReleaseAll => "mixed",
            _ => "human",
        };
        self.driven_seat = Some(match self.driven_seat {
            None => driven,
            Some(previous) if previous == driven => previous,
            Some(_) => "mixed",
        });
        SequenceNext::Run { index, step }
    }

    /// The step's reply. A success is kept for the final reply (once per
    /// coalesced step, marked `coalesced`). A refusal ends the run: release
    /// its holds (`Holds::drop_owner`) and send the returned `step_failed`.
    pub fn record(
        &mut self,
        index: usize,
        verb: &'static str,
        reply: ControlReply,
        coalesced: usize,
    ) -> Option<ControlReply> {
        match reply {
            ControlReply::Body(mut body) => {
                if coalesced > 1 {
                    body["coalesced"] = json!(coalesced);
                }
                for _ in 0..coalesced.max(1) {
                    self.replies.push(body.clone());
                }
                None
            }
            refusal => Some(ControlReply::refused(
                "step_failed",
                json!({
                    "seat": self.reply_seat(),
                    "index": index,
                    "verb": verb,
                    "step": refusal.wire_json(),
                    "completed": self.replies,
                    "released": true,
                }),
            )),
        }
    }

    /// The final reply of a run that ran every step.
    pub fn finish(self, elapsed_ms: u64) -> ControlReply {
        ControlReply::Body(json!({
            "seat": self.reply_seat(),
            "steps": self.replies,
            "elapsed_ms": elapsed_ms,
        }))
    }

    /// The reply of an agent run cancelled by human input (its holds
    /// released first).
    pub fn cleared(self) -> ControlReply {
        ControlReply::refused(
            "input_cleared",
            json!({"seat":"agent", "completed": self.replies, "released": true}),
        )
    }

    /// The reply's `seat`: the sequence's own, else the seats its steps
    /// drove (`human`, `agent`, or `mixed`).
    pub fn reply_seat(&self) -> &'static str {
        self.seat
            .map(SeatKind::name)
            .or(self.driven_seat)
            .unwrap_or("human")
    }
}

#[cfg(test)]
#[path = "agent_tests.rs"]
mod tests;

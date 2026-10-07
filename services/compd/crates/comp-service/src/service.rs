// The engine is reached through the `CompEngine` trait; the props-watch
// baseline and the pointer lease are the engine's (it owns the diff and the
// pointer), and this layer keeps the ordering, fencing, batching and reply
// rules.

//! The engine's half of the `comp` port: drain the command channel on the
//! engine's own loop (after the waker fires), call the engine in arrival
//! order, and answer every admission exactly once.

use std::collections::BTreeSet;
use std::sync::{
    Arc,
    atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use comp_model::observation::{POINTER_TOPIC_SUFFIX, PROPS_TOPIC_SUFFIX, PanelRequest, topic_name};
use comp_model::reply::ControlReply;
use comp_model::request::{InputOp, LongOp, SelectionIdentity, WindowOp};
use comp_model::snapshot::{BROKER_CONNECTED, BROKER_RETRYING, CompSnapshot, PortSnapshot, ReadScopes};

use crate::channel::{CommandSource, Waker};
use crate::port::{PORT_QUEUE_CAPACITY, PortCommand, PortControl, PortRequest};

/// The pointer watch lease.
pub const POINTER_LEASE: Duration = Duration::from_secs(3);

/// The most agent-seat controls one service pass runs; the rest stay queued
/// and the waker fires again.
pub const AGENT_CONTROL_BATCH: usize = 8;

/// The counters and identity both halves of the port share (the engine
/// fills the decoration fields into `decoration.*` itself).
#[derive(Debug)]
pub struct PortContext {
    pub service: Arc<str>,
    pub version: Arc<str>,
    pub backend: &'static str,
    pub engine: &'static str,
    pub instance: Arc<str>,
    pub agent_epoch: Arc<AtomicU64>,
    pub broker: Arc<AtomicU8>,
    pub queue_depth: Arc<AtomicUsize>,
    pub reply_timeouts: Arc<AtomicU64>,
    pub publish_timeouts: Arc<AtomicU64>,
    pub event_seq: Arc<AtomicU64>,
    pub lost_count: Arc<AtomicU64>,
    pub pending_idle_order: Arc<AtomicU64>,
    pub pending_active_order: Arc<AtomicU64>,
}

impl PortContext {
    pub fn new(service: &str, version: &str, backend: &'static str, engine: &'static str, instance: &str) -> Self {
        Self {
            service: Arc::from(service),
            version: Arc::from(version),
            backend,
            engine,
            instance: Arc::from(instance),
            agent_epoch: Arc::new(AtomicU64::new(0)),
            broker: Arc::new(AtomicU8::new(BROKER_RETRYING)),
            queue_depth: Arc::new(AtomicUsize::new(0)),
            reply_timeouts: Arc::new(AtomicU64::new(0)),
            publish_timeouts: Arc::new(AtomicU64::new(0)),
            event_seq: Arc::new(AtomicU64::new(0)),
            lost_count: Arc::new(AtomicU64::new(0)),
            pending_idle_order: Arc::new(AtomicU64::new(0)),
            pending_active_order: Arc::new(AtomicU64::new(0)),
        }
    }

    /// The `port.*` subtree.
    pub fn port_snapshot(&self, slug_collisions: u64) -> PortSnapshot {
        PortSnapshot {
            level: "L2",
            event_seq: self.event_seq.load(Ordering::Acquire),
            lost_count: self.lost_count.load(Ordering::Acquire),
            queue_depth: self.queue_depth.load(Ordering::Acquire),
            reply_timeouts: self.reply_timeouts.load(Ordering::Acquire),
            publish_timeouts: self.publish_timeouts.load(Ordering::Acquire),
            slug_collisions,
            broker: if self.broker.load(Ordering::Acquire) == BROKER_CONNECTED {
                "connected"
            } else {
                "retrying"
            },
        }
    }

    /// Input authority was lost (a session lock, a VT switch): every agent
    /// admission still queued is refused
    /// `input_cleared`, and the engine releases the agent's holds. Human input
    /// does not call this: the human and agent seats are independent.
    pub fn clear_agent(&self) {
        self.agent_epoch.fetch_add(1, Ordering::AcqRel);
    }
}

/// The reply handle of a long verb (`comp.window.wait`, `close {force}`,
/// `region.select`, `input.sequence`): the engine answers when the verb
/// resolves. Dropping it unanswered reads as `busy` to the caller.
pub struct LongReply(tokio::sync::oneshot::Sender<ControlReply>);

impl LongReply {
    pub fn send(self, reply: ControlReply) {
        let _ = self.0.send(reply);
    }

    /// The caller stopped waiting (its admission deadline passed).
    pub fn is_closed(&self) -> bool {
        self.0.is_closed()
    }
}

/// What compd's engine provides to the `comp` service. Every method runs on
/// the engine's own loop, inside [`PortService::service`], at a stable
/// dispatch boundary; the engine applies policy (policy) and executes
/// its effects before answering.
pub trait CompEngine {
    /// A read snapshot whose volatile leaves cover `scopes`; `None` when the
    /// state cannot be represented (the reads answer `busy`).
    fn snapshot(&mut self, scopes: &ReadScopes) -> Option<CompSnapshot>;
    /// `comp.props.set`, already through the ingress gate.
    fn set(&mut self, path: &str, value: &Value, generation: Option<u64>) -> ControlReply;
    /// A one-pass `comp.window.*` verb.
    fn window(&mut self, op: &WindowOp) -> ControlReply;
    /// A single-step `comp.input.*` verb.
    fn input(&mut self, op: &InputOp) -> ControlReply;
    /// `comp.panel.hold` / `comp.panel.mode` (`sender` is the broker's).
    fn panel(&mut self, request: &PanelRequest) -> ControlReply;
    /// `comp.region.cancel {selection}`: a short owner operation, answered
    /// in this pass. The engine fences the compositor instance and cancels
    /// only the exact active selection.
    fn region_cancel(&mut self, selection: &SelectionIdentity) -> ControlReply;
    /// Start a long verb; answer through `reply` when it resolves. Its
    /// deadline runs from `admitted`.
    fn start_long(&mut self, op: LongOp, reply: LongReply, admitted: Instant);
    /// The broker's live service set (holder cleanup).
    fn services_live(&mut self, live: &BTreeSet<String>);
    /// Seed (or keep) the `props.changed` diff baseline; `false` when no
    /// baseline can be taken (the watch replies `busy`).
    fn watch_props(&mut self, active: bool) -> bool;
    /// Renew the pointer observation lease.
    fn renew_pointer_lease(&mut self);
    /// `compd.truth` ([`crate::port::TRUTH_VERB`]): the engine's own view, as JSON.
    /// Default: null (an engine with no truth to offer).
    fn truth(&mut self) -> Value {
        Value::Null
    }
    /// Two adjacent agent motions as one delivery, if they combine. Default:
    /// never.
    fn coalesce_input(&self, _previous: &InputOp, _next: &InputOp) -> Option<InputOp> {
        None
    }
}

/// The engine's end of the port.
pub struct PortService {
    source: CommandSource,
    context: Arc<PortContext>,
    waker: Waker,
    controls: Vec<PortControl>,
    reads: Vec<PortRequest>,
    watch_active: bool,
}

impl PortService {
    pub fn new(source: CommandSource, context: Arc<PortContext>, waker: Waker) -> Self {
        Self {
            source,
            context,
            waker,
            controls: Vec::new(),
            reads: Vec::new(),
            watch_active: false,
        }
    }

    pub fn context(&self) -> &Arc<PortContext> {
        &self.context
    }

    /// Whether a `props.changed` subscriber exists (the engine diffs only
    /// then).
    pub fn watch_active(&self) -> bool {
        self.watch_active
    }

    /// Run everything the worker admitted. Call after every wake (and, if the
    /// engine prefers, after every dispatch). Returns whether work remains
    /// (the waker has already fired for it).
    pub fn service<E: CompEngine>(&mut self, engine: &mut E) -> bool {
        self.drain(engine);
        self.service_controls(engine);
        self.service_reads(engine);
        let more = !self.controls.is_empty() || !self.reads.is_empty();
        if more {
            (self.waker)();
        }
        more
    }

    fn drain<E: CompEngine>(&mut self, engine: &mut E) {
        while let Ok(command) = self.source.try_recv() {
            match command {
                PortCommand::ServicesLive(live) => engine.services_live(&live),
                PortCommand::Snapshot(request) => {
                    if self.reads.len() < PORT_QUEUE_CAPACITY {
                        self.reads.push(request);
                    }
                }
                PortCommand::Panel(request) => self.push(PortControl::Panel(request)),
                PortCommand::Watch(request) => self.push(PortControl::Watch(request)),
                PortCommand::PointerWatch(request) => self.push(PortControl::PointerWatch(request)),
                PortCommand::Set(request) => self.push(PortControl::Set(request)),
                PortCommand::Window(request) => self.push(PortControl::Window(request)),
                PortCommand::Input(request) => self.push(PortControl::Input(request)),
                PortCommand::Long(request) => self.push(PortControl::Long(request)),
                PortCommand::RegionCancel(request) => self.push(PortControl::RegionCancel(request)),
                PortCommand::Truth(request) => self.push(PortControl::Truth(request)),
                PortCommand::WatchState { active, order } => {
                    if self.controls.len() < PORT_QUEUE_CAPACITY {
                        self.controls.push(PortControl::WatchState { active, order });
                    } else if active {
                        self.context.pending_active_order.fetch_max(order, Ordering::AcqRel);
                    } else {
                        self.context.pending_idle_order.fetch_max(order, Ordering::AcqRel);
                    }
                }
            }
        }
    }

    fn push(&mut self, control: PortControl) {
        // The ingress is bounded at the same capacity, so this only drops
        // when the engine has stopped servicing; a dropped control's reply
        // sender goes with it and the caller reads `busy`.
        if self.controls.len() < PORT_QUEUE_CAPACITY {
            self.controls.push(control);
        }
    }

    fn service_controls<E: CompEngine>(&mut self, engine: &mut E) {
        let mut controls = std::mem::take(&mut self.controls);
        for (active, order) in [
            (false, self.context.pending_idle_order.swap(0, Ordering::AcqRel)),
            (true, self.context.pending_active_order.swap(0, Ordering::AcqRel)),
        ] {
            if order != 0 {
                controls.push(PortControl::WatchState { active, order });
            }
        }
        controls.sort_by_key(PortControl::order);
        let epoch = self.context.agent_epoch.load(Ordering::Acquire);
        controls.retain_mut(|control| !control.refuse_cleared_agent(epoch));
        // Bound agent verbs without reordering: a suffix stays queued.
        let mut agent_ops = 0;
        let mut initial_sequence_available = true;
        let end = controls
            .iter()
            .position(|control| {
                if control.uses_agent() {
                    if matches!(control, PortControl::Long(_)) {
                        if !initial_sequence_available {
                            return true;
                        }
                        initial_sequence_available = false;
                    }
                    agent_ops += 1;
                }
                agent_ops > AGENT_CONTROL_BATCH
            })
            .unwrap_or(controls.len());
        self.controls = controls.split_off(end);
        // Mutations run in arrival order, so a script's set -> minimise ->
        // restore -> click lands in the order it was sent.
        let mut cursor = 0;
        while cursor < controls.len() {
            if controls[cursor].refuse_cleared_agent(self.context.agent_epoch.load(Ordering::Acquire)) {
                cursor += 1;
                continue;
            }
            // Adjacent agent motions coalesce: every reply describes the
            // final delivery, marked `coalesced`.
            if let PortControl::Input(request) = &controls[cursor] {
                let mut op = request.op.clone();
                let mut end = cursor + 1;
                while let Some(PortControl::Input(next)) = controls.get(end) {
                    let Some(combined) = engine.coalesce_input(&op, &next.op) else { break };
                    op = combined;
                    end += 1;
                }
                if end > cursor + 1 {
                    let mut reply = engine.input(&op);
                    if let ControlReply::Body(body) = &mut reply {
                        body["coalesced"] = json!(end - cursor);
                    }
                    for control in &mut controls[cursor..end] {
                        if let PortControl::Input(request) = control
                            && let Some(sender) = request.reply.take()
                        {
                            let _ = sender.send(reply.clone());
                        }
                    }
                    cursor = end;
                    continue;
                }
            }
            let control = &mut controls[cursor];
            cursor += 1;
            match control {
                PortControl::Panel(request) => {
                    let reply = engine.panel(&request.op);
                    if let Some(sender) = request.reply.take() {
                        let _ = sender.send(reply);
                    }
                }
                PortControl::RegionCancel(request) => {
                    let reply = engine.region_cancel(&request.selection);
                    if let Some(sender) = request.reply.take() {
                        let _ = sender.send(reply);
                    }
                }
                PortControl::Set(request) => {
                    let reply = engine.set(&request.path, &request.value, request.generation);
                    if let Some(sender) = request.reply.take() {
                        let _ = sender.send(reply);
                    }
                }
                PortControl::Window(request) => {
                    let reply = engine.window(&request.op);
                    if let Some(sender) = request.reply.take() {
                        let _ = sender.send(reply);
                    }
                }
                PortControl::Input(request) => {
                    let reply = engine.input(&request.op);
                    if let Some(sender) = request.reply.take() {
                        let _ = sender.send(reply);
                    }
                }
                PortControl::Long(request) => {
                    // The ingress slot is released here: the verb now waits
                    // on its own permit and deadline, not the bounded queue.
                    request.slot.take();
                    if let (Some(op), Some(reply)) = (request.op.take(), request.reply.take()) {
                        engine.start_long(op, LongReply(reply), request.admitted);
                    }
                }
                PortControl::Watch(_)
                | PortControl::PointerWatch(_)
                | PortControl::WatchState { .. }
                | PortControl::Truth(_) => {}
            }
        }

        let mut desired_active = self.watch_active;
        let mut watches = Vec::new();
        for control in controls {
            match control {
                PortControl::PointerWatch(request) => {
                    engine.renew_pointer_lease();
                    let _ = request.reply.send(ControlReply::PointerWatch {
                        topic: topic_name(&self.context.service, POINTER_TOPIC_SUFFIX),
                        lease_ms: POINTER_LEASE.as_millis() as u64,
                    });
                }
                PortControl::Watch(request) => {
                    desired_active = true;
                    watches.push(request);
                }
                PortControl::WatchState { active, .. } => desired_active = active,
                // After this batch's mutations, like the watches.
                PortControl::Truth(request) => {
                    let _ = request.reply.send(ControlReply::Body(engine.truth()));
                }
                PortControl::Set(_)
                | PortControl::Panel(_)
                | PortControl::RegionCancel(_)
                | PortControl::Window(_)
                | PortControl::Input(_)
                | PortControl::Long(_) => {}
            }
        }
        let needs_seed = !watches.is_empty() || desired_active != self.watch_active;
        let seeded = needs_seed && engine.watch_props(desired_active);
        if needs_seed {
            self.watch_active = desired_active && seeded;
        }
        for request in watches {
            let reply = if seeded {
                ControlReply::Watch {
                    topic: topic_name(&self.context.service, PROPS_TOPIC_SUFFIX),
                    event_seq: self.context.event_seq.load(Ordering::Acquire),
                    lost_count: self.context.lost_count.load(Ordering::Acquire),
                }
            } else {
                ControlReply::Busy
            };
            let _ = request.reply.send(reply);
        }
    }

    /// Reads admitted after a still-queued control cannot observe the state
    /// before it: only reads older than every queued control run now, all
    /// from one snapshot scoped to what they asked for.
    fn service_reads<E: CompEngine>(&mut self, engine: &mut E) {
        if self.reads.is_empty() {
            return;
        }
        let fence = self.controls.iter().map(PortControl::order).min();
        let mut ready = Vec::new();
        for request in std::mem::take(&mut self.reads) {
            if fence.is_some_and(|order| request.order > order) {
                self.reads.push(request);
            } else {
                ready.push(request);
            }
        }
        if ready.is_empty() {
            return;
        }
        let mut scopes = ReadScopes::Paths(Vec::new());
        for request in &ready {
            scopes.add(request.scope.as_deref());
        }
        // An unrepresentable state drops the replies: the readers see `busy`.
        let Some(snapshot) = engine.snapshot(&scopes).map(Arc::new) else {
            return;
        };
        for request in ready {
            let _ = request.reply.send(Arc::clone(&snapshot));
        }
    }
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;

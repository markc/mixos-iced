//! The native `comp` Bus service.
//!
//! comp-service owns the transport: a worker thread ("compd-port") with its own
//! current-thread tokio runtime and the supervised noded connection. This
//! module wires its engine half into compd's calloop loop:
//!
//! - the comp-service waker is a calloop ping; its source only marks work pending,
//!   so the loop wakes for Bus traffic and never polls for it;
//! - [`Bus::service`] runs from the `event_loop.run` post-dispatch closure,
//!   AFTER `drain_protocol`, so a Bus read never observes state the drain has
//!   not applied (comp serviced its port after every dispatch the same way);
//! - [`Engine`] is the `CompEngine` adapter over policy-host.
//!
//! Answered: `comp.ping`, the reads (`comp.info`, `comp.props.*`,
//! `comp.windows.list`) from the projection, `comp.window.wait`,
//! `comp.props.watch`, `compd.truth`, and the workspace controls
//! (`comp.workspace.switch`, `comp.window.send_to_workspace`, the
//! `workspaces.*` / `windows.s<id>.workspace` sets). Every other control verb
//! answers `busy` until it is implemented (an honest refusal, never a fake
//! success).
//!
//! Published: `surface.mapped` / `surface.unmapped`,
//! `focus.changed` and, while watched, `props.changed`, from policy-host's
//! edge pass, run after each dispatch BEFORE the port is serviced (so
//! observers hear the state a Bus reply is about to describe).
//!
//! `comp.window.wait`: a waiter is admitted
//! with a calloop deadline timer and re-evaluated after every dispatch while
//! any is pending. Its edges (map, focus, unmap, destroy) all arrive as loop
//! events, so nothing polls; the timer only answers `timeout`.
//!
//! Injected input: the single `comp.input.*` verbs are
//! policy-host `input`; `comp.input.sequence` runs on [`sequence`]'s driver;
//! `comp.pointer.watch` opens a demand lease and `pointer.changed` is
//! published from here while it holds (policy `pointer`). A session pause
//! (VT switch, lost input authority) clears the agent seat: the agent epoch
//! moves, so comp-service refuses the agent admissions still queued
//! `input_cleared`, and the agent's runs end `input_cleared`. Human input does
//! not clear the agent.
//!
//! Region select: `comp.region.select` is policy-host
//! `region`; the run holds the human seat, its reply waits here and is sent
//! from `Bus::service` once decided; one timer waits at its deadline.
//! `comp.region.cancel {selection}` is a short control: the engine fences
//! the compositor instance here, policy-host cancels only the exact active
//! run through the ordinary finish path, and the pending select reply
//! completes from the existing `Bus::service` region section.
//!
//! Panel holders: `comp.panel.hold` / `mode` are
//! policy-host `panel::request`; every pass runs `panel::service` (membership,
//! probes, enforcement, focus restoration) and publishes the `panel.command`s
//! owed; one timer waits at the holders' next deadline.
//!
//! Hot corners: seat feeds the detector from the
//! pointer path (`world::comp::corners`); the topics it earned are
//! published here, before the edge pass, and the dwell timer is a one-shot
//! armed only while a dwell is pending ([`DwellTimer`]).

mod sequence;

use std::cell::Cell;
use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use serde_json::{Value, json};
use smithay::reexports::calloop::ping::make_ping;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::{LoopHandle, RegistrationToken};

use comp_model::observation::{ObservationRecord, PanelRequest, PointerPosition, PointerSample};
use comp_model::reply::ControlReply;
use comp_model::request::{InputOp, LongOp, SelectionIdentity, WaitSpec, WindowOp};
use comp_model::snapshot::{CompSnapshot, ReadScopes, project_window_row, surface_key};
use comp_service::{
    CompEngine, LongReply, ObservationProducer, PortContext, PortService, PortWorker,
};
use outputs::render_contract::contract::RendererId;
use policy::window::{WaitResolution, WindowFacts};
use world::state::Loop;

use crate::cli::Backend;

/// The renderer `info.engine` reports. Both backend contracts are GLES
/// (nested `WinitContract::id`, native `NativeContract::id`); Vulkan is
/// not in compd's build.
const RENDERER: RendererId = RendererId::Gles;

/// What docked Mix Scenes panels reserve on each output: the scene
/// host keys its panels by compd's output key (`make model serial`), the
/// usable area by output name. Empty with no scene host.
fn refresh_scene_reserved(lp: &mut Loop) {
    use dispatcher::wire::trait_::wire_trait::WireTrait;
    let mut reserved = std::collections::BTreeMap::new();
    for output in lp.inner.host_space().state.outputs() {
        let zones = scene_host::exclusive_zones(&world::state::state::output_key(output));
        if zones.is_empty() {
            continue;
        }
        let mut edges = world::comp::usable::Reserved::default();
        for (edge, px) in zones {
            edges.add(edge.as_str(), px);
        }
        reserved.insert(output.name(), edges);
    }
    if reserved != lp.inner.comp.reserved {
        lp.inner.comp.reserved = reserved;
    }
}

/// Reconcile settings-owned panels before frame work, even without a comp Bus
/// port. Scene activation runs after the port; do not leave windows using the
/// previous reservation until another dispatch happens to arrive.
pub(crate) fn reconcile_scene_geometry(lp: &mut Loop) {
    refresh_scene_reserved(lp);
    policy_host::control::refresh_usable(lp);
    #[cfg(feature = "backend-native")]
    let parked = native::render::execute::execute::frames_parked(lp);
    #[cfg(not(feature = "backend-native"))]
    let parked = false;
    if !parked && lp.inner.comp.input_geometry_dirty() {
        // The previous frame has now applied pending iced sizes/scales. Publish
        // current scene rows before routing the unchanged pointer. A delayed
        // client buffer can remove a letterbox after scene geometry settled;
        // the commit observer requests the same reconciliation in that case.
        refresh_scene_surfaces(lp);
        if policy_host::input::try_retarget_pointer(lp) {
            lp.inner.comp.clear_input_geometry_dirty();
        }
    }
}

/// The scene host's surfaces and keyboard focus as comp.props rows: they stand
/// in for Quoin's layer surfaces, so `surfaces.*`, `stack` and
/// `focus.keyboard` must carry them too. Positions are made global from
/// each output's logical origin; outputs are keyed as comp.props keys them.
fn refresh_scene_surfaces(lp: &mut Loop) {
    use dispatcher::wire::trait_::wire_trait::WireTrait;
    let mut inputs = Vec::new();
    let space = &lp.inner.host_space().state;
    for output in space.outputs() {
        let Some(geometry) = space.output_geometry(output) else {
            continue;
        };
        let (ox, oy) = (geometry.loc.x as f32, geometry.loc.y as f32);
        for surface in scene_host::surfaces(&world::state::state::output_key(output)) {
            inputs.push(world::comp::scenes::SceneInput {
                key: surface.id,
                output: comp_model::snapshot::output_key(&output.name()),
                x: ox + surface.x,
                y: oy + surface.y,
                width: surface.width,
                height: surface.height,
                stratum: surface.stratum,
                interactivity: surface.interactivity,
                exclusive_zone: surface.exclusive_zone.max(0.0).ceil() as i32,
            });
        }
    }
    let focus = scene_host::focused_scene(lp);
    lp.inner.comp.set_scene_surfaces(inputs, focus.as_deref());
}

/// The service name: `--bus-service`, else the default (`comp` on KMS,
/// `comp-nested` nested).
pub fn service_name(backend: Backend, requested: Option<&str>) -> String {
    match (requested, backend) {
        (Some(name), _) => name.to_string(),
        (None, Backend::Kms) => comp_service::port::SERVICE.to_string(),
        (None, Backend::Nested) => comp_service::port::NESTED_SERVICE.to_string(),
    }
}

/// An admitted `comp.window.wait`.
struct Waiter {
    kind: WaiterKind,
    reply: LongReply,
    admitted: Instant,
    deadline: Instant,
}

/// What a waiter waits for (comp `WaiterKind`).
enum WaiterKind {
    Hardware(comp_model::request::HardwareWaitSpec),
    /// `comp.window.wait`.
    Wait(WaitSpec),
    /// `comp.window.close {force}`: the polite close went out at admission;
    /// `gone` as soon as the window is, else the kill at the deadline.
    ForceClose {
        id: u64,
        generation: u64,
    },
}

/// The engine half of the port, owned by the event loop's closure.
pub struct Bus {
    port: PortService,
    /// The outbox the publisher task drains (dropping it ends the outbox).
    producer: ObservationProducer,
    edges: policy_host::Edges,
    context: Arc<PortContext>,
    binding_profile: &'static str,
    pending: Rc<Cell<bool>>,
    handle: LoopHandle<'static, Loop>,
    waiters: Vec<Waiter>,
    dwell: OneShot,
    /// The panel holders' next deadline (conceal, enforce grace, probe).
    panel_timer: OneShot,
    /// The running `comp.region.select`'s reply, and its deadline timer.
    region_reply: Option<LongReply>,
    region_timer: OneShot,
    sequences: sequence::Sequences,
    pointer: PointerWatch,
    /// The session was paused at the last pass (a pause clears the agent).
    paused: bool,
    /// A session lock was in force at the last pass (entry clears the agent).
    locked: bool,
    worker: Option<PortWorker>,
}

/// `comp.pointer.watch` (comp `service_pointer`): the demand lease, the last
/// pointer state published, and the one timer at the lease's deadline.
struct PointerWatch {
    lease: policy::pointer::PointerLease,
    seen: Option<Option<(String, f64, f64)>>,
    timer: OneShot,
    /// `timestamp_ms` counts from here (this observation instance).
    epoch: Instant,
}

/// One calloop timer at a deadline that moves (comp `rearm_corner_timer`, the
/// pointer observation timer): re-armed only when the deadline changes, its
/// callback runs `on_fire` (the corner dwell re-samples the resting pointer;
/// the pointer watch only needs the loop to turn). Nothing polls.
#[derive(Default)]
struct OneShot {
    /// The armed timer, its deadline, and whether it has fired (a fired
    /// timer's source is gone and must not be removed).
    armed: Option<(RegistrationToken, Instant, Rc<Cell<bool>>)>,
}

impl OneShot {
    fn rearm(
        &mut self,
        handle: &LoopHandle<'static, Loop>,
        deadline: Option<Instant>,
        on_fire: fn(&mut Loop),
    ) {
        let live = self.armed.as_ref().filter(|(_, _, fired)| !fired.get());
        if live.is_some_and(|(_, at, _)| Some(*at) == deadline) {
            return;
        }
        if let Some((token, _, fired)) = self.armed.take()
            && !fired.get()
        {
            handle.remove(token);
        }
        let Some(deadline) = deadline else { return };
        let fired = Rc::new(Cell::new(false));
        let flag = Rc::clone(&fired);
        match handle.insert_source(
            Timer::from_deadline(deadline),
            move |_, _, lp: &mut Loop| {
                flag.set(true);
                on_fire(lp);
                TimeoutAction::Drop
            },
        ) {
            Ok(token) => self.armed = Some((token, deadline, fired)),
            Err(error) => model::warn!("Bus one-shot timer: {error}"),
        }
    }
}

impl Bus {
    /// Prepare the port, register its wake source on the loop and start the
    /// worker thread. The broker connection is the worker's: a missing noded
    /// is retried with backoff and never blocks the compositor.
    pub fn start(
        service: String,
        backend: Backend,
        handle: &LoopHandle<'static, Loop>,
    ) -> Result<Self, String> {
        let (ping, source) = make_ping().map_err(|error| format!("Bus wake source: {error}"))?;
        let waker: comp_service::Waker = Arc::new(move || ping.ping());
        let wake = Arc::clone(&waker);
        let identity = comp_service::PortIdentity {
            service,
            version: buildinfo::build_info!().version.to_string(),
            backend: backend.label(),
            engine: RENDERER.label(),
            noded_url: comp_service::default_noded_url(),
        };
        let noded_url = identity.noded_url.clone();
        let (wiring, starter) = comp_service::prepare(identity, waker)?;
        let pending = Rc::new(Cell::new(false));
        let flag = Rc::clone(&pending);
        handle
            .insert_source(source, move |_, _, _| flag.set(true))
            .map_err(|error| format!("Bus wake source: {error}"))?;
        let worker = starter.start()?;
        model::info!(
            "comp Bus port: service {} via {noded_url}",
            wiring.context.service
        );
        Ok(Self {
            port: wiring.service,
            producer: wiring.observation_producer,
            edges: policy_host::Edges::new(),
            context: wiring.context,
            binding_profile: match backend {
                Backend::Nested => "nested",
                Backend::Kms => "kms-live",
            },
            pending,
            handle: handle.clone(),
            waiters: Vec::new(),
            dwell: OneShot::default(),
            panel_timer: OneShot::default(),
            region_reply: None,
            region_timer: OneShot::default(),
            sequences: sequence::Sequences::new(wake),
            pointer: PointerWatch {
                lease: policy::pointer::PointerLease::default(),
                seen: None,
                timer: OneShot::default(),
                epoch: Instant::now(),
            },
            paused: false,
            locked: false,
            worker: Some(worker),
        })
    }

    /// Run everything the worker admitted, then the pending waits. Called from
    /// the post-dispatch closure. The port runs only when its wake source (or a
    /// wait deadline) fired; `PortService::service` re-fires the waker when it
    /// leaves work queued (the agent batch bound), so the next iteration picks
    /// it up. Waits are re-evaluated on every dispatch while any is pending.
    pub fn service(&mut self, lp: &mut Loop) {
        // The camera pinned to identity: world coordinates are
        // output-logical, as every comp coordinate is. Re-pinned every pass so a
        // resize or a pane split never projects through a stale camera.
        if world::camera::pin::pin(&mut lp.inner) {
            lp.state
                .schedule_redraw(dispatcher::state::state::RedrawReason::Output);
        }
        // The pointer starts at the output's centre, not in its top-left corner.
        world::camera::pin::centre_pointer_once(lp);
        // The workspace model keys its current workspace on the default output.
        policy_host::control::refresh_default_output(lp);
        // Docked scene panel edges reserve their thickness, pulled
        // from the scene host every pass (it changes only on a panel answer
        // or a model tick, both of which redraw, so the next pass sees it).
        refresh_scene_reserved(lp);
        // ... and its surfaces and keyboard focus, for comp.props.
        refresh_scene_surfaces(lp);
        // Layer exclusive zones: each output's usable area; maximised windows
        // follow it when it moves.
        policy_host::control::refresh_usable(lp);
        // The session lock's transitions, keyboard and `locked` confirmation
        // (idempotent, also run from the loop closure).
        policy_host::input::release_keys_for_lock(lp);
        world::comp::session_lock::service(lp);
        // The exclusive-keyboard latch (F2; idempotent, also run from the loop closure).
        world::comp::latch::service(lp);
        // A window that left fullscreen gets its band back (F3/F5).
        world::comp::fullscreen::service(lp);
        // The comp policy's hidden decision stamped for the hit driver (E5).
        world::comp::visibility::sync_hidden(lp);
        // `input.host.passthrough` exists on the nested backend only.
        lp.inner.comp.injection.host_passthrough_available = self.binding_profile == "nested";
        // The key-binding table for this profile (built once; batch B).
        lp.inner.comp.bindings.ensure(self.binding_profile);
        // Losing input authority (a session pause) clears the agent seat.
        let paused = matches!(
            lp.inner.status_session,
            world::state::state::StatusSession::Paused
        );
        // So does a session lock beginning.
        let locked = world::comp::session_lock::active(lp);
        let lock_began = locked && !self.locked;
        let lock_ended = !locked && self.locked;
        self.locked = locked;
        // Unlocked: the pointer finds what is under it again (comp
        // `restore_unlocked_focus_and_input`).
        if lock_ended {
            policy_host::input::retarget_pointer(lp);
        }
        if (paused && !self.paused) || lock_began {
            self.context.clear_agent();
            self.sequences.clear_agent(lp);
            policy_host::input::clear_agent(lp);
        }
        self.paused = paused;
        // Sequence runs a delay timer or a yield made ready.
        self.sequences.service(lp, &self.handle);
        // The clients' own maximise / minimise requests, queued by the drain.
        policy_host::control::apply_client_requests(lp);
        // The interactive move/resize grab's latest step.
        policy_host::control::apply_interactive(lp);
        // The renderer's occlusion decisions since the last pass, as row
        // changes for the edge pass below (F6).
        world::comp::occlusion::service(lp);
        // The corner topics the pointer path earned, then the dwell timer for
        // whatever is pending now.
        for topic in policy_host::edges::corner_topics(lp) {
            self.producer
                .offer_next(move |event_seq| topic.into_record(event_seq));
        }
        self.dwell
            .rearm(&self.handle, lp.inner.comp.corners.next_deadline(), |lp| {
                lp.inner.comp.corners.tick()
            });
        let (context, binding_profile) = (&self.context, self.binding_profile);
        for record in self.edges.pass(lp, || identity(context, binding_profile)) {
            self.producer
                .offer_next(move |event_seq| record.with_event_seq(event_seq));
        }
        // The causes noted for this pass's changes are spent (F4).
        lp.inner.comp.causes.clear();
        if self.pending.replace(false) {
            let mut engine = Engine {
                lp: &mut *lp,
                context: &self.context,
                binding_profile: self.binding_profile,
                handle: &self.handle,
                pending: &self.pending,
                waiters: &mut self.waiters,
                edges: &mut self.edges,
                sequences: &mut self.sequences,
                pointer_lease: &mut self.pointer.lease,
                region_reply: &mut self.region_reply,
            };
            self.port.service(&mut engine);
        }
        // The window and workspace chords the key bindings queued: by device
        // input in this dispatch, or by a `comp.input.*` key the port just ran
        // (after it, so an injected chord lands in this pass, not the next).
        policy_host::control::apply_bindings(lp);
        // X11's EWMH desktops and HIDDEN after any state change this pass
        // (E1): after the port and the chords, so a verb's change lands now.
        policy_host::x11::sync_if_changed(lp);
        // ...and the hidden stamps, so a window a verb hid this pass is out of
        // the hit driver before the next input dispatch (E5).
        world::comp::visibility::sync_hidden(lp);
        // What the port's verbs and the chords changed this pass reaches the
        // topics NOW, with its noted causes. The pass above ran before them,
        // so observers see the applied control before its next frame. This
        // also covers state-only controls. Revision-gated, so a pass with no
        // change is free.
        let (context, binding_profile) = (&self.context, self.binding_profile);
        for record in self.edges.pass(lp, || identity(context, binding_profile)) {
            self.producer
                .offer_next(move |event_seq| record.with_event_seq(event_seq));
        }
        lp.inner.comp.causes.clear();
        self.service_pointer(lp);
        // The region selection, once decided (or its caller gone).
        if let Some(reply) = self.region_reply.take() {
            if reply.is_closed() {
                policy_host::region::abandon(lp);
            } else if let Some(answer) = policy_host::region::service(lp, Instant::now()) {
                reply.send(answer);
            } else {
                self.region_reply = Some(reply);
            }
        }
        self.region_timer
            .rearm(&self.handle, policy_host::region::next_deadline(lp), |_| {});
        // The panel holders after everything this dispatch changed.
        let deadline = policy_host::panel::service(lp, Instant::now());
        self.panel_timer.rearm(&self.handle, deadline, |_| {});
        for (output, edge, surface, reveal) in policy_host::panel::take_commands(lp) {
            self.producer
                .offer_next(move |event_seq| ObservationRecord::PanelCommand {
                    output,
                    edge,
                    surface,
                    reveal,
                    event_seq,
                });
        }
        if !self.waiters.is_empty() {
            self.settle_waiters(lp);
        }
    }

    /// `pointer.changed` while a watcher holds the lease (comp
    /// `service_pointer`): published when the cursor (or whether it may be
    /// reported) changed, at most once per interval; one timer at the
    /// lease's deadline, none without a watcher.
    fn service_pointer(&mut self, lp: &Loop) {
        let now = Instant::now();
        let watch = &mut self.pointer;
        if watch.lease.active(now) {
            let seen = policy_host::input::human_pointer(lp);
            if watch.seen.as_ref() != Some(&seen) {
                watch.seen = Some(seen.clone());
                watch.lease.changed();
            }
            if watch.lease.take(now) {
                let (output, position) = match seen {
                    Some((output, x, y)) => (Some(output), Some(PointerPosition { x, y })),
                    None => (None, None),
                };
                let sample = PointerSample {
                    version: 1,
                    instance: Arc::clone(&self.context.instance),
                    output,
                    valid: position.is_some(),
                    position,
                    timestamp_ms: u64::try_from(watch.epoch.elapsed().as_millis())
                        .unwrap_or(u64::MAX),
                };
                self.producer
                    .offer_next(move |event_seq| ObservationRecord::PointerChanged {
                        sample,
                        event_seq,
                    });
            }
        } else {
            watch.seen = None;
        }
        let deadline = watch.lease.deadline(now);
        watch.timer.rearm(&self.handle, deadline, |_| {});
    }

    /// Answer every wait that resolved, timed out or lost its caller.
    fn settle_waiters(&mut self, lp: &Loop) {
        self.waiters.retain(|waiter| !waiter.reply.is_closed());
        if self.waiters.is_empty() {
            return;
        }
        let snapshot = policy_host::project(
            lp,
            identity(&self.context, self.binding_profile),
            &ReadScopes::All,
        );
        let facts = |id: surfaces::SurfaceId| WindowFacts {
            presented_since_map: lp.inner.comp.presentation.presented_since_map(id),
            ..policy_host::control::window_facts(lp, id)
        };
        let now = Instant::now();
        let mut still = Vec::with_capacity(self.waiters.len());
        for waiter in std::mem::take(&mut self.waiters) {
            let waited_ms =
                u64::try_from(waiter.admitted.elapsed().as_millis()).unwrap_or(u64::MAX);
            let Waiter {
                kind,
                reply,
                admitted,
                deadline,
            } = waiter;
            match kind {
                WaiterKind::Hardware(spec) => {
                    let kind = hardware_kind(spec.until);
                    if let Some(event) = lp
                        .inner
                        .comp
                        .hardware
                        .latest_before(kind, spec.after, deadline)
                    {
                        reply.send(ControlReply::Body(json!({"instance":self.context.instance,"until":kind.name(),"event":{"sequence":event.sequence,"device":event.device},"waited_ms":waited_ms})));
                    } else if now >= deadline {
                        reply.send(ControlReply::refused(
                            "timeout",
                            json!({"until":kind.name(),"waited_ms":waited_ms}),
                        ));
                    } else {
                        still.push(Waiter {
                            kind: WaiterKind::Hardware(spec),
                            reply,
                            admitted,
                            deadline,
                        });
                    }
                }
                WaiterKind::Wait(spec) => {
                    let outcome = policy::window::wait_outcome(
                        &lp.inner.comp.registry,
                        &spec,
                        world::comp::session_lock::active(lp),
                        lp.inner.comp.current_workspace(),
                        facts,
                    );
                    match outcome {
                        Some(resolution) => {
                            let window = window_value(&snapshot, resolution);
                            reply.send(policy::window::wait_reply(&spec, window, waited_ms));
                        }
                        None if now >= deadline => reply.send(ControlReply::refused(
                            "timeout",
                            json!({"until": spec.until.name(), "waited_ms": waited_ms}),
                        )),
                        None => still.push(Waiter {
                            kind: WaiterKind::Wait(spec),
                            reply,
                            admitted,
                            deadline,
                        }),
                    }
                }
                WaiterKind::ForceClose { id, generation } => {
                    use policy::window::{
                        ForceCloseOutcome, close_reply, force_close_at_deadline, kill_reply,
                    };
                    match force_close_at_deadline(
                        &lp.inner.comp.registry,
                        id,
                        generation,
                        world::comp::session_lock::active(lp),
                    ) {
                        ForceCloseOutcome::Gone => {
                            reply.send(close_reply(id, generation, "gone", waited_ms))
                        }
                        _ if now < deadline => still.push(Waiter {
                            kind: WaiterKind::ForceClose { id, generation },
                            reply,
                            admitted,
                            deadline,
                        }),
                        ForceCloseOutcome::Locked => reply.send(ControlReply::Locked),
                        ForceCloseOutcome::StillOpen(refusal) => reply.send(refusal),
                        ForceCloseOutcome::Kill { id: target, mapped } => {
                            let (pid, windows) = policy_host::control::kill_client(lp, target);
                            reply.send(kill_reply(id, generation, waited_ms, mapped, pid, windows));
                        }
                    }
                }
            }
        }
        self.waiters = still;
    }

    /// Deregister and close (bounded: about 300 ms). Called once the loop
    /// has stopped.
    pub fn shutdown(&mut self) {
        if let Some(mut worker) = self.worker.take() {
            worker.begin_shutdown();
            worker.finish();
        }
    }
}

fn hardware_kind(until: comp_model::request::HardwareUntil) -> world::comp::hardware::Kind {
    use comp_model::request::HardwareUntil;
    use world::comp::hardware::Kind;
    match until {
        HardwareUntil::Keyboard => Kind::Keyboard,
        HardwareUntil::Pointer => Kind::Pointer,
        HardwareUntil::Paused => Kind::Paused,
        HardwareUntil::Active => Kind::Active,
    }
}

fn hardware_snapshot(lp: &Loop, context: &PortContext, profile: &str) -> ControlReply {
    use world::comp::hardware::Kind;
    let witness = &lp.inner.comp.hardware;
    let events: serde_json::Map<String, Value> =
        [Kind::Keyboard, Kind::Pointer, Kind::Paused, Kind::Active]
            .into_iter()
            .map(|kind| {
                (
                    kind.name().to_string(),
                    witness.latest(kind).map_or(
                        Value::Null,
                        |event| json!({"sequence":event.sequence,"device":event.device}),
                    ),
                )
            })
            .collect();
    ControlReply::Body(
        json!({"instance":context.instance,"native":profile == "kms-live","sequence":witness.sequence(),
        "session_active":matches!(lp.inner.status_session,world::state::state::StatusSession::Active),"events":events}),
    )
}

fn identity(context: &PortContext, binding_profile: &'static str) -> policy_host::Identity {
    policy_host::Identity {
        service: Arc::clone(&context.service),
        version: Arc::clone(&context.version),
        backend: context.backend,
        engine: context.engine,
        instance: Arc::clone(&context.instance),
        binding_profile,
        port: context.port_snapshot(0),
    }
}

/// A resolved wait's `window`: the window row of that surface (comp
/// `window_row`), or `{"id": id}` when it has none.
fn window_value(snapshot: &CompSnapshot, resolution: WaitResolution) -> Value {
    match resolution {
        WaitResolution::Null => Value::Null,
        WaitResolution::Window(id) => snapshot
            .surfaces
            .get(&surface_key(id.0))
            .and_then(|row| serde_json::to_value(project_window_row(row)).ok())
            .unwrap_or_else(|| json!({"id": id.0})),
    }
}

/// The `CompEngine` adapter: everything it reads is in `Loop`; the waits it
/// admits go to the Bus.
struct Engine<'a> {
    lp: &'a mut Loop,
    context: &'a PortContext,
    binding_profile: &'static str,
    handle: &'a LoopHandle<'static, Loop>,
    pending: &'a Rc<Cell<bool>>,
    waiters: &'a mut Vec<Waiter>,
    edges: &'a mut policy_host::Edges,
    sequences: &'a mut sequence::Sequences,
    pointer_lease: &'a mut policy::pointer::PointerLease,
    region_reply: &'a mut Option<LongReply>,
}

impl Engine<'_> {
    /// Admit a wait: refuse an id never handed out, else queue it with a
    /// deadline timer (the timer only marks the Bus pending; the waiter is
    /// answered from `Bus::settle_waiters`, after this dispatch).
    fn start_wait(&mut self, spec: WaitSpec, reply: LongReply, admitted: Instant) {
        if let Err(refusal) = policy::window::start_wait(&self.lp.inner.comp.registry, &spec) {
            reply.send(refusal);
            return;
        }
        let deadline = admitted + spec.timeout;
        let flag = Rc::clone(self.pending);
        if let Err(error) =
            self.handle
                .insert_source(Timer::from_deadline(deadline), move |_, _, _| {
                    flag.set(true);
                    TimeoutAction::Drop
                })
        {
            model::warn!("comp.window.wait: no deadline timer: {error}");
            reply.send(ControlReply::Busy);
            return;
        }
        self.waiters.push(Waiter {
            kind: WaiterKind::Wait(spec),
            reply,
            admitted,
            deadline,
        });
    }

    /// Admit `comp.window.close {force}`: the polite close now (also on a
    /// refusal), then a deadline timer for the kill.
    fn start_force_close(
        &mut self,
        id: u64,
        generation: u64,
        timeout: std::time::Duration,
        reply: LongReply,
        admitted: Instant,
    ) {
        let effects = match policy::window::start_force_close(
            &self.lp.inner.comp.registry,
            id,
            generation,
            world::comp::session_lock::active(self.lp),
        ) {
            Ok(effects) => effects,
            Err((refusal, effects)) => {
                policy_host::control::execute(self.lp, effects);
                reply.send(refusal);
                return;
            }
        };
        policy_host::control::execute(self.lp, effects);
        let deadline = admitted + timeout;
        let flag = Rc::clone(self.pending);
        if let Err(error) =
            self.handle
                .insert_source(Timer::from_deadline(deadline), move |_, _, _| {
                    flag.set(true);
                    TimeoutAction::Drop
                })
        {
            model::warn!("comp.window.close force: no deadline timer: {error}");
            reply.send(ControlReply::Busy);
            return;
        }
        self.waiters.push(Waiter {
            kind: WaiterKind::ForceClose { id, generation },
            reply,
            admitted,
            deadline,
        });
    }
}

impl CompEngine for Engine<'_> {
    fn snapshot(&mut self, scopes: &ReadScopes) -> Option<CompSnapshot> {
        Some(policy_host::project(
            self.lp,
            identity(self.context, self.binding_profile),
            scopes,
        ))
    }

    // Controls: the workspace verbs and sets; everything else
    // answers `busy` until it is implemented.
    fn set(&mut self, path: &str, value: &Value, generation: Option<u64>) -> ControlReply {
        policy_host::control::set(self.lp, path, value, generation).unwrap_or(ControlReply::Busy)
    }
    fn window(&mut self, op: &WindowOp) -> ControlReply {
        if matches!(op, WindowOp::HardwareSnapshot) {
            return hardware_snapshot(self.lp, self.context, self.binding_profile);
        }
        policy_host::control::window(self.lp, op).unwrap_or(ControlReply::Busy)
    }
    fn input(&mut self, op: &InputOp) -> ControlReply {
        policy_host::input::input(self.lp, op)
    }
    fn panel(&mut self, request: &PanelRequest) -> ControlReply {
        policy_host::panel::request(self.lp, request)
    }
    fn region_cancel(&mut self, selection: &SelectionIdentity) -> ControlReply {
        // The instance fence: a selection sent to another compositor
        // process (or before a restart) can never cancel this one, and the
        // refusal changes no owner state.
        if selection.instance != self.context.instance.as_ref() {
            return ControlReply::refused("stale_instance", json!({}));
        }
        policy_host::region::cancel(self.lp, selection)
    }
    fn start_long(&mut self, op: LongOp, reply: LongReply, admitted: Instant) {
        // `comp.input.sequence`.
        let Some((op, reply)) = self
            .sequences
            .start(self.lp, self.handle, op, reply, admitted)
        else {
            return;
        };
        match op {
            LongOp::HardwareWait(spec) => {
                if self.binding_profile != "kms-live" {
                    reply.send(ControlReply::refused("unsupported_backend", json!({})));
                    return;
                }
                if spec.instance != self.context.instance.as_ref() {
                    reply.send(ControlReply::refused("stale_instance", json!({})));
                    return;
                }
                if spec.after > self.lp.inner.comp.hardware.sequence() {
                    reply.send(ControlReply::refused("invalid_sequence", json!({})));
                    return;
                }
                let deadline = admitted + spec.timeout;
                let flag = Rc::clone(self.pending);
                if self
                    .handle
                    .insert_source(Timer::from_deadline(deadline), move |_, _, _| {
                        flag.set(true);
                        TimeoutAction::Drop
                    })
                    .is_err()
                {
                    reply.send(ControlReply::Busy);
                    return;
                }
                self.waiters.push(Waiter {
                    kind: WaiterKind::Hardware(spec),
                    reply,
                    admitted,
                    deadline,
                });
            }
            LongOp::CaptureFrame(spec) => {
                policy_host::capture::start(self.lp, spec, admitted, move |answer| {
                    reply.send(answer)
                });
            }
            LongOp::RegionSelect {
                output,
                timeout,
                selection,
            } => {
                if self.region_reply.is_some() {
                    reply.send(ControlReply::Busy);
                    return;
                }
                let busy = !self.sequences.is_empty();
                match policy_host::region::start(
                    self.lp,
                    output.as_deref(),
                    timeout,
                    admitted,
                    busy,
                    selection,
                ) {
                    Ok(()) => *self.region_reply = Some(reply),
                    Err(answer) => reply.send(answer),
                }
            }
            LongOp::Wait(spec) => self.start_wait(spec, reply, admitted),
            LongOp::ForceClose {
                id,
                generation,
                timeout,
            } => self.start_force_close(id, generation, timeout, reply, admitted),
            _ => reply.send(ControlReply::Busy),
        }
    }

    // Panel holders are the only consumer of the live set.
    fn services_live(&mut self, live: &BTreeSet<String>) {
        policy_host::panel::services_live(self.lp, live);
    }

    // Seed (or keep) the props.changed baseline; idle drops it.
    fn watch_props(&mut self, active: bool) -> bool {
        self.edges.watch(
            self.lp,
            identity(self.context, self.binding_profile),
            active,
        )
    }

    // pointer.watch: (re)open the lease; `Bus::service_pointer`
    // publishes.
    fn renew_pointer_lease(&mut self) {
        self.pointer_lease.renew(Instant::now());
    }

    // compd.truth: compd's own view, for the Bus-truth comparator.
    fn truth(&mut self) -> Value {
        policy_host::truth(self.lp)
    }
}

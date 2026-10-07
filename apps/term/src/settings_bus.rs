// SPDX-License-Identifier: MIT OR Apache-2.0
//! The desktop adapter: one supervised Bus connection that serves both the
//! `term.*` verb lane (through term-core's narrow serve entry) and the shared
//! settings lane, multiplexed on one worker and one runtime. The connection
//! starts without blocking — offline is a state, not a startup failure — and
//! the settings lane's client IS the verb lane's client: never a second
//! connection. The PTY supervisor (`native_lane`) is untouched and stays
//! distinct from this lane.
use crate::presentation::{Content, LocalContext};
use ::bus::native_client::{
    BoundedIncomingEvent, BoundedIncomingReceiver, ConnState, IncomingCommand,
    RegistrationRejectionKind, SupervisedClient,
};
use application::message::Once;
use application::native_actor::{Accepted, Completed, Reply, TaskSet, submit_replies};
use application::native_queue::{Admission, Flush, Outbox, SendError};
use application::presentation::native::{
    Event as SettingsEvent, Progress, Session, Ui, Worker, bridge,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Condvar, Mutex, RwLock},
    time::Duration,
};
use term_core::tabs::{Cleanup, CompletionNote, TabSet};
use term_core::terminal::Wake;

/// One routed delivery for the UI: an `app.describe` request to answer.
#[derive(Debug, Clone)]
pub struct Describe {
    pub id: u64,
    ticket: Once<u64>,
}

enum Effect {
    /// The UI's answer for one tracked describe request.
    Reply(u64, u8, Value),
    Quit,
}

/// What the shared handle carries: the one supervised client (replaced at
/// most once by the gen-0 name fallback), the last observed connection state
/// and the broker's refusal words, when any. `client` is `None` only in the
/// test sink, which owns no client at all; production always holds the real
/// current client.
struct Shared {
    client: Option<Arc<SupervisedClient>>,
    state: ConnState,
    refused: Option<String>,
}

/// The connection provenance the chrome labels: sampled on the UI thread from
/// the shared handle — no second connection, no extra worker. Independent of
/// the transient log lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionEvidence {
    pub state: ConnState,
    pub refused: Option<String>,
}

/// The UI-thread half of the adapter.
#[derive(Clone)]
pub struct Handle {
    tx: tokio::sync::mpsc::UnboundedSender<Effect>,
    done: Arc<(Mutex<bool>, Condvar)>,
    shared: Arc<RwLock<Shared>>,
}

impl Handle {
    /// Sample the settings generation from the ACTUAL shared client, not a
    /// probe connection: the UI reconciles against this value.
    pub fn settings_generation(&self) -> Option<u64> {
        let shared = self.shared.read().unwrap_or_else(|e| e.into_inner());
        shared
            .client
            .as_ref()
            .and_then(|client| settings::native::live_generation(client))
    }

    /// The name this window serves NOW — the base name, or the one gen-0
    /// `-<pid>` fallback after an explicit name-taken refusal. Describe
    /// replies report this actual name, never the original one.
    pub fn service_name(&self) -> String {
        self.shared
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .client
            .as_ref()
            .map_or_else(
                || "term".to_owned(),
                |client| client.service_name().to_owned(),
            )
    }

    /// The live connection provenance for the chrome label.
    pub fn connection(&self) -> ConnectionEvidence {
        let shared = self.shared.read().unwrap_or_else(|e| e.into_inner());
        ConnectionEvidence {
            state: shared.state,
            refused: shared.refused.clone(),
        }
    }

    pub fn reply(&self, describe: &Describe, rc: u8, value: Value) {
        if let Some(id) = describe.ticket.take() {
            let _ = self.tx.send(Effect::Reply(id, rc, value));
        }
    }
    pub fn quit(&self) {
        let _ = self.tx.send(Effect::Quit);
    }
    /// Wait for the worker's shutdown receipt. The done flag itself is
    /// authoritative: `Ok` only when the worker confirmed done (even on a
    /// deadline edge), `Err` when the budget expired with done still false.
    /// On `Err` the worker thread must then NOT be joined (a blocking
    /// resource job in the settings worker cannot be cancelled).
    pub fn wait_done(&self) -> Result<(), String> {
        let (lock, changed) = &*self.done;
        let state = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (state, _timeout) = changed
            .wait_timeout_while(state, Duration::from_secs(5), |done| !*done)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *state {
            Ok(())
        } else {
            Err("the Bus worker did not confirm its shutdown receipt; its thread is left unjoined (blocking resource I/O cannot be cancelled)".to_owned())
        }
    }
    #[cfg(test)]
    pub fn sink() -> Self {
        // A pure fixture sink: no client, no supervisor thread, no runtime.
        Self {
            tx: tokio::sync::mpsc::unbounded_channel().0,
            done: Arc::new((Mutex::new(true), Condvar::new())),
            shared: Arc::new(RwLock::new(Shared {
                client: None,
                state: ConnState::Disconnected,
                refused: None,
            })),
        }
    }
}

pub struct Started {
    pub handle: Handle,
    pub frames: application::frames::Handle,
    #[cfg(feature = "acceptance")]
    pub fixture_frames: Option<application::acceptance::frames::Endpoint>,
    pub ui: Ui<Content, LocalContext>,
    pub bootstrap: appearance::settings::Prepared,
    pub describes: tokio::sync::mpsc::Receiver<Describe>,
    /// The worker thread, stored so shutdown joins it instead of detaching it.
    pub worker: std::thread::JoinHandle<()>,
}

type Ready = std::sync::mpsc::Sender<
    Result<
        (
            Arc<RwLock<Shared>>,
            Ui<Content, LocalContext>,
            appearance::settings::Prepared,
        ),
        String,
    >,
>;

pub(crate) struct PreparationSeed {
    pub local: LocalContext,
    pub raster: term_core::raster::PreparedRaster,
    #[cfg(feature = "acceptance")]
    pub fixture: Option<crate::acceptance::Fixture>,
    #[cfg(all(test, feature = "acceptance"))]
    pub fixture_admission: Option<tokio::sync::mpsc::UnboundedSender<usize>>,
}

struct WorkerChannels {
    frames: application::frames::Handle,
    #[cfg(feature = "acceptance")]
    fixture_frames: Option<application::acceptance::frames::Endpoint>,
    describes: tokio::sync::mpsc::Sender<Describe>,
    effects: tokio::sync::mpsc::UnboundedReceiver<Effect>,
    ready: Ready,
    seed: PreparationSeed,
}

pub fn start(
    service: &'static str,
    url: String,
    tabs: Arc<Mutex<TabSet>>,
    cleanup: Cleanup,
    notify_rx: tokio::sync::mpsc::UnboundedReceiver<CompletionNote>,
    wake: Wake,
    seed: PreparationSeed,
) -> Result<Started, String> {
    let frames = application::frames::Handle::new();
    let worker_frames = frames.clone();
    #[cfg(feature = "acceptance")]
    let fixture_frames = seed
        .fixture
        .as_ref()
        .map(|_| application::acceptance::frames::Endpoint::new(frames.clone()));
    #[cfg(feature = "acceptance")]
    let worker_fixture_frames = fixture_frames.clone();
    let (describe_tx, describe_rx) = tokio::sync::mpsc::channel(PENDING_CAP);
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let (ready_send, ready_receive) = std::sync::mpsc::channel();
    let done = Arc::new((Mutex::new(false), Condvar::new()));
    let finished = done.clone();
    let thread = std::thread::Builder::new()
        .name(format!("{service}-bus"))
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            match runtime {
                Ok(runtime) => {
                    runtime.block_on(worker(
                        service,
                        url,
                        tabs,
                        cleanup,
                        notify_rx,
                        wake,
                        WorkerChannels {
                            frames: worker_frames,
                            #[cfg(feature = "acceptance")]
                            fixture_frames: worker_fixture_frames,
                            describes: describe_tx,
                            effects: rx,
                            ready: ready_send,
                            seed,
                        },
                    ));
                    // The bounded lane drain has ended; outstanding task
                    // cancellation may remain unconfirmed. Runtime teardown
                    // has a separate allowance capped at 100 ms.
                    runtime.shutdown_timeout(Duration::from_millis(100));
                }
                Err(error) => {
                    let _ = ready_send.send(Err(format!("Bus runtime: {error}")));
                }
            }
            let (lock, changed) = &*finished;
            *lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
            changed.notify_all();
        })
        .map_err(|e| e.to_string())?;
    let (shared, ui, bootstrap) = ready_receive
        .recv_timeout(Duration::from_secs(5))
        .map_err(|e| format!("Bus startup: {e}"))??;
    Ok(Started {
        handle: Handle { tx, done, shared },
        frames,
        #[cfg(feature = "acceptance")]
        fixture_frames,
        ui,
        bootstrap,
        describes: describe_rx,
        worker: thread,
    })
}

fn wake_ui(wake: &Wake, needed: bool) {
    if needed {
        wake();
    }
}

/// The number of pending `app.describe` requests the adapter tracks before
/// answering BUSY.
const PENDING_CAP: usize = 32;
/// The total in-flight reply/refusal tasks. Bounded on its own account: the
/// incoming lane's capacity never implies any bound on spawned tasks.
const OPERATIONS_CAP: usize = 8;
/// Starts when the frontend answer is ready, including retained-send queue
/// delay. UI/domain work is not given a new implicit two-second deadline.
const REPLY_BUDGET: Duration = Duration::from_secs(2);
/// One shared shutdown budget for the whole lane, as everywhere else.
const SHUTDOWN_BUDGET: Duration = Duration::from_secs(2);

async fn worker(
    service: &'static str,
    url: String,
    tabs: Arc<Mutex<TabSet>>,
    cleanup: Cleanup,
    notify_rx: tokio::sync::mpsc::UnboundedReceiver<CompletionNote>,
    wake: Wake,
    channels: WorkerChannels,
) {
    let WorkerChannels {
        frames,
        #[cfg(feature = "acceptance")]
        fixture_frames,
        describes,
        mut effects,
        ready,
        seed,
    } = channels;
    let PreparationSeed {
        local: initial,
        raster,
        #[cfg(feature = "acceptance")]
        mut fixture,
        #[cfg(all(test, feature = "acceptance"))]
        fixture_admission,
    } = seed;
    // The shared session binding, following the existing diagnostics: a
    // binding failure is reported and startup stops — a terminal never
    // fabricates an identity to keep settings alive.
    let consumer = match settings::session::binding()
        .and_then(|binding| settings::consumer::Consumer::for_app(binding, "term"))
    {
        Ok(consumer) => consumer,
        Err(error) => {
            let _ = ready.send(Err(error.message));
            return;
        }
    };
    // Bootstrap readiness precedes resource registration: the UI half owns a
    // prepared presentation before any font installation runs on the worker.
    let bootstrap = match appearance::settings::bootstrap() {
        Ok(bootstrap) => bootstrap,
        Err(error) => {
            let _ = ready.send(Err(error.message));
            return;
        }
    };
    // One client for both lanes, started without waiting for a broker:
    // transport failures retry under the supervisor; offline leaves graphics
    // and native startup working. The started client persists through Fatal —
    // only the one gen-0 name-taken fallback below replaces it.
    let mut name = service.to_string();
    let mut client = Arc::new(
        SupervisedClient::connect_options(&name, &url)
            .fatal_on_registration_rejection(true)
            .bounded_incoming(16)
            .start(),
    );
    let mut incoming: Option<BoundedIncomingReceiver> = client.incoming_bounded();
    let mut connection = client.subscribe_state();
    let shared = Arc::new(RwLock::new(Shared {
        client: Some(Arc::clone(&client)),
        state: ConnState::Connecting,
        refused: None,
    }));
    // A missing AppDirs root keeps the settings lane on the shared offline
    // worker; the bridge records that missing cache configuration as a
    // diagnostic in the session's cache evidence, so it is visible rather
    // than silent.
    #[cfg(feature = "acceptance")]
    let prepare_hook = fixture.as_ref().map(|fixture| fixture.hook.clone());
    let settings_worker = Worker::contextual(move |appearance, snapshot, local: &LocalContext| {
        #[cfg(feature = "acceptance")]
        if let Some(hook) = &prepare_hook {
            use application::acceptance::barrier::Observation;
            let fault = |message| {
                settings::Diagnostic::new("fixture_prepare_cancelled", "terminal.prepare", message)
            };
            let observation = Observation::try_new(format!(
                "revision={} scale={} zoom={}",
                snapshot.revision.0, local.scale, local.zoom_steps
            ))
            .map_err(|error| fault(format!("{error:?}")))?;
            if let Some(permit) = hook
                .reach("terminal.prepare", observation)
                .map_err(|error| fault(format!("{error:?}")))?
            {
                permit
                    .wait_blocking()
                    .map_err(|error| fault(format!("{error:?}")))?;
            }
        }
        crate::presentation::prepare(appearance, snapshot, local, &raster)
    });
    let settings_worker = match config::AppDirs::resolve("term") {
        Some(dirs) => settings_worker.with_cache_directory(dirs.cache().join("settings")),
        None => settings_worker,
    };
    let (ui, mut lane) = bridge(Session::with_context(consumer, initial), settings_worker);
    wake_ui(&wake, lane.connect(Arc::clone(&client)));
    if ready
        .send(Ok((Arc::clone(&shared), ui, bootstrap)))
        .is_err()
    {
        let _ = tokio::time::timeout(Duration::from_secs(2), client.close()).await;
        return;
    }
    // Font files are installed on the existing worker, after UI readiness and
    // before the Lane may prepare a checked presentation.
    if let Err(error) = appearance::fonts::register_installed() {
        eprintln!("term: static assets: {error}");
    }
    // The verb lane runs on a bounded queue fed here; it spawns once the
    // client first connects (commands simply queue until then), so the gen-0
    // fallback always resolves before the serve task takes the notify channel.
    let (commands_tx, commands_rx) = tokio::sync::mpsc::channel::<BoundedIncomingEvent>(64);
    let mut commands_rx = Some(commands_rx);
    let mut notify_rx = Some(notify_rx);
    let mut serve_task: Option<tokio::task::JoinHandle<usize>> = None;
    let mut served = false;
    // A tracked describe keeps the client that received it, so its reply is
    // sent on the SAME connection whatever happened to the shared handle.
    let mut pending: HashMap<u64, Accepted> = HashMap::new();
    let mut next_id = 0u64;
    let mut lifecycle = None;
    let mut incoming_open = true;
    let mut connection_open = true;
    let mut pending_overflow: Option<BoundedIncomingEvent> = None;
    // Bounded reply tasks: the permit pool caps every in-flight respond, so a
    // BUSY flood cannot allocate unbounded tasks. Accepted replies take
    // priority — when the pool is exhausted they are RETAINED here and
    // started as capacity frees; refusals are shed with a diagnostic instead.
    let admitted = Admission::new(PENDING_CAP);
    let refusals = Admission::new(4);
    let mut retained_replies = Outbox::<Reply, 0>::new(PENDING_CAP);
    let mut deliveries = Outbox::<Describe, 0>::new(PENDING_CAP);
    let mut operations = TaskSet::<Result<(), String>>::new(OPERATIONS_CAP);
    // Two independently bounded waits cannot occupy ordinary control/reply
    // slots. Credits remain in the JoinSet result until the actor reaps them.
    let mut fixture_waits = TaskSet::<Result<(), String>>::new(2);
    loop {
        // Accepted replies retained while the task cap was exhausted start
        // the moment capacity frees; an unused permit returns immediately.
        submit_replies(&mut retained_replies, &mut operations);
        if deliveries.flush_with(|describe| match describes.try_send(describe) {
            Ok(()) => {
                wake_ui(&wake, true);
                Ok(())
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(value)) => Err(SendError::Full(value)),
            Err(tokio::sync::mpsc::error::TrySendError::Closed(value)) => {
                Err(SendError::Closed(value))
            }
        }) == Flush::Closed
        {
            eprintln!(
                "{service} frontend closed with {} accepted requests",
                pending.len()
            );
            break;
        }
        // Retry a retained overflow into the verb queue the moment capacity
        // may have returned; the core serve loop consumes it and logs it.
        if let Retained::Closed = deliver_retained(&commands_tx, &mut pending_overflow) {
            // The serve loop is gone: the marker can never be delivered. Drop
            // it once, saying so, and publish the loss into the settings lane.
            eprintln!("{service} Bus verb lane closed; a retained overflow marker is dropped");
            wake_ui(&wake, lane.publish(SettingsEvent::Wake));
        }
        // Sample once before waiting: fast registration may already be done.
        let now = *connection.borrow_and_update();
        let generation = client.connection_generation();
        if lifecycle != Some((now, generation)) {
            lifecycle = Some((now, generation));
            frames.set_live_generation(settings::native::live_generation(&client));
            #[cfg(feature = "acceptance")]
            if let Some(fixture) = &fixture {
                fixture
                    .controller
                    .close(application::acceptance::barrier::ClosedReason::LostGeneration);
            }
            // EVERY lifecycle transition publishes into the settings lane:
            // edge-triggered consumers and the UI reconcile on one signal.
            {
                // The shared provenance the chrome labels, kept alongside the
                // log lines — not instead of them.
                let mut shared = shared.write().unwrap_or_else(|e| e.into_inner());
                shared.state = now;
                match now {
                    ConnState::Connected => shared.refused = None,
                    ConnState::Fatal => {
                        shared.refused = client.registration_rejection().map(|r| r.message);
                    }
                    _ => {}
                }
            }
            // Publish after the provenance is visible to the awakened UI.
            wake_ui(&wake, lane.publish(SettingsEvent::Wake));
            match now {
                ConnState::Connected => {
                    if !served {
                        served = true;
                        let client = Arc::clone(&client);
                        let (tabs, cleanup) = (tabs.clone(), cleanup.clone());
                        let notify = notify_rx.take().expect("one serve spawn");
                        let mut incoming = commands_rx.take().expect("one serve spawn");
                        // The ACTUAL served name — the base, or the gen-0
                        // fallback — never the original one.
                        let name = name.clone();
                        serve_task = Some(tokio::spawn(async move {
                            let mut notify = notify;
                            // Caller-owned completion-note set: aborting this
                            // task cancels its JoinSet with it, and the outer
                            // shutdown deadline bounds the whole drain.
                            let mut notifications = tokio::task::JoinSet::new();
                            term_core::bus::serve_registered(
                                &name,
                                &tabs,
                                &cleanup,
                                &mut notify,
                                &mut notifications,
                                &mut incoming,
                                &client,
                            )
                            .await
                        }));
                    }
                }
                ConnState::Fatal => {
                    // One initial fallback: an explicit name-taken refusal of
                    // the BASE name before anything connected. Close the first
                    // client BEFORE replacing the shared handle, so no sampler
                    // ever observes two live names.
                    let reason = client.registration_rejection();
                    let name_taken = reason
                        .as_ref()
                        .is_some_and(|r| r.kind() == RegistrationRejectionKind::NameTaken);
                    let fallback = term_core::bus::pid_fallback(
                        service,
                        &name,
                        name_taken,
                        client.connection_generation(),
                    );
                    if let Some(fallback) = fallback {
                        if tokio::time::timeout(Duration::from_secs(2), client.close())
                            .await
                            .is_err()
                        {
                            eprintln!(
                                "{service} old supervisor did not retire; refusing name fallback"
                            );
                            break;
                        }
                        // Replies from the old connection can never be sent:
                        // fence both the pending describes and any retained
                        // accepted replies on the replaced generation.
                        for (_, accepted) in pending.drain() {
                            accepted.retire().finish();
                        }
                        for reply in retained_replies.drain() {
                            reply.retire().finish();
                        }
                        for delivery in deliveries.drain() {
                            let _ = delivery.ticket.take();
                        }
                        name = fallback;
                        client = Arc::new(
                            SupervisedClient::connect_options(&name, &url)
                                .fatal_on_registration_rejection(true)
                                .bounded_incoming(16)
                                .start(),
                        );
                        {
                            let mut shared = shared.write().unwrap_or_else(|e| e.into_inner());
                            shared.client = Some(Arc::clone(&client));
                            shared.state = ConnState::Connecting;
                            shared.refused = None;
                        }
                        wake_ui(&wake, lane.connect(Arc::clone(&client)));
                        incoming = client.incoming_bounded();
                        incoming_open = true;
                        connection = client.subscribe_state();
                        lifecycle = None; // re-observe the fresh client
                        continue;
                    }
                    eprintln!(
                        "{service} Bus unavailable: {}",
                        reason.map_or_else(|| "connection stopped".into(), |r| r.message)
                    );
                }
                ConnState::Connecting | ConnState::Disconnected | ConnState::ShuttingDown => {}
            }
            // Stale describes can never be answered: keep only commands whose
            // client is still THIS one, at the connection generation the
            // command arrived on. A reply may never ride current-generation
            // evidence back to an old connection.
            let stale: Vec<_> = pending
                .iter()
                .filter_map(|(id, accepted)| (!accepted.is_current(&client)).then_some(*id))
                .collect();
            for id in stale {
                if let Some(accepted) = pending.remove(&id) {
                    eprintln!("{service} retired stale describe {id}");
                    accepted.retire().finish();
                }
            }
            let queued: Vec<_> = deliveries.drain().collect();
            for delivery in queued {
                if pending.contains_key(&delivery.id) {
                    assert!(deliveries.push(delivery).is_ok());
                } else {
                    let _ = delivery.ticket.take();
                }
            }
        }
        // Fairness under an incoming flood is ordered, not left to chance:
        // completed reply tasks (which free the operation cap), the settings
        // lane and UI effects — including Quit — are polled before the
        // command lane, so accepted replies and quit progress however ready
        // `incoming` is. Each earlier branch is only transiently ready, so
        // the command lane cannot starve either.
        tokio::select! {
            biased;
            result = fixture_waits.join_next(), if !fixture_waits.is_empty() => {
                match result {
                    Some(Ok(Completed {permit,value:result})) => {
                        if let Err(error) = result { eprintln!("{service} fixture wait: {error}"); }
                        permit.finish();
                    }
                    Some(Err(error)) => eprintln!("{service} fixture wait: {error}"),
                    None => {}
                }
            }
            _ = async {
                #[cfg(feature = "acceptance")]
                if let Some(fixture) = fixture.as_mut() { fixture.controller.drive().await; return; }
                std::future::pending::<()>().await;
            } => {},
            result = operations.join_next(), if !operations.is_empty() => {
                match result {
                    Some(Ok(Completed {permit,value:result})) => {
                        if let Err(error) = result { eprintln!("{service} Bus reply: {error}"); }
                        permit.finish();
                    }
                    Some(Err(error)) => eprintln!("{service} Bus reply: {error}"),
                    None => {}
                }
            }
            progress = lane.drive() => match progress {
                Progress::Wake => wake_ui(&wake, true),
                Progress::UiClosed => break,
                Progress::Updated => {}
            },
            effect = effects.recv() => {
                let Some(effect) = effect else { break };
                match effect {
                    Effect::Reply(id, rc, value) => {
                        // Tracked send on the client that received the
                        // command; the lane is never blocked on a reply.
                        // Accepted replies take priority over refusals: when
                        // the task cap is exhausted they are retained, never
                        // dropped, and start as capacity frees.
                        if let Some(accepted) = pending.remove(&id) {
                            let reply = accepted.reply(rc, value.to_string(), std::time::Instant::now() + REPLY_BUDGET);
                            if let Err(reply) = retained_replies.push(reply) {
                                // One credit covers pending + retained + tasks, so
                                // this cannot be full after removing that pending.
                                eprintln!("{service} accepted reply retention invariant failed");
                                reply.retire().finish();
                            }
                        }
                    }
                    Effect::Quit => break,
                }
            }
            changed = connection.changed(), if connection_open => {
                if changed.is_err() { connection_open = false; }
            }
            reserved = describes.reserve(), if !deliveries.is_empty() => {
                if reserved.is_err() { break; }
                // Capacity is only a wakeup. The value stays in the outbox
                // until the synchronous flush at the next loop boundary.
            }
            event = async { incoming.as_mut().expect("guarded incoming").recv().await },
                if incoming.is_some() && incoming_open => {
                let command = match event {
                    Some(BoundedIncomingEvent::Command(command)) => command,
                    Some(BoundedIncomingEvent::Overflow { dropped }) => {
                        // Overflow reaches BOTH lanes: the settings lane
                        // invalidates delivery-derived state once per received
                        // overflow, and the verb lane logs the drop. Successive
                        // losses aggregate while the bounded queue is full —
                        // retained, never dropped a second time, saturating.
                        wake_ui(&wake, lane.publish(SettingsEvent::Lost));
                        retain_overflow(&mut pending_overflow, dropped);
                        continue;
                    }
                    None => {
                        // The lane ends when the supervised client stops for
                        // good — a fatal registration or the shutdown close.
                        // Transport bounces reconnect on the same lane without
                        // closing it. The settings lane and any offline
                        // preparation stay alive; only Quit or a closed UI
                        // ends this loop.
                        incoming_open = false;
                        wake_ui(&wake, lane.publish(SettingsEvent::Wake));
                        continue;
                    }
                };
                if let Some(needed) = lane.delivery(&command) {
                    wake_ui(&wake, needed);
                    continue;
                }
                #[cfg(feature = "acceptance")]
                if let Some(fixture) = &fixture
                    && let Some(class) = application::acceptance::classify(&command.command) {
                        frames.set_live_generation(settings::native::live_generation(&client));
                        let waiting = class == application::acceptance::Class::Wait;
                        let tasks = if waiting { &mut fixture_waits } else { &mut operations };
                        let full = tasks.is_full();
                        let permit = if full { None } else { admitted.try_acquire() };
                        let Some(permit) = permit else {
                            try_spawn(service,&refusals,&mut operations,Arc::clone(&client),command,10,
                                "{\"error_code\":\"BUSY\",\"message\":\"acceptance capacity exhausted\"}".to_string());
                            continue;
                        };
                        let accepted = Accepted::new(Arc::clone(&client), command, permit, std::time::Instant::now());
                        let result = tasks.try_spawn_with(accepted, |accepted| accepted.into_task(|client, command, _| {
                            let future = application::acceptance::track_result(client,command,
                                &fixture.describe,&fixture.inspector,&fixture.controller,fixture_frames.as_ref()).expect("exact fixture verb");
                            async move {future.await.map_err(|error| format!("acceptance: {error:?}"))}
                        }));
                        if let Err(accepted) = result {
                            accepted.retire().finish();
                            eprintln!("{service} fixture task capacity invariant failed");
                        }
                        #[cfg(all(test,feature = "acceptance"))]
                        if waiting && let Some(probe) = &fixture_admission { let _ = probe.send(fixture_waits.len()); }
                        continue;
                }
                if command.command == "app.describe" {
                    if client.state() != ConnState::Connected
                        || client.connection_generation() != command.generation {
                        eprintln!("{service} rejected stale queued describe");
                        continue;
                    }
                    // The frontend answers: a UI request that reconciles live
                    // settings evidence first, tracked here by id, bounded so
                    // a slow UI cannot grow the map without limit. While the
                    // reply path is saturated (retained accepted replies), new
                    // describes are refused instead of queueing further.
                    let Some(permit) = admitted.try_acquire() else {
                        let client = Arc::clone(&client);
                        try_spawn(service, &refusals, &mut operations, client, command, 10,
                            "{\"error_code\":\"BUSY\",\"message\":\"too many pending commands\"}".to_string());
                        continue;
                    };
                    let Some(id) = next_id.checked_add(1) else {
                        permit.finish();
                        try_spawn(service, &refusals, &mut operations, Arc::clone(&client), command, 10,
                            "{\"error_code\":\"EXHAUSTED\"}".into());
                        continue;
                    };
                    next_id = id;
                    if describes.is_closed() {
                        // The UI is gone: no answer can ever come. Refuse once,
                        // tracked on the originating client.
                        let client = Arc::clone(&client);
                        permit.finish();
                        try_spawn(service, &refusals, &mut operations, client, command, 10,
                            "{\"error_code\":\"CLOSED\",\"message\":\"no frontend to answer app.describe\"}".to_string());
                        continue;
                    }
                    pending.insert(id, Accepted::new(Arc::clone(&client), command, permit, std::time::Instant::now()));
                    let delivery = Describe {id, ticket: Once::new(id)};
                    if let Err(delivery) = deliveries.push(delivery) {
                        let _ = delivery.ticket.take();
                        if let Some(accepted) = pending.remove(&id) { accepted.retire().finish(); }
                        eprintln!("{service} frontend retention invariant failed");
                        break;
                    }
                    wake_ui(&wake, true);
                    continue;
                }
                // Everything else is the verb lane's: bounded queue, BUSY
                // instead of an unbounded buffer.
                match commands_tx.try_send(BoundedIncomingEvent::Command(command)) {
                    Ok(()) => {}
                    Err(tokio::sync::mpsc::error::TrySendError::Full(event)) => match event {
                        BoundedIncomingEvent::Command(command) => {
                            let client = Arc::clone(&client);
                            try_spawn(service, &refusals, &mut operations, client, command, 10,
                                "{\"error_code\":\"BUSY\",\"message\":\"verb queue full\"}".to_string());
                        }
                        // A retained overflow marker can lose the same
                        // capacity race: keep it instead of answering BUSY for
                        // a command this arm never held.
                        BoundedIncomingEvent::Overflow { dropped } => {
                            retain_overflow(&mut pending_overflow, dropped);
                        }
                    },
                    Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {}
                }
            }
            reserved = commands_tx.reserve(), if pending_overflow.is_some() => {
                if reserved.is_err() {
                    // The serve loop is gone; drop the marker once instead of
                    // spinning on an instantly-ready closed reserve.
                    pending_overflow = None;
                    eprintln!("{service} Bus verb lane closed; a retained overflow marker is dropped");
                    wake_ui(&wake, lane.publish(SettingsEvent::Wake));
                }
            }
        }
    }
    frames.close();
    #[cfg(feature = "acceptance")]
    if let Some(fixture) = &fixture {
        fixture.close(application::acceptance::barrier::ClosedReason::Shutdown);
    }
    // tabs.close -> global Bus finish: the serve task has drained the final
    // reap's completion notes by the time the TabSet emptied; wait for it,
    // the tracked replies, the cache flush and the client close all under ONE
    // shared 2 s deadline. On expiry request cancellation and report any
    // unreaped work; abort alone does not prove the future was destroyed.
    let deadline = std::time::Instant::now() + SHUTDOWN_BUDGET;
    let deadline_at = tokio::time::Instant::from_std(deadline);
    let mut faults = Vec::new();
    while !fixture_waits.is_empty() {
        if std::time::Instant::now() >= deadline {
            faults.push("fixture wait drain timed out".to_owned());
            cancel_tasks("fixture wait", &mut fixture_waits, &mut faults);
            break;
        }
        match tokio::time::timeout_at(deadline_at, fixture_waits.join_next()).await {
            Ok(Some(Ok(Completed {
                permit,
                value: result,
            }))) => {
                if let Err(error) = result {
                    faults.push(error);
                }
                permit.finish();
            }
            Ok(Some(Err(error))) => faults.push(format!("fixture wait: {error}")),
            Ok(None) => break,
            Err(_) => {
                faults.push("fixture wait drain timed out".to_owned());
                cancel_tasks("fixture wait", &mut fixture_waits, &mut faults);
                break;
            }
        }
    }
    // Accepted replies retained while the task cap was exhausted stay fenced
    // on their originating client and generation: they must be sent under the
    // shared deadline or be counted as undelivered — never silently dropped.
    for (_, accepted) in pending.drain() {
        faults.push("accepted describe retired without frontend reply".into());
        accepted.retire().finish();
    }
    for delivery in deliveries.drain() {
        let _ = delivery.ticket.take();
    }
    if let Some(mut task) = serve_task.take() {
        tokio::select! {
            result = &mut task => {
                if let Err(error) = result {
                    faults.push(format!("term Bus serve: {error}"));
                }
            }
            _ = tokio::time::sleep_until(deadline_at) => {
                task.abort();
                faults.push("term Bus serve drain timed out".into());
                // Drop the aborted handle without an unbounded post-deadline
                // await. The bounded owning runtime handles cancellation.
                faults.push("term Bus serve cancellation requested; completion unconfirmed".into());
            }
        }
    }
    while !operations.is_empty() || !retained_replies.is_empty() {
        if std::time::Instant::now() >= deadline {
            cancel_tasks("Bus reply", &mut operations, &mut faults);
            let unsent = retained_replies.len();
            for reply in retained_replies.drain() {
                reply.retire().finish();
            }
            faults.push(format!(
                "term Bus reply drain timed out with {unsent} retained unsent replies"
            ));
            break;
        }
        submit_replies(&mut retained_replies, &mut operations);
        match tokio::time::timeout_at(deadline_at, operations.join_next()).await {
            Ok(Some(Ok(Completed {
                permit,
                value: result,
            }))) => {
                if let Err(error) = result {
                    faults.push(error);
                }
                permit.finish();
            }
            Ok(Some(Err(error))) => faults.push(format!("term Bus reply: {error}")),
            Ok(None) => break,
            Err(_) => {
                let outstanding = operations.len() + retained_replies.len();
                cancel_tasks("Bus reply", &mut operations, &mut faults);
                for reply in retained_replies.drain() {
                    reply.retire().finish();
                }
                if outstanding > 0 {
                    faults.push(format!(
                        "term Bus reply drain timed out with {outstanding} outstanding replies; delivery unconfirmed"
                    ));
                }
                break;
            }
        }
    }
    drop(fixture_waits);
    drop(operations);
    if let Err(error) = lane.flush_cache(deadline).await {
        faults.push(format!("settings cache: {}: {}", error.code, error.message));
    }
    if tokio::time::timeout_at(deadline_at, client.close())
        .await
        .is_err()
    {
        faults.push("Bus close timed out".into());
    }
    let accepted = admitted.counts();
    let refused = refusals.counts();
    eprintln!(
        "TERM_SHUTDOWN {}",
        json!({"faults": faults,
        "describes": {"active":accepted.active,"finished":accepted.finished,"abandoned":accepted.abandoned},
        "refusals": {"active":refused.active,"finished":refused.finished,"abandoned":refused.abandoned}})
    );
}

/// Start one bounded reply task if a permit remains. A refusal under
/// saturation is shed with a diagnostic — the caller times out — while an
/// accepted reply is retained by the caller instead, never dropped here.
fn try_spawn(
    service: &'static str,
    permits: &Admission,
    operations: &mut TaskSet<Result<(), String>>,
    client: Arc<SupervisedClient>,
    command: IncomingCommand,
    rc: u8,
    body: String,
) {
    if operations.is_full() {
        eprintln!("{service} Bus refusal send capacity exhausted (caller times out)");
        return;
    }
    let Some(permit) = permits.try_acquire() else {
        eprintln!("{service} Bus reply capacity exhausted; dropping a refusal (caller times out)");
        return;
    };
    let admitted_at = std::time::Instant::now();
    let reply = Accepted::new(client, command, permit, admitted_at).reply(
        rc,
        body,
        admitted_at + REPLY_BUDGET,
    );
    if let Err(reply) = operations.try_spawn_with(reply, Reply::into_task) {
        reply.retire().finish();
        eprintln!("{service} refusal task capacity invariant failed");
    }
}

/// This finite synchronous drain cannot extend the shared shutdown deadline.
/// Unreaped aborted tasks stay unconfirmed, rather than being counted finished.
fn cancel_tasks(label: &str, tasks: &mut TaskSet<Result<(), String>>, faults: &mut Vec<String>) {
    let report = std::mem::replace(tasks, TaskSet::new(0)).abort_and_report();
    for result in report.ready {
        match result {
            Ok(Completed { permit, value }) => {
                if let Err(error) = value {
                    faults.push(error);
                }
                permit.finish();
            }
            Err(error) if error.is_cancelled() => {}
            Err(error) => faults.push(format!("{label}: {error}")),
        }
    }
    if report.unconfirmed > 0 {
        faults.push(format!(
            "{label} cancellation requested with {} unreaped tasks",
            report.unconfirmed
        ));
    }
}

/// Successive overflow markers aggregate while the bounded verb queue is
/// full: the retained count is the saturating total, never replaced.
fn retain_overflow(pending: &mut Option<BoundedIncomingEvent>, dropped: u64) {
    let total = match pending.take() {
        Some(BoundedIncomingEvent::Overflow { dropped: prior }) => dropped.saturating_add(prior),
        _ => dropped,
    };
    *pending = Some(BoundedIncomingEvent::Overflow { dropped: total });
}

enum Retained {
    Delivered,
    Full,
    Closed,
}

/// Try to deliver a retained overflow into the verb queue. Full retains it
/// for the next capacity; Closed drops it (the serve loop is gone).
fn deliver_retained(
    commands: &tokio::sync::mpsc::Sender<BoundedIncomingEvent>,
    pending: &mut Option<BoundedIncomingEvent>,
) -> Retained {
    let Some(event) = pending.take() else {
        return Retained::Delivered;
    };
    match commands.try_send(event) {
        Ok(()) => Retained::Delivered,
        Err(tokio::sync::mpsc::error::TrySendError::Full(event)) => {
            *pending = Some(event);
            Retained::Full
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => Retained::Closed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    async fn connected(name: &str, url: &str) -> Arc<SupervisedClient> {
        let client = Arc::new(
            SupervisedClient::connect_options(name, url)
                .fatal_on_registration_rejection(true)
                .bounded_incoming(4)
                .start(),
        );
        let mut state = client.subscribe_state();
        tokio::time::timeout(Duration::from_secs(5), async {
            while *state.borrow_and_update() != ConnState::Connected {
                state.changed().await.unwrap();
            }
        })
        .await
        .expect("actual broker registration");
        client
    }

    async fn incoming_command(incoming: &mut BoundedIncomingReceiver) -> IncomingCommand {
        match tokio::time::timeout(Duration::from_secs(5), incoming.recv())
            .await
            .unwrap()
        {
            Some(BoundedIncomingEvent::Command(command)) => command,
            other => panic!("real command expected: {other:?}"),
        }
    }

    #[test]
    fn retained_expired_reply_sends_nothing_and_holds_credit_until_reaped() {
        let broker = term_test_broker::Broker::start();
        runtime().block_on(async {
            let client = connected("actor-reply", &broker.url).await;
            let mut incoming = client.incoming_bounded().unwrap();
            let caller = Arc::new(
                ::bus::native_client::NodedClient::connect_anonymous(&broker.url)
                    .await
                    .unwrap(),
            );
            let calling = caller.clone();
            let call = tokio::spawn(async move {
                calling.call("actor-reply", "app.describe", json!({})).await
            });
            let command = incoming_command(&mut incoming).await;
            let original = (
                command.generation,
                command.from.clone(),
                command.command.clone(),
                command.id.clone(),
            );
            let admission = Admission::new(2);
            let mut tasks = TaskSet::new(1);
            let (release, held) = tokio::sync::oneshot::channel();
            assert!(
                tasks
                    .try_spawn_with(admission.try_acquire().unwrap(), |permit| (
                        permit,
                        async move {
                            held.await.unwrap();
                            Ok(())
                        }
                    ))
                    .is_ok()
            );
            let admitted_at = std::time::Instant::now();
            let deadline = admitted_at + Duration::from_millis(20);
            let accepted = Accepted::new(
                client.clone(),
                command,
                admission.try_acquire().unwrap(),
                admitted_at,
            );
            let mut retained = Outbox::<Reply, 0>::new(1);
            assert!(
                retained
                    .push(accepted.reply(0, json!({"expired":true}).to_string(), deadline))
                    .is_ok()
            );
            submit_replies(&mut retained, &mut tasks);
            assert_eq!(retained.len(), 1);
            tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
            release.send(()).unwrap();
            let blocker = tasks.join_next().await.unwrap().unwrap();
            blocker.value.unwrap();
            blocker.permit.finish();
            submit_replies(&mut retained, &mut tasks);
            assert!(retained.is_empty());
            tokio::task::yield_now().await;
            assert!(tasks.is_full());
            assert_eq!(admission.counts().active, 1);
            let completed = tasks.join_next().await.unwrap().unwrap();
            assert_eq!(completed.value.unwrap_err(), "Bus reply timed out");
            assert_eq!(admission.counts().active, 1);
            completed.permit.finish();
            // If the expired helper sent anything, that first response would
            // have won. Reply now using the exact saved real wire identity.
            client
                .respond_parts(
                    original.0,
                    &original.1,
                    &original.2,
                    original.3.as_deref(),
                    0,
                    &json!({"sentinel":"after-expiry"}).to_string(),
                )
                .await
                .unwrap();
            let response = tokio::time::timeout(Duration::from_secs(5), call)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(response, json!({"sentinel":"after-expiry"}));
            assert_eq!(admission.counts().finished, 2);
            client.close().await;
            caller.close().await;
        });
    }

    #[test]
    fn reused_generation_never_substitutes_a_reply_supervisor() {
        let broker = term_test_broker::Broker::start();
        runtime().block_on(async {
            let a = connected("actor-origin", &broker.url).await;
            let b = connected("actor-other", &broker.url).await;
            assert_eq!(a.connection_generation(), 1);
            assert_eq!(b.connection_generation(), 1);
            let mut incoming = a.incoming_bounded().unwrap();
            let caller = Arc::new(
                ::bus::native_client::NodedClient::connect_anonymous(&broker.url)
                    .await
                    .unwrap(),
            );
            let calling = caller.clone();
            let call = tokio::spawn(async move {
                calling
                    .call("actor-origin", "app.describe", json!({}))
                    .await
            });
            let command = incoming_command(&mut incoming).await;
            let admission = Admission::new(1);
            let accepted = Accepted::new(
                a.clone(),
                command,
                admission.try_acquire().unwrap(),
                std::time::Instant::now(),
            );
            assert!(accepted.is_current(&a));
            assert!(
                !accepted.is_current(&b),
                "generation equality is insufficient"
            );
            let (permit, reply) = accepted
                .reply(
                    0,
                    json!({"origin":"a"}).to_string(),
                    std::time::Instant::now() + Duration::from_secs(5),
                )
                .into_task();
            reply.await.unwrap();
            permit.finish();
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(5), call)
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap(),
                json!({"origin":"a"})
            );

            let calling = caller.clone();
            let old_call = tokio::spawn(async move {
                calling
                    .call("actor-origin", "app.describe", json!({}))
                    .await
            });
            let command = incoming_command(&mut incoming).await;
            let old = Accepted::new(
                a.clone(),
                command,
                admission.try_acquire().unwrap(),
                std::time::Instant::now(),
            );
            a.close().await;
            let replacement = connected("actor-origin", &broker.url).await;
            assert_eq!(replacement.connection_generation(), 1);
            assert!(!old.is_current(&replacement));
            let mut fresh_incoming = replacement.incoming_bounded().unwrap();
            let calling = caller.clone();
            let fresh_call = tokio::spawn(async move {
                calling
                    .call("actor-origin", "app.describe", json!({}))
                    .await
            });
            let fresh_command = incoming_command(&mut fresh_incoming).await;
            let (permit, reply) = old
                .reply(
                    0,
                    json!({"origin":"old"}).to_string(),
                    std::time::Instant::now() + Duration::from_secs(5),
                )
                .into_task();
            assert!(
                reply.await.unwrap_err().starts_with("Bus reply:"),
                "old supervisor rejects the send"
            );
            permit.finish();
            replacement
                .respond(
                    &fresh_command,
                    0,
                    &json!({"origin":"replacement"}).to_string(),
                )
                .await
                .unwrap();
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(5), fresh_call)
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap(),
                json!({"origin":"replacement"})
            );
            old_call.abort();
            let _ = old_call.await;
            replacement.close().await;
            b.close().await;
            caller.close().await;
        });
    }

    /// Real ABP actor and actual blocking preparation hook. The artificial
    /// runtime target only holds a frame wait; this is not presentation proof.
    #[cfg(feature = "acceptance")]
    #[test]
    fn fixture_waits_leave_release_and_shutdown_control_headroom() {
        use application::acceptance::{Launch, frames::Target};
        let broker = term_test_broker::Broker::start();
        let settings = term_core::config::Settings {
            config: term_core::config::Config::default(),
            term: "xterm-256color",
        };
        let tabs = Arc::new(Mutex::new(TabSet::starting(settings)));
        let (cleanup, reaper) = term_core::tabs::Cleanup::start().unwrap();
        let (_notes, notify_rx) = tokio::sync::mpsc::unbounded_channel();
        let (wake_tx, mut wake_rx) = tokio::sync::mpsc::unbounded_channel();
        let wake: Wake = Arc::new(move || {
            let _ = wake_tx.send(());
        });
        let (fixture, _inspect_task) = crate::acceptance::setup_launch(Launch {
            run: "actor-owned".into(),
            instance: 51,
        })
        .unwrap();
        let initial = LocalContext::new(1.0, term_core::config::Cursor::Underline).unwrap();
        let (admission_tx, mut admission_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut started = start(
            "term",
            broker.url.clone(),
            tabs.clone(),
            cleanup.clone(),
            notify_rx,
            wake,
            PreparationSeed {
                local: initial,
                raster: term_core::raster::Raster::for_test(1.0, 13.0, initial.cursor)
                    .unwrap()
                    .prepared_snapshot(),
                fixture: Some(fixture),
                fixture_admission: Some(admission_tx),
            },
        )
        .unwrap();
        runtime().block_on(async {
            let caller = Arc::new(::bus::native_client::NodedClient::connect_anonymous(&broker.url).await.unwrap());
            tokio::time::timeout(Duration::from_secs(5),async {
                loop {
                    let generation = started.handle.settings_generation();
                    started.ui.reconcile(generation);
                    started.ui.drain_with(|| started.handle.settings_generation(), |_| {});
                    if generation.is_some() && started.ui.preparation_evidence().current {break;}
                    assert!(started.ui.preparation_evidence().fault.is_none(),"{:?}",started.ui.preparation_evidence().fault);
                    wake_rx.recv().await.expect("native worker wake");
                }
            }).await.expect("native contextual bootstrap");
            let identity = json!({"run":"actor-owned","instance":51});
            let window = application::iced::window::Id::unique();
            let stamp = started.ui.session().frame_stamp().unwrap();
            started.fixture_frames.as_ref().unwrap().publish(Target {window,stamp:Some(stamp)}).unwrap();
            let arm = caller.call("term","app.acceptance.barrier.arm",json!({"run":"actor-owned","instance":51,"point":"terminal.prepare","token":"held"})).await.unwrap();
            assert_eq!(arm["state"],"armed");
            let reference = json!({"run":"actor-owned","instance":51,"token":"held","sequence":arm["sequence"]});
            let barrier_wait = {
                let caller = caller.clone(); let reference = reference.clone();
                tokio::spawn(async move {caller.call("term","app.acceptance.barrier.wait",reference).await})
            };
            let frame_wait = {
                let caller = caller.clone();
                tokio::spawn(async move {caller.call("term","app.acceptance.frame.wait",json!({"run":"actor-owned","instance":51,
                    "window":window.raw(),"activation_epoch":stamp.activation_epoch,"local_revision":stamp.local_revision,"timeout_ms":10000})).await})
            };
            tokio::time::timeout(Duration::from_secs(2),async {
                assert_eq!(admission_rx.recv().await,Some(1));
                assert_eq!(admission_rx.recv().await,Some(2));
            }).await.expect("both actual native wait operations admitted");
            assert!(!barrier_wait.is_finished());
            assert!(!frame_wait.is_finished());
            let held = caller.call("term","app.acceptance.barrier.state",reference.clone()).await.unwrap();
            assert_eq!(held["state"],"armed","control progresses while both wait operations are retained");
            let mut next = initial; next.zoom_steps = 1;
            started.ui.set_context(next,started.handle.settings_generation()).unwrap();
            let reached = tokio::time::timeout(Duration::from_secs(3),barrier_wait).await.unwrap().unwrap().unwrap();
            assert_eq!(reached["state"],"reached","actual preparation worker must reach the hold");
            let released = tokio::time::timeout(Duration::from_secs(2),caller.call("term","app.acceptance.barrier.release",reference)).await.unwrap().unwrap();
            assert_eq!(released["state"],"released","release must bypass held frame wait");
            let state = caller.call("term","app.acceptance.frame.state",identity.clone()).await.unwrap();
            assert_eq!(state["target"]["window"],window.raw());
            let describe = caller.call("term","app.acceptance.describe",identity).await.unwrap();
            assert_eq!(describe["frames"]["enabled"],true);
            started.handle.quit();
            let stopped = tokio::time::timeout(Duration::from_secs(3),frame_wait).await.unwrap().unwrap().unwrap();
            assert_eq!(stopped["ok"],false);
            assert!(stopped["error"].as_str().unwrap().contains("Closed"));
            assert!(started.frames.snapshot().closed);
        });
        cleanup.submit(tabs.lock().unwrap().shutdown());
        started.handle.quit();
        started.handle.wait_done().unwrap();
        started.worker.join().unwrap();
        drop(cleanup);
        reaper.join().unwrap();
    }

    /// One client serves both lanes: an `app.describe` request is tracked by
    /// the adapter, answered by the UI half (the frontend) and carried back
    /// to the caller over the real embedded broker.
    #[test]
    fn one_client_serves_settings_and_describe_round_trips() {
        let broker = term_test_broker::Broker::start();
        let settings = term_core::config::Settings {
            config: term_core::config::Config::default(),
            term: "xterm-256color",
        };
        let tabs = Arc::new(Mutex::new(TabSet::starting(settings)));
        let (cleanup, reaper) = term_core::tabs::Cleanup::start().unwrap();
        let (_notes, notify_rx) = tokio::sync::mpsc::unbounded_channel();
        let wake: Wake = Arc::new(|| {});
        let mut started = start(
            "term",
            broker.url.clone(),
            tabs.clone(),
            cleanup.clone(),
            notify_rx,
            wake,
            PreparationSeed {
                local: LocalContext::new(1.0, term_core::config::Cursor::Underline).unwrap(),
                #[cfg(feature = "acceptance")]
                fixture: None,
                #[cfg(feature = "acceptance")]
                fixture_admission: None,
                raster: term_core::raster::Raster::for_test(
                    1.0,
                    13.0,
                    term_core::config::Cursor::Underline,
                )
                .unwrap()
                .prepared_snapshot(),
            },
        )
        .unwrap();
        runtime().block_on(async {
            let caller = ::bus::native_client::NodedClient::connect_anonymous(&broker.url)
                .await
                .unwrap();
            // The UI half answers while the caller waits, exactly as the
            // frontend does on its wake loop.
            let handle = started.handle.clone();
            let answered = tokio::spawn(async move {
                loop {
                    while let Ok(describe) = started.describes.try_recv() {
                        handle.reply(&describe, 0, json!({"app": "term"}));
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            });
            let mut answer = None;
            for _ in 0..40 {
                answer = caller.call("term", "app.describe", json!({})).await.ok();
                if answer.is_some() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            answered.abort();
            let body = answer.expect("describe answered once registered");
            assert_eq!(body["app"], "term");
        });
        cleanup.submit(tabs.lock().unwrap().shutdown());
        started.handle.quit();
        started.handle.wait_done().unwrap();
        started.worker.join().unwrap();
        drop(cleanup);
        reaper.join().unwrap();
    }

    /// Successive overflow markers aggregate, saturating, while the verb
    /// queue is full — the retained count is a total, never a replacement.
    #[test]
    fn repeated_overflows_aggregate_saturating_while_retained() {
        let mut pending = None;
        retain_overflow(&mut pending, 3);
        retain_overflow(&mut pending, 5);
        assert!(matches!(
            pending,
            Some(BoundedIncomingEvent::Overflow { dropped: 8 })
        ));
        retain_overflow(&mut pending, u64::MAX);
        assert!(matches!(
            pending,
            Some(BoundedIncomingEvent::Overflow { dropped: u64::MAX })
        ));
    }

    /// The full-queue mechanics themselves: a retained marker waits for real
    /// capacity, is delivered once it frees, and a closed lane drops it once
    /// instead of spinning on it. No external poller — the channel is the
    /// queue the worker feeds.
    #[test]
    fn a_retained_overflow_waits_for_verb_queue_capacity_and_drops_on_a_closed_lane() {
        fn command() -> BoundedIncomingEvent {
            BoundedIncomingEvent::Command(IncomingCommand {
                generation: 0,
                from: "test".into(),
                command: "term.tabs".into(),
                id: None,
                args: serde_json::Value::Null,
                body: String::new(),
                headers: Default::default(),
            })
        }
        let (tx, mut rx) = tokio::sync::mpsc::channel::<BoundedIncomingEvent>(1);
        tx.try_send(command()).unwrap(); // the queue is full
        let mut pending = Some(BoundedIncomingEvent::Overflow { dropped: 2 });
        assert!(matches!(
            deliver_retained(&tx, &mut pending),
            Retained::Full
        ));
        assert!(matches!(
            pending,
            Some(BoundedIncomingEvent::Overflow { dropped: 2 })
        ));
        rx.try_recv().unwrap(); // capacity frees
        assert!(matches!(
            deliver_retained(&tx, &mut pending),
            Retained::Delivered
        ));
        assert!(pending.is_none());
        assert!(matches!(
            rx.try_recv().unwrap(),
            BoundedIncomingEvent::Overflow { dropped: 2 }
        ));
        // A closed lane drops the marker once instead of retaining forever.
        let (tx, rx) = tokio::sync::mpsc::channel::<BoundedIncomingEvent>(1);
        drop(rx);
        let mut pending = Some(BoundedIncomingEvent::Overflow { dropped: 1 });
        assert!(matches!(
            deliver_retained(&tx, &mut pending),
            Retained::Closed
        ));
        assert!(pending.is_none());
    }

    #[test]
    fn describe_credit_stays_owned_by_completed_unreaped_output() {
        runtime().block_on(async {
            let admission = Admission::new(PENDING_CAP);
            let mut tasks = tokio::task::JoinSet::new();
            let (completed, mut completion) = tokio::sync::mpsc::channel(PENDING_CAP);
            for _ in 0..PENDING_CAP {
                let permit = admission.try_acquire().unwrap();
                let completed = completed.clone();
                tasks.spawn(async move {
                    completed.send(()).await.unwrap();
                    (permit, Ok::<(), String>(()))
                });
            }
            for _ in 0..PENDING_CAP {
                completion.recv().await.unwrap();
            }
            tokio::task::yield_now().await;
            assert!(
                admission.try_acquire().is_none(),
                "finished tasks retain accepted credit until reap"
            );
            assert_eq!(admission.counts().active, PENDING_CAP);
            let (permit, result) = tasks.join_next().await.unwrap().unwrap();
            result.unwrap();
            permit.finish();
            let one = admission.try_acquire().unwrap();
            assert!(admission.try_acquire().is_none());
            one.finish();
            while let Some(result) = tasks.join_next().await {
                let (permit, result) = result.unwrap();
                result.unwrap();
                permit.finish();
            }
            assert_eq!(admission.counts().active, 0);
            assert_eq!(admission.counts().finished, PENDING_CAP as u64 + 1);
            assert_eq!(admission.counts().abandoned, 0);
        });
    }

    #[test]
    fn cloned_frontend_request_queues_only_one_reply_effect() {
        let mut handle = Handle::sink();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        handle.tx = tx;
        let describe = Describe {
            id: 42,
            ticket: Once::new(42),
        };
        handle.reply(&describe, 0, json!({"app":"term"}));
        handle.reply(&describe.clone(), 0, json!({"app":"term"}));
        assert!(matches!(rx.try_recv(), Ok(Effect::Reply(42, 0, _))));
        assert!(matches!(
            rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
    }

    /// A describe flood with a silent frontend is refused BUSY rather than
    /// queued unboundedly; accepted describes keep waking the settings lane;
    /// and quit still progresses while a stalled broker wedges every
    /// in-flight reply under the shared shutdown deadline.
    #[test]
    fn a_describe_flood_with_slow_responses_stays_bounded_and_quit_progresses() {
        use std::collections::BTreeMap;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let broker = term_test_broker::Broker::start();
        let settings = term_core::config::Settings {
            config: term_core::config::Config::default(),
            term: "xterm-256color",
        };
        let tabs = Arc::new(Mutex::new(TabSet::starting(settings)));
        let (cleanup, reaper) = term_core::tabs::Cleanup::start().unwrap();
        let (_notes, notify_rx) = tokio::sync::mpsc::unbounded_channel();
        let wakes = Arc::new(AtomicUsize::new(0));
        let (wake_tx, mut wake_rx) = tokio::sync::mpsc::unbounded_channel();
        let wake: Wake = {
            let wakes = Arc::clone(&wakes);
            Arc::new(move || {
                wakes.fetch_add(1, Ordering::Relaxed);
                let _ = wake_tx.send(());
            })
        };
        let mut started = start(
            "term",
            broker.url.clone(),
            tabs.clone(),
            cleanup.clone(),
            notify_rx,
            wake,
            PreparationSeed {
                local: LocalContext::new(1.0, term_core::config::Cursor::Underline).unwrap(),
                #[cfg(feature = "acceptance")]
                fixture: None,
                #[cfg(feature = "acceptance")]
                fixture_admission: None,
                raster: term_core::raster::Raster::for_test(
                    1.0,
                    13.0,
                    term_core::config::Cursor::Underline,
                )
                .unwrap()
                .prepared_snapshot(),
            },
        )
        .unwrap();
        runtime().block_on(async {
            let caller = Arc::new(
                ::bus::native_client::NodedClient::connect_anonymous(&broker.url)
                    .await
                    .unwrap(),
            );
            let before = wakes.load(Ordering::Relaxed);
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            // Observe the actual supervised client's registration watch.
            // This silent frontend deliberately does not drain the settings
            // mailbox, whose notifications are coalesced until UI consumption.
            let mut connection = started
                .handle
                .shared
                .read()
                .unwrap()
                .client
                .as_ref()
                .expect("actual supervised client")
                .subscribe_state();
            while *connection.borrow_and_update() != ConnState::Connected {
                tokio::time::timeout_at(deadline, connection.changed())
                    .await
                    .expect("adapter registers before the deadline")
                    .expect("connection watch remains open");
            }
            // Fill the frontend admission map one accepted request at a time.
            // A simultaneous transport flood can overflow the incoming lane
            // before reaching this cap and would not establish BUSY behaviour.
            let mut calls = tokio::task::JoinSet::new();
            let mut accepted = Vec::new();
            for _ in 0..PENDING_CAP {
                let caller = Arc::clone(&caller);
                calls.spawn(async move {
                    caller
                        .call_with_headers_raw("term", "app.describe", &BTreeMap::new(), "{}")
                        .await
                });
                loop {
                    match started.describes.try_recv() {
                        Ok(describe) => {
                            accepted.push(describe);
                            break;
                        }
                        Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                            tokio::time::timeout_at(deadline, wake_rx.recv())
                                .await
                                .expect("each request reaches frontend admission")
                                .expect("wake lane remains open");
                        }
                        Err(error) => panic!("describe lane closed: {error}"),
                    }
                }
            }
            assert_eq!(accepted.len(), PENDING_CAP);
            for _ in 0..4 {
                let caller = Arc::clone(&caller);
                calls.spawn(async move {
                    caller
                        .call_with_headers_raw("term", "app.describe", &BTreeMap::new(), "{}")
                        .await
                });
            }
            let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
            let mut refused = 0;
            while refused < 4 && tokio::time::Instant::now() < deadline {
                match tokio::time::timeout_at(deadline, calls.join_next()).await {
                    Ok(Some(Ok(Ok((rc, body, _))))) if rc == 10 && body.contains("BUSY") => {
                        refused += 1;
                    }
                    Ok(Some(_)) => {}
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
            calls.abort_all();
            assert!(
                refused == 4,
                "the flood beyond the pending cap is refused BUSY, not queued"
            );
            assert!(
                wakes.load(Ordering::Relaxed) > before,
                "accepted describes keep publishing into the settings lane"
            );
            // Slow-response leg: a stalled broker wedges every in-flight
            // send; quit must still progress under the shared deadline.
            let paused = broker.pause();
            for id in accepted {
                started.handle.reply(&id, 0, json!({"app": "term"}));
            }
            started.handle.quit();
            started
                .handle
                .wait_done()
                .expect("quit progresses under wedged replies");
            started.worker.join().unwrap();
            drop(paused);
        });
        cleanup.submit(tabs.lock().unwrap().shutdown());
        drop(cleanup);
        reaper.join().unwrap();
    }
}

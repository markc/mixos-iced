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
use application::presentation::native::{
    Event as SettingsEvent, Progress, Session, Ui, Worker, bridge,
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Condvar, Mutex, RwLock},
    time::Duration,
};
use term_core::tabs::{Cleanup, CompletionNote, TabSet};
use term_core::terminal::Wake;

/// One routed delivery for the UI: an `app.describe` request to answer.
#[derive(Debug, Clone)]
pub struct Describe {
    pub id: u64,
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

    pub fn reply(&self, id: u64, rc: u8, value: Value) {
        let _ = self.tx.send(Effect::Reply(id, rc, value));
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
    pub ui: Ui<Content, LocalContext>,
    pub bootstrap: appearance::settings::Prepared,
    pub describes: std::sync::mpsc::Receiver<Describe>,
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
}

struct WorkerChannels {
    describes: std::sync::mpsc::Sender<Describe>,
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
    let (describe_tx, describe_rx) = std::sync::mpsc::channel();
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
                            describes: describe_tx,
                            effects: rx,
                            ready: ready_send,
                            seed,
                        },
                    ));
                    // Bounded shutdown: the supervisor, serve task and flush
                    // have finished; 100 ms is a ceiling, not a wait.
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
const OPERATIONS_CAP: usize = 32;
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
        describes,
        mut effects,
        ready,
        seed,
    } = channels;
    let PreparationSeed {
        local: initial,
        raster,
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
    let settings_worker = Worker::contextual(move |appearance, snapshot, local: &LocalContext| {
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
    let mut pending: HashMap<u64, (Arc<SupervisedClient>, IncomingCommand)> = HashMap::new();
    let mut next_id = 0;
    let mut lifecycle = None;
    let mut incoming_open = true;
    let mut connection_open = true;
    let mut pending_overflow: Option<BoundedIncomingEvent> = None;
    // Bounded reply tasks: the permit pool caps every in-flight respond, so a
    // BUSY flood cannot allocate unbounded tasks. Accepted replies take
    // priority — when the pool is exhausted they are RETAINED here and
    // started as capacity frees; refusals are shed with a diagnostic instead.
    let permits = Arc::new(tokio::sync::Semaphore::new(OPERATIONS_CAP));
    let mut retained_replies: VecDeque<(Arc<SupervisedClient>, IncomingCommand, u8, String)> =
        VecDeque::new();
    let mut operations = tokio::task::JoinSet::new();
    loop {
        // Accepted replies retained while the task cap was exhausted start
        // the moment capacity frees; an unused permit returns immediately.
        while let Ok(permit) = permits.clone().try_acquire_owned() {
            match retained_replies.pop_front() {
                Some((client, command, rc, body)) => {
                    operations.spawn(async move {
                        let _permit = permit;
                        respond(client, command, rc, body).await
                    });
                }
                None => break, // the permit is returned unused
            }
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
        if lifecycle != Some(now) {
            lifecycle = Some(now);
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
                        let _ = tokio::time::timeout(Duration::from_secs(2), client.close()).await;
                        // Replies from the old connection can never be sent:
                        // fence both the pending describes and any retained
                        // accepted replies on the replaced generation.
                        pending.clear();
                        retained_replies.clear();
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
            pending.retain(|_, (stored, command)| {
                Arc::ptr_eq(stored, &client) && client.connection_generation() == command.generation
            });
        }
        // Fairness under an incoming flood is ordered, not left to chance:
        // completed reply tasks (which free the operation cap), the settings
        // lane and UI effects — including Quit — are polled before the
        // command lane, so accepted replies and quit progress however ready
        // `incoming` is. Each earlier branch is only transiently ready, so
        // the command lane cannot starve either.
        tokio::select! {
            biased;
            result = operations.join_next(), if !operations.is_empty() => {
                if let Some(Err(error)) = result {
                    eprintln!("{service} Bus reply: {error}");
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
                        if let Some((client, command)) = pending.remove(&id) {
                            let body = value.to_string();
                            match permits.clone().try_acquire_owned() {
                                Ok(permit) => {
                                    operations.spawn(async move {
                                        let _permit = permit;
                                        respond(client, command, rc, body).await
                                    });
                                }
                                Err(_) => retained_replies
                                    .push_back((client, command, rc, body)),
                            }
                        }
                    }
                    Effect::Quit => break,
                }
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
                if command.command == "app.describe" {
                    // The frontend answers: a UI request that reconciles live
                    // settings evidence first, tracked here by id, bounded so
                    // a slow UI cannot grow the map without limit. While the
                    // reply path is saturated (retained accepted replies), new
                    // describes are refused instead of queueing further.
                    if pending.len() >= PENDING_CAP || !retained_replies.is_empty() {
                        let client = Arc::clone(&client);
                        try_spawn(service, &permits, &mut operations, client, command, 10,
                            "{\"error_code\":\"BUSY\",\"message\":\"too many pending commands\"}".to_string());
                        continue;
                    }
                    next_id += 1;
                    if describes.send(Describe { id: next_id }).is_err() {
                        // The UI is gone: no answer can ever come. Refuse once,
                        // tracked on the originating client.
                        let client = Arc::clone(&client);
                        try_spawn(service, &permits, &mut operations, client, command, 10,
                            "{\"error_code\":\"CLOSED\",\"message\":\"no frontend to answer app.describe\"}".to_string());
                        continue;
                    }
                    pending.insert(next_id, (Arc::clone(&client), command));
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
                            try_spawn(service, &permits, &mut operations, client, command, 10,
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
            changed = connection.changed(), if connection_open => {
                if changed.is_err() { connection_open = false; }
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
    // tabs.close -> global Bus finish: the serve task has drained the final
    // reap's completion notes by the time the TabSet emptied; wait for it,
    // the tracked replies, the cache flush and the client close all under ONE
    // shared 2 s deadline. Tasks that exhaust the budget are aborted, never
    // detached.
    let deadline = std::time::Instant::now() + SHUTDOWN_BUDGET;
    let deadline_at = tokio::time::Instant::from_std(deadline);
    let mut faults = Vec::new();
    // Accepted replies retained while the task cap was exhausted stay fenced
    // on their originating client and generation: they must be sent under the
    // shared deadline or be counted as undelivered — never silently dropped.
    // Retained + in-flight stays within 2 × OPERATIONS_CAP at shutdown.
    for (client, command, rc, body) in retained_replies.drain(..) {
        operations.spawn(respond(client, command, rc, body));
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
            }
        }
    }
    while !operations.is_empty() {
        match tokio::time::timeout_at(deadline_at, operations.join_next()).await {
            Ok(Some(Ok(Ok(())))) => {}
            Ok(Some(Ok(Err(error)))) => faults.push(error),
            Ok(Some(Err(error))) => faults.push(format!("term Bus reply: {error}")),
            Ok(None) => break,
            Err(_) => {
                let undelivered = operations.len();
                operations.abort_all();
                if undelivered > 0 {
                    faults.push(format!(
                        "term Bus reply drain timed out with {undelivered} undelivered replies"
                    ));
                }
                break;
            }
        }
    }
    if let Err(error) = lane.flush_cache(deadline).await {
        faults.push(format!("settings cache: {}: {}", error.code, error.message));
    }
    if tokio::time::timeout_at(deadline_at, client.close())
        .await
        .is_err()
    {
        faults.push("Bus close timed out".into());
    }
    eprintln!("TERM_SHUTDOWN {}", json!({"faults": faults}));
}

/// One bounded reply on the client that received the command. Spawned as a
/// tracked operation, so the settings lane is never blocked on a send.
async fn respond(
    client: Arc<SupervisedClient>,
    command: IncomingCommand,
    rc: u8,
    body: String,
) -> Result<(), String> {
        tokio::time::timeout(
            Duration::from_secs(2),
            SupervisedClient::respond(&client, &command, rc, &body),
        )
        .await
        .map_err(|_| "Bus reply timed out".to_owned())?
        .map_err(|error| format!("Bus reply: {error}"))
}

/// Start one bounded reply task if a permit remains. A refusal under
/// saturation is shed with a diagnostic — the caller times out — while an
/// accepted reply is retained by the caller instead, never dropped here.
fn try_spawn(
    service: &'static str,
    permits: &Arc<tokio::sync::Semaphore>,
    operations: &mut tokio::task::JoinSet<Result<(), String>>,
    client: Arc<SupervisedClient>,
    command: IncomingCommand,
    rc: u8,
    body: String,
) {
    let Ok(permit) = permits.clone().try_acquire_owned() else {
        eprintln!("{service} Bus reply capacity exhausted; dropping a refusal (caller times out)");
        return;
    };
    operations.spawn(async move {
        let _permit = permit;
        respond(client, command, rc, body).await
    });
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
        let started = start(
            "term",
            broker.url.clone(),
            tabs.clone(),
            cleanup.clone(),
            notify_rx,
            wake,
            PreparationSeed {
                local: LocalContext::new(1.0, term_core::config::Cursor::Underline).unwrap(),
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
                    for describe in started.describes.try_iter() {
                        handle.reply(describe.id, 0, json!({"app": "term"}));
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

    /// The operations cap is a real permit bound: nothing starts beyond it,
    /// and capacity returns with completed tasks. `try_acquire_owned` takes
    /// the `Arc`, exactly as the worker holds it.
    #[test]
    fn operations_permits_bound_in_flight_reply_tasks() {
        let permits = Arc::new(tokio::sync::Semaphore::new(OPERATIONS_CAP));
        let held: Vec<_> = (0..OPERATIONS_CAP)
            .map(|_| {
                permits
                    .clone()
                    .try_acquire_owned()
                    .expect("permit under the cap")
            })
            .collect();
        assert!(
            permits.clone().try_acquire_owned().is_err(),
            "no reply task may start beyond the operations cap"
        );
        drop(held);
        assert!(
            permits.clone().try_acquire_owned().is_ok(),
            "capacity returns with the completed tasks"
        );
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
        let started = start(
            "term",
            broker.url.clone(),
            tabs.clone(),
            cleanup.clone(),
            notify_rx,
            wake,
            PreparationSeed {
                local: LocalContext::new(1.0, term_core::config::Cursor::Underline).unwrap(),
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
            let mut connection = started.handle.shared.read().unwrap().client.as_ref()
                .expect("actual supervised client").subscribe_state();
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
                            accepted.push(describe.id);
                            break;
                        }
                        Err(std::sync::mpsc::TryRecvError::Empty) => {
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
                started.handle.reply(id, 0, json!({"app": "term"}));
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

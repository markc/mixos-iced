// SPDX-License-Identifier: MIT OR Apache-2.0
//! One supervised native Bus connection on its own bounded runtime. Startup is
//! nonblocking (`options.start`): the settings lane and the app stream are
//! ready before registration settles. Settings frames decode before ordinary
//! traffic; channel overflow and lifecycle loss are explicit. An explicit
//! typed name collision hands the launch paths to the running instance on this
//! same worker — never from reply text, and only when no connection was ever
//! established.
//!
//! The GUI outbox is bounded and reliable: commands await their slot, while
//! settings wakes, lifecycle edges and refresh nudges coalesce. Accepted
//! replies travel through one ordered writer with a fixed admission sum
//! (pending + queued + in flight), so an accepted reply is never shed;
//! refusals use a separate capped pool with diagnosed shedding. Quit drains
//! the accepted writer, refusals, capture restoration calls, the settings
//! cache and the client close against one shared deadline.
use ::bus::native_client::{
    BoundedIncomingEvent, ConnState, IncomingCommand, NodedClient, RegistrationRejectionKind,
    SupervisedClient, SupervisedError,
};
use application::iced::futures::SinkExt;
use application::iced::futures::channel::{mpsc, oneshot};
use application::presentation::native::{
    Event as SettingsEvent, Progress, Session, Ui, Worker, bridge,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};

/// Everything the bus thread delivers to the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// A `cap.*` or `app.describe` command.
    Command(Command),
    /// The bounded incoming channel lost frames; resample window state.
    Changed,
    /// A settings event batch awaits a UI drain.
    Settings,
    /// The supervised connection was refused or stopped. The GUI keeps its
    /// offline UI; only an actual quit ends the process.
    Refused {
        message: String,
    },
    /// The launch paths were handed to the running instance (name collision).
    Forwarded(Result<(), String>),
    Connected,
    Disconnected,
}

/// One request to cap. `id` indexes a pending reply; `None`-reply verbs
/// still get one (an error reply at least).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub id: u64,
    pub verb: String,
    pub body: String,
    /// `local:<from>` / `mesh:<service>@<peer>` / `anon` (editd E0 §4.3).
    pub caller_key: String,
}

/// Effects the app sends back to the bus thread.
#[derive(Debug)]
pub enum Effect {
    /// Reply to command `id` with `(rc, body)`.
    Respond { id: u64, rc: u8, body: String },
    Call {
        service: String,
        verb: String,
        args: Value,
        limit: Duration,
        reply: oneshot::Sender<Result<Value, String>>,
    },
    Delay {
        duration: Duration,
        reply: oneshot::Sender<()>,
    },
    /// Stop the bus thread (the app is quitting).
    Quit,
}

/// The handle the app uses to call, reply and quit.
#[derive(Clone)]
pub struct BusHandle {
    tx: tokio::sync::mpsc::UnboundedSender<Effect>,
    /// Set + notified when the bus thread has finished (replies flushed,
    /// client closed) — `wait_done` before process exit guarantees the
    /// last reply reached the wire instead of racing it.
    done: Arc<(Mutex<bool>, Condvar)>,
    client: Option<Arc<SupervisedClient>>,
}

impl BusHandle {
    /// The actual supervised connection state, sampled now — never a queued
    /// edge that a later state change already invalidated.
    pub fn connected(&self) -> bool {
        self.client
            .as_ref()
            .is_none_or(|client| settings::native::live_generation(client).is_some())
    }
    pub fn settings_generation(&self) -> Option<u64> {
        self.client
            .as_ref()
            .and_then(|client| settings::native::live_generation(client))
    }
    pub fn ever_registered(&self) -> bool {
        self.client
            .as_ref()
            .is_some_and(|client| client.connection_generation() > 0)
    }
    pub async fn call(
        &self,
        service: &str,
        verb: &str,
        args: Value,
        limit: Duration,
    ) -> Result<Value, String> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Effect::Call {
                service: service.into(),
                verb: verb.into(),
                args,
                limit,
                reply: tx,
            })
            .map_err(|_| "Bus worker stopped")?;
        rx.await.map_err(|_| "Bus request abandoned")?
    }
    pub async fn delay(&self, duration: Duration) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Effect::Delay {
                duration,
                reply: tx,
            })
            .map_err(|_| "Bus worker stopped")?;
        rx.await.map_err(|_| "Bus delay abandoned".into())
    }
    /// Exercise the real window command performer without a broker connection.
    #[cfg(test)]
    pub fn response_sink() -> (Self, tokio::sync::mpsc::UnboundedReceiver<Effect>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (
            Self {
                tx,
                done: Arc::new((Mutex::new(true), Condvar::new())),
                client: None,
            },
            rx,
        )
    }
    pub fn respond(&self, id: u64, rc: u8, body: String) {
        let _ = self.tx.send(Effect::Respond { id, rc, body });
    }
    pub fn quit(&self) {
        let _ = self.tx.send(Effect::Quit);
    }
    /// Block until the bus thread is finished (bounded). Call after
    /// [`BusHandle::quit`] and before exiting the process.
    pub fn wait_done(&self, timeout: Duration) {
        let (lock, notified) = &*self.done;
        let finished = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *finished {
            return;
        }
        let _ = notified
            .wait_timeout_while(finished, timeout, |finished| !*finished)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
    }
}

/// The attested caller key editd would derive (E0 §4.3). noded strips
/// client-supplied `broker_*` headers, so these are the broker's stamps.
pub fn caller_key(cmd: &IncomingCommand) -> String {
    match cmd.header("broker_origin") {
        Some("local") if !cmd.from.is_empty() => format!("local:{}", cmd.from),
        Some("mesh") => format!(
            "mesh:{}@{}",
            cmd.header("broker_service").unwrap_or("unknown"),
            cmd.header("broker_peer").unwrap_or("unknown")
        ),
        _ => "anon".to_string(),
    }
}

/// Accepted commands awaiting a reply, fenced to the connection generation
/// each arrived on. Entries survive disconnects; a reply for a dead
/// connection is never sent through its replacement.
#[derive(Default)]
struct Pending {
    commands: HashMap<u64, IncomingCommand>,
}
impl Pending {
    fn insert(&mut self, id: u64, command: IncomingCommand) {
        self.commands.insert(id, command);
    }
    fn len(&self) -> usize {
        self.commands.len()
    }
    /// Take a reply, fenced to `live`: `None` when the command belongs to
    /// another connection or is unknown. The authoritative fence is the
    /// client's own generation check at send time; this avoids enqueuing a
    /// doomed reply at all.
    fn take(&mut self, id: u64, live: u64) -> Option<IncomingCommand> {
        match self.commands.get(&id) {
            Some(command) if command.generation == live => self.commands.remove(&id),
            _ => None,
        }
    }
}

/// The fixed sum across accepted work: pending commands plus replies queued
/// or in flight. Admission refuses a new command before the sum can reach
/// the cap, so an accepted reply is never shed and the accepted pipeline
/// never exceeds its channel.
const ACCEPTED_CAP: usize = 32;
/// Separate capped pool for refusal replies; saturation is diagnosed.
const REFUSAL_CAP: usize = 16;
/// Outbound operations (calls, delays).
const OPERATION_CAP: usize = 32;
/// The GUI outbox: commands await a slot; edges coalesce or are dropped only
/// when the app is already behind (they carry no exclusive data).
const OUTBOX_CAP: usize = 64;

/// Mirrors the accepted sum for admission and the truthful shutdown receipt.
#[derive(Default)]
struct AcceptedGate {
    pending: usize,
    queued: usize,
}
impl AcceptedGate {
    fn admit(&self) -> bool {
        self.pending + self.queued < ACCEPTED_CAP
    }
    fn accepted(&mut self) {
        self.pending += 1;
    }
    fn replied(&mut self) {
        self.pending -= 1;
        self.queued += 1;
    }
    fn sent(&mut self) {
        self.queued -= 1;
    }
}

/// The single-instance handoff gate: only an explicit typed name collision
/// on a connection that never established, with launch paths to forward.
fn handoff_gate(
    kind: RegistrationRejectionKind,
    generation: u64,
    handoff: &Option<Vec<String>>,
) -> bool {
    kind == RegistrationRejectionKind::NameTaken && generation == 0 && handoff.is_some()
}

/// Start the bus thread registered as `service`, connecting to `url`
/// (`::bus::client_helpers::resolve_noded_url()` unless `--noded-url`
/// overrode it). `handoff` carries the launch paths a typed name collision
/// forwards to the running instance (`None` disables the handoff, e.g. the
/// headless service, whose duplicate remains a plain error). The returned
/// bootstrap presentation uses only generic families; installed fonts are
/// registered on the worker before the settings lane may prepare a checked
/// presentation.
pub fn start(
    service: &str,
    url: &str,
    handoff: Option<Vec<String>>,
) -> Result<
    (
        BusHandle,
        Ui<()>,
        appearance::settings::Prepared,
        mpsc::Receiver<Delivery>,
    ),
    String,
> {
    let (send, receive) = mpsc::channel(OUTBOX_CAP);
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let (ready_send, ready_receive) = std::sync::mpsc::channel();
    let done = Arc::new((Mutex::new(false), Condvar::new()));
    let finished = Arc::clone(&done);
    let service = service.to_owned();
    let url = url.to_owned();
    std::thread::Builder::new()
        .name(format!("{service}-bus"))
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            match runtime {
                Ok(runtime) => {
                    runtime.block_on(worker(service, url, handoff, send, rx, ready_send));
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
    let (client, ui, bootstrap) = ready_receive
        .recv_timeout(Duration::from_secs(5))
        .map_err(|e| format!("Bus startup: {e}"))??;
    Ok((
        BusHandle {
            tx,
            done,
            client: Some(client),
        },
        ui,
        bootstrap,
        receive,
    ))
}
type Ready = std::sync::mpsc::Sender<
    Result<
        (
            Arc<SupervisedClient>,
            Ui<()>,
            appearance::settings::Prepared,
        ),
        String,
    >,
>;
/// A settings wake is a poke, coalesced by the mailbox itself; it carries no
/// exclusive data, so a full outbox only delays a wake the queued deliveries
/// already imply.
fn settings_wake(send: &mut mpsc::Sender<Delivery>, needed: bool) {
    if needed {
        let _ = send.try_send(Delivery::Settings);
    }
}

/// Refusal for a saturated command queue.
const BUSY_BODY: &str = "{\"error\":\"too many pending Cap commands\"}";
/// The one total shutdown budget: accepted replies, refusals, capture
/// restoration calls, the settings cache and the client close.
const SHUTDOWN_BUDGET: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Edge {
    Connected,
    Disconnected,
}

async fn worker(
    service: String,
    url: String,
    mut handoff: Option<Vec<String>>,
    mut send: mpsc::Sender<Delivery>,
    mut effects: tokio::sync::mpsc::UnboundedReceiver<Effect>,
    ready: Ready,
) {
    let consumer = match settings::session::binding()
        .and_then(|binding| settings::consumer::Consumer::for_app(binding, "cap"))
    {
        Ok(consumer) => consumer,
        Err(error) => {
            let _ = ready.send(Err(error.message));
            return;
        }
    };
    let bootstrap = match appearance::settings::bootstrap() {
        Ok(bootstrap) => bootstrap,
        Err(error) => {
            let _ = ready.send(Err(error.message));
            return;
        }
    };
    let client = Arc::new(
        SupervisedClient::connect_options(&service, &url)
            .fatal_on_registration_rejection(true)
            .bounded_incoming(64)
            .start(),
    );
    let Some(mut incoming) = client.incoming_bounded() else {
        let _ = ready.send(Err("no incoming Bus channel".into()));
        return;
    };
    let mut connection = client.subscribe_state();
    let build = |_: &appearance::settings::Prepared, _: &settings::Snapshot| Ok(());
    let settings_worker = match config::AppDirs::resolve("cap") {
        Some(dirs) => Worker::offline_with_cache(dirs.cache().join("settings"), build),
        None => Worker::offline(build),
    };
    let (ui, mut lane) = bridge(Session::new(consumer), settings_worker);
    settings_wake(&mut send, lane.connect(Arc::clone(&client)));
    if ready
        .send(Ok((Arc::clone(&client), ui, bootstrap)))
        .is_err()
    {
        let _ = client.close().await;
        return;
    }
    // Font files are installed on the existing worker, after UI readiness and
    // before the Lane may prepare a checked presentation.
    if let Err(error) = appearance::fonts::register_installed() {
        eprintln!("cap: static assets: {error}");
    }
    let mut pending = Pending::default();
    let mut gate = AcceptedGate::default();
    // One ordered accepted-reply writer: a single serial task sends replies
    // in admission order, so accepted capture and quit replies reach the
    // wire in FIFO. The acks keep the admission sum exact.
    let (accepted_tx, mut accepted_rx) =
        tokio::sync::mpsc::channel::<(u8, String, IncomingCommand)>(ACCEPTED_CAP);
    let (acks, mut ack_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let faults = Arc::new(Mutex::new(Vec::<String>::new()));
    let accepted_writer = tokio::spawn({
        let client = client.clone();
        let faults = faults.clone();
        let acks = acks.clone();
        async move {
            while let Some((rc, body, command)) = accepted_rx.recv().await {
                let result = client.respond(&command, rc, &body).await;
                if let Err(error) = result {
                    faults
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(format!("reply: {error}"));
                }
                let _ = acks.send(());
            }
        }
    });
    let mut refusals: tokio::task::JoinSet<Result<(), SupervisedError>> =
        tokio::task::JoinSet::new();
    let mut operations = tokio::task::JoinSet::new();
    let mut shed_refusals = 0u64;
    let mut busy_rejected = 0u64;
    let mut next_id = 0;
    let mut handoff_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut incoming_open = true;
    let mut connection_open = true;
    let mut lifecycle = None;
    let mut last_edge = None;
    let mut changed_out = false;
    let mut deadline = None;
    // Sample once before waiting: fast registration may already have completed.
    loop {
        while let Ok(()) = ack_rx.try_recv() {
            gate.sent();
        }
        // Reap the finished single-instance handoff, its result already
        // delivered; a panicked handoff is recorded, never silent.
        if handoff_task.as_mut().is_some_and(|task| task.is_finished()) {
            if let Err(error) = handoff_task.take().expect("finished handoff").await {
                faults
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(error.to_string());
                eprintln!("cap: handoff: {error}");
            }
        }
        let now = *connection.borrow_and_update();
        if lifecycle != Some(now) {
            lifecycle = Some(now);
            settings_wake(&mut send, lane.publish(SettingsEvent::Wake));
            match now {
                ConnState::Connected => {
                    if last_edge != Some(Edge::Connected) {
                        last_edge = Some(Edge::Connected);
                        let _ = send.try_send(Delivery::Connected);
                    }
                }
                ConnState::Fatal | ConnState::ShuttingDown => {
                    let rejection = client.registration_rejection();
                    let message = rejection
                        .clone()
                        .map_or_else(|| "connection stopped".into(), |reason| reason.message);
                    let _ = send.try_send(Delivery::Refused { message });
                    if rejection.as_ref().is_some_and(|reason| {
                        handoff_gate(reason.kind(), client.connection_generation(), &handoff)
                    }) && let Some(paths) = handoff.take()
                    {
                        let url = url.clone();
                        let service = service.clone();
                        let mut send = send.clone();
                        handoff_task = Some(tokio::spawn(async move {
                            let result = forward_async(&url, &service, &paths).await;
                            let _ = send.send(Delivery::Forwarded(result)).await;
                        }));
                    }
                }
                ConnState::Disconnected => {
                    if last_edge != Some(Edge::Disconnected) {
                        last_edge = Some(Edge::Disconnected);
                        let _ = send.try_send(Delivery::Disconnected);
                    }
                }
                ConnState::Connecting => {}
            }
        }
        tokio::select! {
            biased;
            result = refusals.join_next(), if !refusals.is_empty() => {
                match result {
                    Some(Ok(Ok(()))) => {}
                    Some(Ok(Err(error))) => {
                        faults
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .push(format!("refusal: {error}"));
                        eprintln!("cap: Bus reply: {error}");
                    }
                    Some(Err(error)) => {
                        faults
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .push(format!("refusal task: {error}"));
                        eprintln!("cap: Bus reply task: {error}");
                    }
                    None => {}
                }
            }
            result = operations.join_next(), if !operations.is_empty() => {
                if let Some(Err(error)) = result {
                    faults
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(error.to_string());
                    eprintln!("cap: Bus work: {error}");
                }
            }
            progress = lane.drive() => match progress {
                Progress::Wake => settings_wake(&mut send, true),
                Progress::UiClosed => break,
                Progress::Updated => {}
            },
            effect = effects.recv() => {
                let Some(effect) = effect else { break };
                match effect {
                    Effect::Respond { id, rc, body } => {
                        if let Some(command) = pending.take(id, client.connection_generation()) {
                            gate.replied();
                            match accepted_tx.try_send((rc, body, command)) {
                                Ok(()) => {}
                                Err(_) => {
                                    faults
                                        .lock()
                                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                                        .push("accepted reply enqueue failed".into());
                                }
                            }
                        }
                    }
                    Effect::Call { service, verb, args, limit, reply } => {
                        // The app's refresh reaction releases the Changed
                        // coalescing flag.
                        if verb == "comp.windows.list" {
                            changed_out = false;
                        }
                        if operations.len() >= OPERATION_CAP {
                            busy_rejected += 1;
                            let _ = reply.send(Err("Bus worker busy".into()));
                        } else {
                            let client = client.clone();
                            operations.spawn(async move {
                                let result = tokio::time::timeout(
                                    limit,
                                    client.call(&service, &verb, args),
                                )
                                .await
                                .map_err(|_| "Bus request timed out".to_string())
                                .and_then(|result| result.map_err(|error| error.to_string()));
                                let _ = reply.send(result);
                            });
                        }
                    }
                    Effect::Delay { duration, reply } => {
                        if operations.len() >= OPERATION_CAP {
                            busy_rejected += 1;
                            drop(reply);
                        } else {
                            operations.spawn(async move {
                                tokio::time::sleep(duration).await;
                                let _ = reply.send(());
                            });
                        }
                    }
                    Effect::Quit => {
                        deadline = Some(Instant::now() + SHUTDOWN_BUDGET);
                        // FIFO: replies accepted before this quit are queued
                        // ahead of it, and the ordered accepted writer puts
                        // every accepted reply — the quit reply above all —
                        // on the wire before the client closes.
                        for (_, rc, body, command) in take_queued_replies(
                            &mut effects,
                            &mut pending,
                            client.connection_generation(),
                        ) {
                            gate.replied();
                            let _ = accepted_tx.try_send((rc, body, command));
                        }
                        break;
                    }
                }
            }
            command = incoming.recv(), if incoming_open => {
                match command {
                    Some(BoundedIncomingEvent::Command(command)) => {
                        // Native settings frames decode before ordinary traffic.
                        if let Some(wake) = lane.delivery(&command) {
                            settings_wake(&mut send, wake);
                            continue;
                        }
                        if command.topic().is_some() || command.command.is_empty() {
                            continue;
                        }
                        if !gate.admit() {
                            if refusals.len() < REFUSAL_CAP {
                                let client = client.clone();
                                refusals.spawn(async move {
                                    client.respond(&command, 10, BUSY_BODY).await
                                });
                            } else {
                                shed_refusals += 1;
                                eprintln!("cap: refusal shed: refusal pool saturated");
                            }
                            continue;
                        }
                        next_id += 1;
                        let delivery = Delivery::Command(Command {
                            id: next_id,
                            verb: command.command.clone(),
                            body: if command.body.trim().is_empty() {
                                "{}".to_string()
                            } else {
                                command.body.clone()
                            },
                            caller_key: caller_key(&command),
                        });
                        pending.insert(next_id, command);
                        gate.accepted();
                        // Reliable: an accepted command awaits its outbox slot.
                        let _ = send.send(delivery).await;
                    }
                    Some(BoundedIncomingEvent::Overflow { .. }) => {
                        settings_wake(&mut send, lane.publish(SettingsEvent::Lost));
                        if !changed_out
                            && send.try_send(Delivery::Changed).is_ok()
                        {
                            changed_out = true;
                        }
                    }
                    None => {
                        incoming_open = false;
                        settings_wake(&mut send, lane.publish(SettingsEvent::Wake));
                    }
                }
            }
            changed = connection.changed(), if connection_open => {
                if changed.is_err() {
                    connection_open = false;
                }
            }
        }
    }
    let deadline = deadline.unwrap_or(Instant::now() + SHUTDOWN_BUDGET);
    let at = || tokio::time::Instant::from_std(deadline);
    // The ordered accepted writer drains the queue, then exits on channel
    // close, against the one shared deadline.
    drop(accepted_tx);
    match tokio::time::timeout_at(at(), accepted_writer).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => faults
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(format!("accepted writer: {error}")),
        Err(_) => faults
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push("accepted reply drain timed out".into()),
    }
    // Every send is followed by its ack before the writer exits, so draining
    // the acks now leaves only replies that never reached the wire.
    while let Ok(()) = ack_rx.try_recv() {
        gate.sent();
    }
    let queued_accepted = gate.queued;
    // Refusals flush within the same budget.
    while !refusals.is_empty() {
        match tokio::time::timeout_at(at(), refusals.join_next()).await {
            Ok(Some(Ok(Ok(())))) => {}
            Ok(Some(Ok(Err(error)))) => faults
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(format!("refusal: {error}")),
            Ok(Some(Err(error))) => faults
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(format!("refusal task: {error}")),
            Ok(None) => break,
            Err(_) => {
                faults
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push("refusal drain timed out".into());
                break;
            }
        }
    }
    refusals.abort_all();
    // Accepted capture restoration and other calls complete within the same
    // budget before being aborted.
    while !operations.is_empty() {
        match tokio::time::timeout_at(at(), operations.join_next()).await {
            Ok(Some(Ok(()))) => {}
            Ok(Some(Err(error))) => faults
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(format!("Bus work: {error}")),
            Ok(None) => break,
            Err(_) => {
                faults
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(format!(
                        "operation drain timed out with {} operations queued",
                        operations.len()
                    ));
                break;
            }
        }
    }
    operations.abort_all();
    // The one handoff permit drains within the same budget.
    if let Some(mut task) = handoff_task.take() {
        if !task.is_finished() {
            match tokio::time::timeout_at(at(), &mut task).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => faults
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(format!("handoff task: {error}")),
                Err(_) => {
                    faults
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push("handoff drain timed out".into());
                    task.abort();
                }
            }
        }
    }
    if let Err(error) = lane.flush_cache(deadline).await {
        faults
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(format!("settings cache: {}: {}", error.code, error.message));
    }
    if tokio::time::timeout_at(at(), client.close()).await.is_err() {
        faults
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push("Bus close timed out".into());
    }
    let faults = Arc::try_unwrap(faults)
        .map(|lock| {
            lock.into_inner()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        })
        .unwrap_or_else(|faults| {
            faults
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        });
    eprintln!(
        "CAP_SHUTDOWN {}",
        json!({
            "faults": faults,
            "queued_accepted": queued_accepted,
            "shed_refusals": shed_refusals,
            "busy_rejected": busy_rejected,
        })
    );
}

/// Drain replies queued ahead of a quit, in FIFO order, fenced to `live`.
fn take_queued_replies(
    effects: &mut tokio::sync::mpsc::UnboundedReceiver<Effect>,
    pending: &mut Pending,
    live: u64,
) -> Vec<(u64, u8, String, IncomingCommand)> {
    let mut replies = Vec::new();
    while let Ok(effect) = effects.try_recv() {
        if let Effect::Respond { id, rc, body } = effect
            && let Some(command) = pending.take(id, live)
        {
            replies.push((id, rc, body, command));
        }
    }
    replies
}

/// The single-instance handoff requests: open the first argv path, then show.
fn forward_requests(paths: &[String]) -> Vec<(&'static str, Value)> {
    let mut requests = Vec::new();
    if let Some(path) = paths.first() {
        requests.push(("cap.open", json!({"path": path})));
    }
    requests.push(("cap.show", json!({})));
    requests
}
async fn forward_async(url: &str, service: &str, paths: &[String]) -> Result<(), String> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let client = NodedClient::connect_anonymous(url)
            .await
            .map_err(|error| error.to_string())?;
        let mut result = Ok(());
        for (verb, args) in forward_requests(paths) {
            match client
                .call_with_headers_raw(service, verb, &BTreeMap::new(), &args.to_string())
                .await
            {
                Ok((0, _, _)) => {}
                Ok((rc, body, _)) => {
                    result = Err(format!("{verb} refused (rc {rc}): {body}"));
                    break;
                }
                Err(error) => {
                    result = Err(format!("{verb} failed: {error}"));
                    break;
                }
            }
        }
        client.close().await;
        result
    })
    .await
    .map_err(|_| "activation handoff timed out".to_owned())?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn handoff_requests_open_the_first_path_then_show() {
        assert_eq!(forward_requests(&[]), vec![("cap.show", json!({}))]);
        assert_eq!(
            forward_requests(&["/tmp/image.png".into()]),
            vec![
                ("cap.open", json!({"path": "/tmp/image.png"})),
                ("cap.show", json!({}))
            ]
        );
    }

    fn command(generation: u64) -> IncomingCommand {
        IncomingCommand {
            generation,
            from: "ctl-90".into(),
            command: "cap.ping".into(),
            id: Some("1".into()),
            args: serde_json::Value::Null,
            body: String::new(),
            headers: BTreeMap::new(),
        }
    }

    #[test]
    fn pending_replies_fence_stale_connection_generations() {
        let mut pending = Pending::default();
        pending.insert(1, command(2));
        pending.insert(3, command(2));
        assert!(pending.take(1, 2).is_some());
        assert!(
            pending.take(3, 1).is_none(),
            "an old token never leaves through a new connection"
        );
        assert!(
            pending.take(3, 2).is_some(),
            "pending survives an attempted stale take and a disconnect"
        );
        assert!(pending.take(9, 2).is_none());
        pending.insert(4, command(0));
        assert!(
            pending.take(4, 0).is_none(),
            "generation-zero replies are refused like the client's own fence"
        );
        assert_eq!(pending.len(), 1);
    }

    #[test]
    fn admission_keeps_the_accepted_sum_fixed_and_never_sheds_accepted() {
        let mut gate = AcceptedGate::default();
        for _ in 0..ACCEPTED_CAP {
            assert!(gate.admit());
            gate.accepted();
        }
        assert!(
            !gate.admit(),
            "the accepted pipeline admits no command beyond its fixed sum"
        );
        // A reply transfers the command from pending to the accepted queue;
        // the sum is unchanged and still saturated.
        gate.replied();
        assert!(!gate.admit());
        // Only a sent (acked) reply frees admission.
        gate.sent();
        assert!(gate.admit());
        gate.accepted();
        assert!(!gate.admit());
    }

    #[test]
    fn quit_drains_accepted_replies_in_fifo_order_and_fences_the_rest() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut pending = Pending::default();
        pending.insert(1, command(2));
        pending.insert(2, command(2));
        pending.insert(3, command(1));
        tx.send(Effect::Respond {
            id: 1,
            rc: 0,
            body: "one".into(),
        })
        .unwrap();
        tx.send(Effect::Respond {
            id: 2,
            rc: 0,
            body: "two".into(),
        })
        .unwrap();
        tx.send(Effect::Quit).unwrap();
        let drained = take_queued_replies(&mut rx, &mut pending, 2);
        let ids: Vec<u64> = drained.iter().map(|(id, ..)| *id).collect();
        assert_eq!(ids, [1, 2], "accepted replies keep their FIFO order");
        assert_eq!(pending.len(), 1);
        // The stale command was fenced, not sent through another generation.
        assert!(pending.take(3, 1).is_some());
    }

    #[test]
    fn caller_keys_follow_editd_rules() {
        let mut headers = BTreeMap::new();
        headers.insert("broker_origin".into(), "local".into());
        assert_eq!(
            caller_key(&IncomingCommand {
                headers,
                ..command(1)
            }),
            "local:ctl-90"
        );
        let mut empty = BTreeMap::new();
        empty.insert("broker_origin".into(), "local".into());
        assert_eq!(
            caller_key(&IncomingCommand {
                from: String::new(),
                headers: empty,
                ..command(1)
            }),
            "anon"
        );
        let mut mesh = BTreeMap::new();
        mesh.insert("broker_origin".into(), "mesh".into());
        mesh.insert("broker_service".into(), "svc".into());
        mesh.insert("broker_peer".into(), "beta".into());
        assert_eq!(
            caller_key(&IncomingCommand {
                headers: mesh,
                ..command(1)
            }),
            "mesh:svc@beta"
        );
        assert_eq!(caller_key(&command(1)), "anon");
    }

    #[test]
    fn handoff_gate_requires_a_typed_name_collision_on_a_never_established_connection() {
        let paths = Some(vec!["/tmp/image.png".into()]);
        assert!(handoff_gate(
            RegistrationRejectionKind::NameTaken,
            0,
            &paths
        ));
        assert!(
            !handoff_gate(RegistrationRejectionKind::Unknown, 0, &paths),
            "an admission refusal never forwards"
        );
        assert!(
            !handoff_gate(RegistrationRejectionKind::NameTaken, 1, &paths),
            "a later generation never forwards"
        );
        assert!(
            !handoff_gate(RegistrationRejectionKind::NameTaken, 0, &None),
            "no handoff payload, no forward"
        );
    }

    #[test]
    fn operation_cap_rejects_calls_busy_and_quit_bounds_stalled_work() {
        let (send, _receive) = mpsc::channel(OUTBOX_CAP);
        let (effects, effects_rx) = tokio::sync::mpsc::unbounded_channel();
        let (ready, ready_rx) = std::sync::mpsc::channel();
        let worker_thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(worker(
                "cap-test".into(),
                "ws://127.0.0.1:1/ws".into(),
                None,
                send,
                effects_rx,
                ready,
            ));
            runtime.shutdown_timeout(Duration::from_millis(100));
        });
        let (client, ui, _bootstrap) = ready_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        let _client = client;
        let _ui = ui;
        // Occupy every operation permit with stalled work.
        for _ in 0..OPERATION_CAP {
            let (reply, _rx) = oneshot::channel();
            effects
                .send(Effect::Delay {
                    duration: Duration::from_secs(60),
                    reply,
                })
                .unwrap();
        }
        let (reply, rx) = oneshot::channel();
        effects
            .send(Effect::Call {
                service: "comp".into(),
                verb: "comp.windows.list".into(),
                args: json!({}),
                limit: Duration::from_secs(5),
                reply,
            })
            .unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(5), rx)
                    .await
                    .unwrap()
                    .expect("the actor must retain and complete the accepted reply channel")
                    .unwrap_err(),
                "Bus worker busy",
                "an exhausted operation pool rejects the call immediately"
            );
        });
        effects.send(Effect::Quit).unwrap();
        worker_thread.join().unwrap();
    }

    #[test]
    fn operation_permits_release_after_a_dynamic_wave() {
        let (send, _receive) = mpsc::channel(OUTBOX_CAP);
        let (effects, effects_rx) = tokio::sync::mpsc::unbounded_channel();
        let (ready, ready_rx) = std::sync::mpsc::channel();
        let worker_thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(worker(
                "cap-test".into(),
                "ws://127.0.0.1:1/ws".into(),
                None,
                send,
                effects_rx,
                ready,
            ));
            runtime.shutdown_timeout(Duration::from_millis(100));
        });
        let (client, ui, _bootstrap) = ready_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        let _client = client;
        let _ui = ui;
        // A wave of short operations saturates the pool...
        for _ in 0..OPERATION_CAP {
            let (reply, _rx) = oneshot::channel();
            effects
                .send(Effect::Delay {
                    duration: Duration::from_millis(300),
                    reply,
                })
                .unwrap();
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (reply, rx) = oneshot::channel();
            effects
                .send(Effect::Call {
                    service: "comp".into(),
                    verb: "comp.windows.list".into(),
                    args: json!({}),
                    limit: Duration::from_secs(5),
                    reply,
                })
                .unwrap();
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(5), rx)
                    .await
                    .unwrap()
                    .expect("the actor must retain and complete the accepted reply channel")
                    .unwrap_err(),
                "Bus worker busy",
                "the wave saturates the pool"
            );
            // ...and the wave's completion releases real permits.
            tokio::time::sleep(Duration::from_millis(600)).await;
            let (reply, rx) = oneshot::channel();
            effects
                .send(Effect::Call {
                    service: "comp".into(),
                    verb: "comp.windows.list".into(),
                    args: json!({}),
                    limit: Duration::from_secs(5),
                    reply,
                })
                .unwrap();
            let error = tokio::time::timeout(Duration::from_secs(5), rx)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err();
            assert_ne!(
                error, "Bus worker busy",
                "the call left the pool instead of a busy rejection"
            );
        });
        effects.send(Effect::Quit).unwrap();
        worker_thread.join().unwrap();
    }
}

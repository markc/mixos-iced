// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Bus thread — ced's `bus.rs` shape (itself the
//! `mixos-term-core/src/bus.rs` shape): a current-thread tokio runtime on its
//! own OS thread holding a [`SupervisedClient`] registered as `dopus` (or
//! `--service NAME`). It forwards `dopus.*` commands to the app through a
//! BOUNDED futures channel (exposed as a `Subscription`, no poll thread)
//! behind a retained outbox: the worker never blocks on GUI backpressure,
//! replaceable edges (lifecycle, settings wakes, forward/theme results, the
//! stopped receipt) coalesce to their latest value, and owned accepted
//! commands/results are retained in bounded FIFO slots — never dropped, and
//! admitted as BUSY refusals once the accepted bound is reached.
//!
//! The windowed app opts into desktop settings ([`spawn_settings`]): the same
//! single worker hosts the settings [`Lane`], a bounded incoming receiver,
//! a nonblocking registration ([`SupervisedClient::start`]) and one shared
//! two-second shutdown budget. Replies, theme applies and the single-instance
//! forward run as BOUNDED owned jobs (a [`tokio::task::JoinSet`] with a
//! semaphore) — never inline awaits that would block the lane, lifecycle or
//! shutdown release — each fenced to the ACCEPT-time connection generation
//! stored with the command (a reconnect never carries an old accepted reply,
//! and a Bus-accepted theme CAS never starts after origin invalidation).
//! Every job outcome (send/call failure, timeout, retirement) is recorded in
//! the authoritative shutdown receipt. Settings deliveries decode before
//! ordinary traffic; overflow publishes [`SettingsEvent::Lost`] (one full
//! read recovers). Fatal registration or a closed receiver/watch never takes
//! the window down — the GUI and its offline lane stay up until it quits.
//!
//! **No broker is not an error**: [`spawn`] (headless) returns
//! [`StartError::Unreachable`]; the windowed app instead sees
//! [`Delivery::Disconnected`] and keeps running — a file manager works
//! standalone.
//!
//! Security posture (see `verbs.rs`): file-mutating verbs do not exist on
//! the Bus surface; `dopus.action` refuses them in the app layer.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use ::bus::native_client::{
    BoundedIncomingEvent, ConnState, IncomingCommand, NodedClient, RegistrationRejectionKind,
    SupervisedClient,
};
use application::iced::futures::channel::mpsc::{Receiver, Sender, channel};
use application::presentation::native::{
    Event as SettingsEvent, Progress, Session, Ui as SettingsUi, Worker as SettingsWorker, bridge,
};

/// Everything the bus thread delivers to the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// A `dopus.*` command.
    Command(Command),
    /// The supervised connection came up (registration succeeded).
    Connected,
    Disconnected,
    /// A settings stage is ready to drain on the UI loop.
    Settings,
    /// The service name is registered (before or with the first Connected).
    Registered,
    /// Registration ended fatally without (or after) owning the name. The
    /// window stays up; an initial [`StartError::NameTaken`] is the
    /// single-instance forward case.
    RegistrationFailed(StartError),
    /// The single-instance forward answered.
    Forwarded(Result<(), String>),
    /// A `theme.*` appearance mutation answered: the applied `(scheme,
    /// mode)` names, or the refusal message for the status line.
    ThemeApplied(Result<(String, String), String>),
    /// The bus thread finished (replies flushed, cache drained); the faults
    /// list is empty on a clean shutdown.
    Stopped { faults: Vec<String> },
}

/// One request to dopus. `id` indexes a pending reply; `None`-reply verbs
/// still get one (an error reply at least).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub id: u64,
    pub verb: String,
    pub body: String,
    /// `local:<from>` / `mesh:<service>@<peer>` / `anon` (editd E0 §4.3).
    pub caller_key: String,
}

/// One fenced appearance mutation, captured on the UI thread from the
/// CONFIRMED consumer read: the binding, incarnation and revision the apply
/// must still match, plus a fresh operation id. The worker validates then
/// applies against `settingsd`; the reply is the real receipt.
#[derive(Debug, Clone)]
pub struct ThemeRequest {
    /// The `dopus.theme.set`/`dopus.action theme.*` command id, when any.
    pub reply_id: Option<u64>,
    pub binding: settings::Binding,
    pub expected_incarnation: String,
    pub expected_revision: settings::Revision,
    pub operation_id: String,
    pub changes: BTreeMap<String, serde_json::Value>,
    /// The requested selection, echoed on a successful apply.
    pub scheme: String,
    pub mode: String,
}

/// Effects the app sends back to the bus thread.
#[derive(Debug, Clone)]
pub enum Effect {
    /// Reply to command `id` with `(rc, body)`.
    Respond { id: u64, rc: u8, body: String },
    /// Single-instance forward of the launch paths to the registered owner.
    ForwardOpen(Vec<String>),
    /// A fenced appearance mutation through `settingsd`.
    ThemeApply(ThemeRequest),
    /// Stop the bus thread (the app is quitting).
    Quit,
}

/// One accepted command: the broker frame plus the connection generation it
/// was ACCEPTED on. Replies and Bus-accepted theme CASes never cross
/// generations — a reconnect retires them instead of sending them on a new
/// socket.
struct Accepted {
    command: IncomingCommand,
    generation: u64,
}

/// The worker's authoritative shutdown receipt, published once after the
/// runtime's own drain completed.
#[derive(Default)]
struct Done {
    finished: bool,
    faults: Vec<String>,
}

/// The handle the app uses to reply / mutate settings / quit.
#[derive(Clone)]
pub struct BusHandle {
    tx: tokio::sync::mpsc::UnboundedSender<Effect>,
    /// Set + notified when the bus thread has finished (replies flushed,
    /// cache drained, client closed) — `wait_done` before process exit
    /// guarantees the last reply reached the wire instead of racing it.
    /// Arc-shared so the handle stays Clone (a raw Receiver is not).
    done: std::sync::Arc<(std::sync::Mutex<Done>, std::sync::Condvar)>,
    client: Option<Arc<SupervisedClient>>,
    settings: Option<SettingsUi<crate::app::Content>>,
    bootstrap: Option<appearance::settings::Prepared>,
}

impl BusHandle {
    /// Exercise the real window command performer without a broker connection.
    #[cfg(test)]
    pub fn response_sink() -> (Self, tokio::sync::mpsc::UnboundedReceiver<Effect>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (
            Self {
                tx,
                done: std::sync::Arc::new((
                    std::sync::Mutex::new(Done {
                        finished: true,
                        faults: Vec::new(),
                    }),
                    std::sync::Condvar::new(),
                )),
                client: None,
                settings: None,
                bootstrap: None,
            },
            rx,
        )
    }

    pub fn take_settings_ui(&mut self) -> Option<SettingsUi<crate::app::Content>> {
        self.settings.take()
    }

    pub fn take_bootstrap(&mut self) -> Option<appearance::settings::Prepared> {
        self.bootstrap.take()
    }

    /// The connection's current sampled generation (settings::native's rule:
    /// a live generation only while Connected).
    pub fn settings_generation(&self) -> Option<u64> {
        self.client
            .as_ref()
            .and_then(|client| settings::native::live_generation(client))
    }

    pub fn registration_generation(&self) -> u64 {
        self.client
            .as_ref()
            .map_or(0, |client| client.connection_generation())
    }

    pub fn connected(&self) -> bool {
        self.client
            .as_ref()
            .is_none_or(|client| settings::native::live_generation(client).is_some())
    }

    pub fn respond(&self, id: u64, rc: u8, body: String) {
        let _ = self.tx.send(Effect::Respond { id, rc, body });
    }

    /// Forward the launch paths to the registered owner (single instance).
    pub fn forward_open(&self, paths: Vec<String>) {
        let _ = self.tx.send(Effect::ForwardOpen(paths));
    }

    /// Send a fenced appearance mutation to `settingsd` on the bus worker.
    pub fn theme_apply(&self, request: ThemeRequest) {
        let _ = self.tx.send(Effect::ThemeApply(request));
    }

    pub fn quit(&self) {
        let _ = self.tx.send(Effect::Quit);
    }

    /// Block until the bus thread finished (bounded) and return the worker's
    /// authoritative shutdown receipt: `Ok(faults)` (empty = clean), or
    /// `Err` when the deadline expired before the thread finished. Call
    /// after [`BusHandle::quit`] and before exiting the process.
    pub fn wait_done(&self, timeout: Duration) -> Result<Vec<String>, String> {
        let (lock, notified) = &*self.done;
        let mut state = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.finished {
            let (next, _) = notified
                .wait_timeout_while(state, timeout, |state| !state.finished)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state = next;
        }
        if state.finished {
            Ok(std::mem::take(&mut state.faults))
        } else {
            Err("Bus shutdown did not complete in time".into())
        }
    }
}

/// Why the Bus could not be started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartError {
    /// Another instance owns the service name (single-instance forward).
    NameTaken,
    /// noded refused registration for another reason (message).
    Rejected(String),
    /// No broker reachable — run windowed without a Bus.
    Unreachable(String),
    /// The local desktop settings binding could not be resolved.
    SettingsBinding(String),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StartError::NameTaken => f.write_str("the service name is already registered"),
            StartError::Rejected(m) => write!(f, "registration refused: {m}"),
            StartError::Unreachable(m) => write!(f, "Bus unreachable: {m}"),
            StartError::SettingsBinding(m) => write!(f, "settings session: {m}"),
        }
    }
}

impl std::error::Error for StartError {}

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

/// Initial connect + register budget.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// The single shutdown budget: accepted jobs, the retained outbox, the
/// settings cache drain and the client close share this ONE deadline.
const SHUTDOWN_BUDGET: Duration = Duration::from_secs(2);
/// The bounded incoming queue (the settingsd/ced shape): overflow drops the
/// oldest frames and publishes one full settings read.
const INCOMING_BOUND: usize = 64;
/// The bounded GUI delivery channel behind the retained outbox.
const DELIVERY_BOUND: usize = 64;
/// Accepted commands awaiting a reply; beyond this the worker admits a BUSY
/// refusal instead of accepting (bounded FIFO, never unbounded).
const PENDING_BOUND: usize = 32;
/// Concurrent owned Bus jobs (replies, theme applies, forwards).
const JOB_BOUND: usize = 16;
/// Retained GUI theme completions; beyond this the worker admits refusal.
const THEME_QUEUE_BOUND: usize = 4;

/// The retained bounded outbox for the GUI channel. The worker never blocks
/// on GUI backpressure: frames are retained here and pumped with
/// [`Sender::try_send`] until the channel takes them. Replaceable edges
/// (lifecycle, settings wakes, forward/theme results, the stopped receipt)
/// coalesce so only the latest value matters; accepted commands take one of
/// [`PENDING_BOUND`] FIFO slots and are never dropped.
#[derive(Default)]
struct Outbox {
    frames: VecDeque<Delivery>,
}

impl Outbox {
    fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    fn commands(&self) -> usize {
        self.frames
            .iter()
            .filter(|frame| matches!(frame, Delivery::Command(_)))
            .count()
    }

    /// Offer one frame. Replaceable edges overwrite their pending twin (one
    /// slot per kind); owned commands take a bounded FIFO slot. `false`
    /// means the frame was NOT retained (command slots exhausted) — the
    /// caller must admit a refusal, never queue unboundedly.
    fn offer(&mut self, delivery: Delivery) -> bool {
        match delivery {
            Delivery::Command(_) => {
                if self.commands() >= PENDING_BOUND {
                    return false;
                }
                self.frames.push_back(delivery);
                true
            }
            edge => {
                let kind = edge_kind(&edge);
                self.frames.retain(|frame| edge_kind(frame) != kind);
                self.frames.push_back(edge);
                true
            }
        }
    }
}

/// The coalescing class of a frame: lifecycle edges share one slot; every
/// other replaceable edge keeps its own.
fn edge_kind(delivery: &Delivery) -> std::mem::Discriminant<Delivery> {
    match delivery {
        Delivery::Connected | Delivery::Disconnected => {
            std::mem::discriminant(&Delivery::Connected)
        }
        other => std::mem::discriminant(other),
    }
}

/// Push retained frames into the GUI channel without blocking. A full
/// channel leaves frames retained for the next pump (capacity polling is
/// cancel-safe); a closed channel clears the outbox — the GUI is gone.
fn pump(dtx: &Sender<Delivery>, outbox: &mut Outbox) {
    while let Some(frame) = outbox.frames.pop_front() {
        match dtx.try_send(frame) {
            Ok(()) => {}
            Err(error) if error.is_full() => {
                outbox.frames.push_front(error.into_inner());
                return;
            }
            Err(_) => {
                outbox.frames.clear();
                return;
            }
        }
    }
}

/// Offer a frame into the retained outbox and pump the channel. `false`
/// means the frame was not retained (bounded slots exhausted).
fn deliver(dtx: &Sender<Delivery>, outbox: &mut Outbox, delivery: Delivery) -> bool {
    let retained = outbox.offer(delivery);
    pump(dtx, outbox);
    retained
}

/// One owned job's outcome: an optional GUI frame the worker must deliver,
/// plus an optional fault for the shutdown receipt. Every send/call failure,
/// timeout and retirement is recorded — a clean receipt is never faked.
type JobOutcome = (Option<Delivery>, Option<String>);

fn spawn_busy(
    jobs: &mut tokio::task::JoinSet<JobOutcome>,
    permit: tokio::sync::OwnedSemaphorePermit,
    client: Arc<SupervisedClient>,
    command: IncomingCommand,
) {
    jobs.spawn(async move {
        let _permit = permit;
        match tokio::time::timeout(
            SHUTDOWN_BUDGET,
            client.respond(
                &command,
                10,
                "{\"error_code\":\"BUSY\",\"message\":\"too many pending commands\"}",
            ),
        )
        .await
        {
            Ok(Ok(())) => Ok((None, None)),
            Ok(Err(error)) => Ok((None, Some(format!("Bus refusal: {error}")))),
            Err(_) => Ok((None, Some("Bus refusal timed out".into()))),
        }
    });
}

fn spawn_reply(
    jobs: &mut tokio::task::JoinSet<JobOutcome>,
    permit: tokio::sync::OwnedSemaphorePermit,
    client: Arc<SupervisedClient>,
    accepted: Accepted,
    rc: u8,
    body: String,
) {
    jobs.spawn(async move {
        let _permit = permit;
        // ACCEPT-time fence: the reply may only travel on the socket its
        // command arrived on; a reconnect retires it.
        if client.is_connected() && client.connection_generation() == accepted.generation {
            match tokio::time::timeout(SHUTDOWN_BUDGET, client.respond(&accepted.command, rc, &body))
                .await
            {
                Ok(Ok(())) => Ok((None, None)),
                Ok(Err(error)) => Ok((None, Some(format!("Bus reply: {error}")))),
                Err(_) => Ok((None, Some("Bus reply timed out".into()))),
            }
        } else {
            Ok((
                None,
                Some("accepted reply retired: its connection generation moved".into()),
            ))
        }
    });
}

fn spawn_theme(
    jobs: &mut tokio::task::JoinSet<JobOutcome>,
    permit: tokio::sync::OwnedSemaphorePermit,
    client: Arc<SupervisedClient>,
    request: ThemeRequest,
    accepted: Option<Accepted>,
) {
    jobs.spawn(async move {
        let _permit = permit;
        // Origin fence: a Bus-accepted CAS never starts after its
        // connection generation moved.
        if let Some(accepted) = &accepted
            && client.connection_generation() != accepted.generation
        {
            return Ok((
                Some(Delivery::ThemeApplied(Err(
                    "appearance request retired: its connection generation moved".into(),
                ))),
                None,
            ));
        }
        // The validated compare-and-set against settingsd is the authority's
        // own fence; it is never a local authority.
        let (rc, body, result) = theme_apply(&client, &request).await;
        let mut fault = None;
        if let Some(accepted) = accepted
            && client.is_connected()
            && client.connection_generation() == accepted.generation
        {
            match tokio::time::timeout(
                SHUTDOWN_BUDGET,
                client.respond(&accepted.command, rc, &body),
            )
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(error)) => fault = Some(format!("Bus reply: {error}")),
                Err(_) => fault = Some("Bus reply timed out".into()),
            }
        }
        Ok((Some(Delivery::ThemeApplied(result)), fault))
    });
}

fn spawn_forward(
    jobs: &mut tokio::task::JoinSet<JobOutcome>,
    permit: tokio::sync::OwnedSemaphorePermit,
    url: &str,
    service: &str,
    paths: Vec<String>,
) {
    let url = url.to_owned();
    let service = service.to_owned();
    jobs.spawn(async move {
        let _permit = permit;
        let result = forward_open_async(&url, &service, &paths).await;
        Ok((Some(Delivery::Forwarded(result)), None))
    });
}

/// Admit retained accepted work into freed job slots, FIFO: accepted replies
/// first, then the one pending forward, then theme completions. BUSY noise
/// is never admitted ahead of owned accepted work.
#[allow(clippy::too_many_arguments)]
fn admit(
    jobs: &mut tokio::task::JoinSet<JobOutcome>,
    permits: &Arc<tokio::sync::Semaphore>,
    client: &Arc<SupervisedClient>,
    url: &str,
    service: &str,
    pending_replies: &mut VecDeque<(Accepted, u8, String)>,
    pending_theme: &mut VecDeque<(ThemeRequest, Option<Accepted>)>,
    pending_forward: &mut Option<Vec<String>>,
) {
    while let Some((accepted, rc, body)) = pending_replies.pop_front() {
        let Ok(permit) = permits.clone().try_acquire_owned() else {
            pending_replies.push_front((accepted, rc, body));
            return;
        };
        spawn_reply(jobs, permit, client.clone(), accepted, rc, body);
    }
    if let Some(paths) = pending_forward.clone() {
        let Ok(permit) = permits.clone().try_acquire_owned() else {
            return;
        };
        if pending_forward.take().is_some() {
            spawn_forward(jobs, permit, url, service, paths);
        }
    }
    while let Some((request, accepted)) = pending_theme.pop_front() {
        let Ok(permit) = permits.clone().try_acquire_owned() else {
            pending_theme.push_front((request, accepted));
            return;
        };
        spawn_theme(jobs, permit, client.clone(), request, accepted);
    }
}

/// Start the bus thread registered as `service`, connecting to `url`
/// (`::bus::client_helpers::resolve_noded_url()` unless
/// `--noded-url` overrode it). The headless path: registration is awaited
/// with a timeout and its refusal is a hard error.
pub fn spawn(
    service: &str,
    url: &str,
) -> Result<(BusHandle, Receiver<Delivery>), StartError> {
    let (dtx, drx) = channel(DELIVERY_BOUND);
    let (etx, erx) = tokio::sync::mpsc::unbounded_channel();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let done = std::sync::Arc::new((
        std::sync::Mutex::new(Done::default()),
        std::sync::Condvar::new(),
    ));
    let done_thread = std::sync::Arc::clone(&done);
    let service = service.to_string();
    let url = url.to_owned();
    std::thread::Builder::new()
        .name(format!("{service}-bus"))
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ =
                        ready_tx.send(Err(StartError::Unreachable(format!("Bus runtime: {e}"))));
                    return;
                }
            };
            runtime.block_on(run(service, url, dtx, erx, ready_tx));
            // Done is emitted only after the runtime's own drain finished.
            let (lock, notified) = &*done_thread;
            let mut done = lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            done.finished = true;
            notified.notify_all();
        })
        .map_err(|e| StartError::Unreachable(format!("Bus thread: {e}")))?;
    match ready_rx.recv() {
        Ok(Ok(())) => Ok((
            BusHandle {
                tx: etx,
                done,
                client: None,
                settings: None,
                bootstrap: None,
            },
            drx,
        )),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(StartError::Unreachable("the Bus thread exited".into())),
    }
}

/// The windowed path: one supervised connection with a nonblocking
/// registration, a bounded incoming receiver and the settings lane on the
/// SAME worker. Readiness means "the worker is up", never "registered" —
/// registration and refusal arrive as [`Delivery`]s.
pub fn spawn_settings(
    service: &str,
    url: &str,
) -> Result<(BusHandle, Receiver<Delivery>), StartError> {
    let (dtx, drx) = channel(DELIVERY_BOUND);
    let (etx, erx) = tokio::sync::mpsc::unbounded_channel();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let done = std::sync::Arc::new((
        std::sync::Mutex::new(Done::default()),
        std::sync::Condvar::new(),
    ));
    let done_thread = std::sync::Arc::clone(&done);
    let service = service.to_string();
    let url = url.to_owned();
    std::thread::Builder::new()
        .name(format!("{service}-bus"))
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ =
                        ready_tx.send(Err(StartError::Unreachable(format!("Bus runtime: {e}"))));
                    return;
                }
            };
            let faults = runtime.block_on(run_settings(service, url, dtx, erx, ready_tx));
            // A timed-out spawn_blocking cache operation cannot be aborted.
            // Do not turn the bounded worker shutdown into an unbounded
            // runtime Drop. Done is emitted only after this owned drain.
            runtime.shutdown_timeout(Duration::from_millis(100));
            let (lock, notified) = &*done_thread;
            let mut done = lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            done.finished = true;
            done.faults = faults;
            notified.notify_all();
        })
        .map_err(|e| StartError::Unreachable(format!("Bus thread: {e}")))?;
    match ready_rx.recv() {
        Ok(Ok((client, settings, bootstrap))) => Ok((
            BusHandle {
                tx: etx,
                done,
                client: Some(client),
                settings: Some(settings),
                bootstrap: Some(bootstrap),
            },
            drx,
        )),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(StartError::Unreachable("the Bus thread exited".into())),
    }
}

async fn run(
    service: String,
    url: String,
    dtx: Sender<Delivery>,
    mut erx: tokio::sync::mpsc::UnboundedReceiver<Effect>,
    ready: std::sync::mpsc::Sender<Result<(), StartError>>,
) {
    let connect = SupervisedClient::connect_options(&service, &url)
        .fatal_on_registration_rejection(true)
        .connect();
    let client = match tokio::time::timeout(CONNECT_TIMEOUT, connect).await {
        Ok(Ok(c)) => Arc::new(c),
        Ok(Err(e)) => {
            // The TYPED rejection classification: a name collision is
            // NameTaken; unknown/admission refusals stay refusals.
            let err = match e.registration_rejection_typed() {
                Some(rejection) if rejection.kind() == RegistrationRejectionKind::NameTaken => {
                    StartError::NameTaken
                }
                Some(_) => StartError::Rejected(format!("registration refused: {e}")),
                None => StartError::Unreachable(e.to_string()),
            };
            let _ = ready.send(Err(err));
            return;
        }
        Err(_) => {
            let _ = ready.send(Err(StartError::Unreachable("connect timed out".into())));
            return;
        }
    };
    let Some(mut incoming) = client.incoming() else {
        let _ = ready.send(Err(StartError::Unreachable("no incoming channel".into())));
        return;
    };
    let mut state = client.subscribe_state();
    let _ = ready.send(Ok(()));

    // Commands awaiting a reply from the app (bounded, PENDING_BOUND).
    let mut commands: HashMap<u64, IncomingCommand> = HashMap::new();
    let mut next_command = 0u64;
    let mut outbox = Outbox::default();
    loop {
        pump(&dtx, &mut outbox);
        tokio::select! {
            cmd = incoming.recv() => {
                let Some(cmd) = cmd else { break };
                if cmd.topic().is_some() {
                    continue;
                }
                if cmd.command.is_empty() {
                    continue;
                }
                if commands.len() >= PENDING_BOUND {
                    let _ = tokio::time::timeout(
                        SHUTDOWN_BUDGET,
                        client.respond(&cmd, 10, "{\"error_code\":\"BUSY\",\"message\":\"too many pending commands\"}"),
                    )
                    .await;
                    continue;
                }
                next_command += 1;
                let delivery = Delivery::Command(Command {
                    id: next_command,
                    verb: cmd.command.clone(),
                    body: if cmd.body.trim().is_empty() { "{}".to_string() } else { cmd.body.clone() },
                    caller_key: caller_key(&cmd),
                });
                if deliver(&dtx, &mut outbox, delivery) {
                    commands.insert(next_command, cmd);
                } else {
                    // Outbox slots exhausted: admit refusal, never queue
                    // beyond the accepted bound.
                    let _ = tokio::time::timeout(
                        SHUTDOWN_BUDGET,
                        client.respond(&cmd, 10, "{\"error_code\":\"BUSY\",\"message\":\"too many pending commands\"}"),
                    )
                    .await;
                }
            }
            effect = erx.recv() => {
                let Some(effect) = effect else { break };
                match effect {
                    Effect::Respond { id, rc, body } => {
                        if let Some(cmd) = commands.remove(&id) {
                            // Awaited INLINE, not spawned: a reply —
                            // `dopus.quit`'s above all — must be on the wire
                            // before this loop can break (Effect::Quit) and
                            // close the client under it. The 2 s cap keeps a
                            // wedged broker from hanging the thread.
                            let _ =
                                tokio::time::timeout(SHUTDOWN_BUDGET, client.respond(&cmd, rc, &body)).await;
                        }
                    }
                    Effect::Quit => {
                        // A quit racing its own reply must not swallow it:
                        // headless replies-then-quits in one breath, and
                        // select! may pick this arm while the Respond is
                        // still queued — drain every pending reply (each
                        // awaited inline, same 2 s cap) before breaking.
                        while let Ok(effect) = erx.try_recv() {
                            if let Effect::Respond { id, rc, body } = effect
                                && let Some(cmd) = commands.remove(&id)
                            {
                                let _ = tokio::time::timeout(
                                    SHUTDOWN_BUDGET,
                                    client.respond(&cmd, rc, &body),
                                )
                                .await;
                            }
                        }
                        break;
                    }
                    Effect::ForwardOpen(_) | Effect::ThemeApply(_) => {
                        unreachable!("the headless path never forwards or applies themes")
                    }
                }
            }
            changed = state.changed() => {
                if changed.is_err() {
                    break;
                }
                let edge = match *state.borrow_and_update() {
                    ConnState::Connected => Some(Delivery::Connected),
                    ConnState::Disconnected => Some(Delivery::Disconnected),
                    ConnState::ShuttingDown | ConnState::Fatal => {
                        break;
                    }
                    ConnState::Connecting => None,
                };
                if let Some(edge) = edge {
                    deliver(&dtx, &mut outbox, edge);
                }
            }
        }
    }
    let _ = tokio::time::timeout(SHUTDOWN_BUDGET, client.close()).await;
}

type Ready = Result<
    (
        Arc<SupervisedClient>,
        SettingsUi<crate::app::Content>,
        appearance::settings::Prepared,
    ),
    StartError,
>;

fn settings_wake(dtx: &Sender<Delivery>, outbox: &mut Outbox, needed: bool) {
    if needed {
        deliver(dtx, outbox, Delivery::Settings);
    }
}

/// The typed registration classification for the supervised client: a name
/// collision counts as the single-instance case ONLY before the first
/// successful registration (generation zero); unknown/admission refusals and
/// post-registration failures stay refusals/notice.
fn registration_error(client: &SupervisedClient) -> StartError {
    let rejection = client.registration_rejection();
    match rejection.map(|rejection| (rejection.kind(), rejection.rc, rejection.message)) {
        Some((RegistrationRejectionKind::NameTaken, ..)) if client.connection_generation() == 0 => {
            StartError::NameTaken
        }
        Some((_, rc, message)) => StartError::Rejected(format!("rc {rc}: {message}")),
        None => StartError::Unreachable("connection stopped".into()),
    }
}

async fn run_settings(
    service: String,
    url: String,
    dtx: Sender<Delivery>,
    mut erx: tokio::sync::mpsc::UnboundedReceiver<Effect>,
    ready: std::sync::mpsc::Sender<Ready>,
) -> Vec<String> {
    let binding = match settings::session::binding() {
        Ok(binding) => binding,
        Err(error) => {
            let _ = ready.send(Err(StartError::SettingsBinding(error.message)));
            return Vec::new();
        }
    };
    let consumer = match settings::consumer::Consumer::for_app(binding, "dopus") {
        Ok(consumer) => consumer,
        Err(error) => {
            let _ = ready.send(Err(StartError::SettingsBinding(error.message)));
            return Vec::new();
        }
    };
    // The generic bootstrap precedes any installed-font I/O: no font
    // discovery happens before the UI is ready.
    let bootstrap = match appearance::settings::bootstrap() {
        Ok(bootstrap) => bootstrap,
        Err(error) => {
            let _ = ready.send(Err(StartError::SettingsBinding(error.message)));
            return Vec::new();
        }
    };
    let client = Arc::new(
        SupervisedClient::connect_options(&service, &url)
            .fatal_on_registration_rejection(true)
            .bounded_incoming(INCOMING_BOUND)
            .start(),
    );
    let Some(mut incoming) = client.incoming_bounded() else {
        let _ = ready.send(Err(StartError::Unreachable("no incoming channel".into())));
        return Vec::new();
    };
    let mut state = client.subscribe_state();
    let worker = match crate::dirs::AppDirs::resolve(crate::dirs::COMPONENT) {
        Some(dirs) => SettingsWorker::offline_with_cache(
            dirs.settings_cache_dir(),
            crate::app::Content::build,
        ),
        None => SettingsWorker::offline(crate::app::Content::build),
    };
    let (ui, mut lane) = bridge(Session::new(consumer), worker);
    let mut outbox = Outbox::default();
    settings_wake(&dtx, &mut outbox, lane.connect(Arc::clone(&client)));
    if ready
        .send(Ok((Arc::clone(&client), ui, bootstrap)))
        .is_err()
    {
        let _ = client.close().await;
        return Vec::new();
    }
    // Font files load on the existing worker, after UI readiness and before
    // the lane may prepare a checked presentation.
    if let Err(error) = appearance::fonts::register_installed() {
        tracing::warn!(%error, "DOpus static assets unavailable");
    }

    let mut commands: HashMap<u64, Accepted> = HashMap::new();
    let mut next_command = 0u64;
    let mut incoming_open = true;
    let mut connection_open = true;
    let mut lifecycle = None;
    let mut registered = client.connection_generation() > 0;
    if registered {
        deliver(&dtx, &mut outbox, Delivery::Registered);
    }
    // Bounded owned Bus jobs: replies, theme applies and forwards run here
    // instead of blocking the select loop, each fenced to the ACCEPT-time
    // connection generation. Retained accepted work waits in FIFO queues
    // until a slot frees — it is never dropped.
    let mut jobs = tokio::task::JoinSet::new();
    let job_permits = Arc::new(tokio::sync::Semaphore::new(JOB_BOUND));
    let mut pending_replies: VecDeque<(Accepted, u8, String)> = VecDeque::new();
    let mut pending_theme: VecDeque<(ThemeRequest, Option<Accepted>)> = VecDeque::new();
    let mut pending_forward: Option<Vec<String>> = None;
    let mut faults = Vec::new();
    loop {
        // Sample once before waiting: a fast registration may have completed
        // before the first select. Actual generation decides ownership — a
        // queued edge cannot authorise a forward after a real refusal.
        let now = *state.borrow_and_update();
        if lifecycle != Some(now) {
            lifecycle = Some(now);
            settings_wake(&dtx, &mut outbox, lane.publish(SettingsEvent::Wake));
            match now {
                ConnState::Connected => {
                    if !registered {
                        registered = true;
                        deliver(&dtx, &mut outbox, Delivery::Registered);
                    }
                    deliver(&dtx, &mut outbox, Delivery::Connected);
                }
                // Fatal/closed keep the GUI alive: the window runs offline
                // with its retained look until it quits.
                ConnState::Fatal | ConnState::ShuttingDown => {
                    deliver(&dtx, &mut outbox, Delivery::RegistrationFailed(registration_error(
                        &client,
                    )));
                }
                ConnState::Disconnected => {
                    deliver(&dtx, &mut outbox, Delivery::Disconnected);
                }
                ConnState::Connecting => {}
            }
        }
        // Retained frames and retained accepted work first: backpressure
        // retries here instead of ever blocking the loop.
        pump(&dtx, &mut outbox);
        admit(
            &mut jobs,
            &job_permits,
            &client,
            &url,
            &service,
            &mut pending_replies,
            &mut pending_theme,
            &mut pending_forward,
        );
        tokio::select! {
            progress = lane.drive() => match progress {
                Progress::Wake => settings_wake(&dtx, &mut outbox, true),
                Progress::UiClosed => break,
                Progress::Updated => {}
            },
            result = jobs.join_next(), if !jobs.is_empty() => {
                match result {
                    Some(Ok((frame, fault))) => {
                        if let Some(frame) = frame {
                            deliver(&dtx, &mut outbox, frame);
                        }
                        if let Some(fault) = fault {
                            faults.push(fault);
                        }
                    }
                    Some(Err(error)) => faults.push(format!("Bus job: {error}")),
                    None => {}
                }
                // A slot freed: admit retained accepted work in FIFO order.
                admit(
                    &mut jobs,
                    &job_permits,
                    &client,
                    &url,
                    &service,
                    &mut pending_replies,
                    &mut pending_theme,
                    &mut pending_forward,
                );
            }
            cmd = incoming.recv(), if incoming_open => {
                let cmd = match cmd {
                    Some(BoundedIncomingEvent::Command(cmd)) => cmd,
                    Some(BoundedIncomingEvent::Overflow { dropped }) => {
                        tracing::warn!(dropped, "dopus: Bus deliveries overflowed; one full settings read recovers");
                        settings_wake(&dtx, &mut outbox, lane.publish(SettingsEvent::Lost));
                        continue;
                    }
                    None => {
                        incoming_open = false;
                        settings_wake(&dtx, &mut outbox, lane.publish(SettingsEvent::Wake));
                        continue;
                    }
                };
                // Settings deliveries decode BEFORE ordinary traffic: a
                // settings frame never waits behind queued commands.
                if let Some(wake) = lane.delivery(&cmd) {
                    settings_wake(&dtx, &mut outbox, wake);
                    continue;
                }
                if cmd.topic().is_some() {
                    continue; // no ordinary topics: the file model owns no subscriptions
                }
                if cmd.command.is_empty() {
                    continue;
                }
                if commands.len() >= PENDING_BOUND {
                    let Ok(permit) = job_permits.clone().try_acquire_owned() else {
                        tracing::warn!("dopus: Bus job capacity exhausted; BUSY refusal dropped");
                        continue;
                    };
                    spawn_busy(&mut jobs, permit, client.clone(), cmd);
                    continue;
                }
                next_command += 1;
                // The ACCEPT-time generation travels with the command: its
                // reply may never cross to a newer socket.
                let generation = client.connection_generation();
                let delivery = Delivery::Command(Command {
                    id: next_command,
                    verb: cmd.command.clone(),
                    body: if cmd.body.trim().is_empty() { "{}".to_string() } else { cmd.body.clone() },
                    caller_key: caller_key(&cmd),
                });
                if deliver(&dtx, &mut outbox, delivery) {
                    commands.insert(
                        next_command,
                        Accepted {
                            command: cmd,
                            generation,
                        },
                    );
                } else {
                    // Outbox slots exhausted: admit refusal, never queue
                    // beyond the accepted bound.
                    let Ok(permit) = job_permits.clone().try_acquire_owned() else {
                        tracing::warn!("dopus: Bus job capacity exhausted; BUSY refusal dropped");
                        continue;
                    };
                    spawn_busy(&mut jobs, permit, client.clone(), cmd);
                }
            }
            effect = erx.recv() => {
                let Some(effect) = effect else { break };
                match effect {
                    Effect::Respond { id, rc, body } => {
                        let Some(accepted) = commands.remove(&id) else { continue };
                        let Ok(permit) = job_permits.clone().try_acquire_owned() else {
                            // Capacity exhausted: retain the accepted reply
                            // (FIFO) until a slot frees; never drop it.
                            pending_replies.push_back((accepted, rc, body));
                            continue;
                        };
                        spawn_reply(&mut jobs, permit, client.clone(), accepted, rc, body);
                    }
                    Effect::ForwardOpen(paths) => {
                        // Generation zero only: once registered, this
                        // instance owns the name and forwards nothing.
                        if client.connection_generation() != 0 {
                            continue;
                        }
                        let Ok(permit) = job_permits.clone().try_acquire_owned() else {
                            if pending_forward.is_none() {
                                pending_forward = Some(paths);
                            } else {
                                deliver(&dtx, &mut outbox, Delivery::Forwarded(Err(
                                    "a forward is already pending".into(),
                                )));
                            }
                            continue;
                        };
                        spawn_forward(&mut jobs, permit, &url, &service, paths);
                        continue;
                    }
                    Effect::ThemeApply(request) => {
                        let accepted = request.reply_id.and_then(|id| commands.remove(&id));
                        let Ok(permit) = job_permits.clone().try_acquire_owned() else {
                            if pending_theme.len() < THEME_QUEUE_BOUND {
                                pending_theme.push_back((request, accepted));
                            } else {
                                // Admit refusal: the appearance queue is
                                // exhausted, never silently vanished.
                                deliver(&dtx, &mut outbox, Delivery::ThemeApplied(Err(
                                    "appearance request queue exhausted".into(),
                                )));
                                if let Some(accepted) = accepted {
                                    pending_replies.push_back((
                                        accepted,
                                        10,
                                        "{\"error_code\":\"BUSY\",\"message\":\"appearance queue exhausted\"}".into(),
                                    ));
                                }
                            }
                            continue;
                        };
                        spawn_theme(&mut jobs, permit, client.clone(), request, accepted);
                    }
                    Effect::Quit => break,
                }
            }
            changed = state.changed(), if connection_open => {
                if changed.is_err() {
                    connection_open = false;
                    settings_wake(&dtx, &mut outbox, lane.publish(SettingsEvent::Wake));
                }
            }
        }
    }
    // ONE total shutdown budget: admit retained accepted work, drain every
    // job (replies, theme applies, forwards), the retained outbox, the
    // settings cache, then the client close — all against this deadline.
    let deadline = std::time::Instant::now() + SHUTDOWN_BUDGET;
    let deadline_at = tokio::time::Instant::from_std(deadline);
    admit(
        &mut jobs,
        &job_permits,
        &client,
        &url,
        &service,
        &mut pending_replies,
        &mut pending_theme,
        &mut pending_forward,
    );
    while !jobs.is_empty() {
        match tokio::time::timeout_at(deadline_at, jobs.join_next()).await {
            Ok(Some(Ok((frame, fault)))) => {
                if let Some(frame) = frame {
                    deliver(&dtx, &mut outbox, frame);
                }
                if let Some(fault) = fault {
                    faults.push(fault);
                }
            }
            Ok(Some(Err(error))) => faults.push(format!("Bus job: {error}")),
            Ok(None) => break,
            Err(_) => {
                faults.push("Bus job drain timed out".into());
                jobs.abort_all();
                break;
            }
        }
    }
    // The retained outbox under the same deadline (bounded retries: a full
    // channel with a blocked GUI is reported, never spun on).
    while !outbox.is_empty() && tokio::time::Instant::now() < deadline_at {
        pump(&dtx, &mut outbox);
        if !outbox.is_empty() {
            tokio::task::yield_now().await;
        }
    }
    if !outbox.is_empty() {
        faults.push("GUI outbox drain timed out".into());
    }
    if let Err(error) = lane.flush_cache(deadline).await {
        faults.push(format!("settings cache: {}: {}", error.code, error.message));
    }
    if tokio::time::timeout_at(deadline_at, client.close()).await.is_err() {
        faults.push("Bus close timed out".into());
    }
    deliver(&dtx, &mut outbox, Delivery::Stopped {
        faults: faults.clone(),
    });
    pump(&dtx, &mut outbox);
    faults
}

async fn call_settings(
    client: &SupervisedClient,
    verb: &str,
    body: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let reply = client
        .call_typed("settingsd", verb, body)
        .await
        .map_err(|error| error.to_string())?;
    match reply {
        bus::PortReply::Ok { value, .. } => Ok(value),
        bus::PortReply::AppError { message, .. } => Err(message),
    }
}

/// The refusal `settingsd` returned, mapped to dopus's error vocabulary.
/// `status` is the authority's structured status field; `body` may carry a
/// message.
fn settings_refusal(status: &str, value: &serde_json::Value) -> (String, String) {
    let message = value
        .get("message")
        .and_then(|m| m.as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| match status {
            "conflict" => format!(
                "settings conflict: the desktop revision moved to {}",
                value
                    .get("revision")
                    .map(|revision| revision.to_string())
                    .unwrap_or_else(|| "?".to_owned())
            ),
            _ => status.to_owned(),
        });
    let code = match status {
        "conflict" => "CONFLICT",
        "validation_failed" => "INVALID_ARGUMENT",
        _ => "INTERNAL",
    };
    (code.to_owned(), message)
}

/// Map an authority reply error to the app's refusal vocabulary: a
/// structured refusal body keeps its status; anything else (transport,
/// timeout) is UNAVAILABLE.
fn authority_refusal(message: &str) -> (String, String) {
    match serde_json::from_str::<serde_json::Value>(message) {
        Ok(value) => {
            let status = value
                .get("status")
                .and_then(|s| s.as_str())
                .unwrap_or("unknown");
            settings_refusal(status, &value)
        }
        Err(_) => ("UNAVAILABLE".to_owned(), message.to_owned()),
    }
}

/// Validate the fenced appearance candidate against `settingsd`, then apply
/// it. Both calls share ONE two-second deadline. The result is the real
/// receipt: `changed`/`unchanged`/`replayed` echo the requested selection,
/// refusals surface verbatim — never a faked success, and never a local
/// authority.
async fn theme_apply(
    client: &SupervisedClient,
    request: &ThemeRequest,
) -> (u8, String, Result<(String, String), String>) {
    let refused = |code: &str, message: String| {
        (
            10,
            serde_json::to_string(&crate::verbs::Refusal {
                error_code: code.to_owned(),
                message: message.clone(),
                reason: None,
            })
            .unwrap_or_default(),
            Err(message),
        )
    };
    let body = serde_json::json!({
        "binding": request.binding,
        "expected_incarnation": request.expected_incarnation,
        "expected_revision": request.expected_revision,
        "operation_id": request.operation_id,
        "changes": request.changes,
        "reset": [],
    });
    let deadline = tokio::time::Instant::now() + SHUTDOWN_BUDGET;
    let outcome: Result<(), (String, String)> = match tokio::time::timeout_at(deadline, async {
        let validated = call_settings(client, "settings.validate", body.clone())
            .await
            .map_err(|message| authority_refusal(&message))?;
        let status = validated.get("status").and_then(|s| s.as_str());
        if status != Some("valid") {
            return Err(settings_refusal(status.unwrap_or("validation_failed"), &validated));
        }
        let applied = call_settings(client, "settings.apply", body)
            .await
            .map_err(|message| authority_refusal(&message))?;
        let status = applied.get("status").and_then(|s| s.as_str());
        match status {
            Some("changed" | "unchanged") => Ok(()),
            _ => Err(settings_refusal(status.unwrap_or("apply_refused"), &applied)),
        }
    })
    .await
    {
        Ok(result) => result,
        Err(_) => Err((
            "UNAVAILABLE".to_owned(),
            "the settings authority did not answer in time".to_owned(),
        )),
    };
    match outcome {
        Ok(()) => (
            0,
            serde_json::to_string(&crate::verbs::ThemeSetReply {
                scheme: request.scheme.clone(),
                mode: request.mode.clone(),
            })
            .unwrap_or_default(),
            Ok((request.scheme.clone(), request.mode.clone())),
        ),
        Err((code, message)) => refused(&code, message),
    }
}

async fn forward_open_async(url: &str, service: &str, paths: &[String]) -> Result<(), String> {
    let client = tokio::time::timeout(Duration::from_secs(5), NodedClient::connect_anonymous(url))
        .await
        .map_err(|_| "forward connection timed out".to_string())?
        .map_err(|error| error.to_string())?;
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        let ping = client
            .call_with_headers_raw(service, "dopus.ping", &BTreeMap::new(), "{}")
            .await
            .map_err(|error| error.to_string())?;
        if ping.0 != 0 {
            return Err(format!("dopus.ping refused: rc {}", ping.0));
        }
        if paths.is_empty() {
            return Ok(());
        }
        let reply = client
            .call_with_headers_raw(
                service,
                "dopus.open",
                &BTreeMap::new(),
                &serde_json::json!({ "paths": paths }).to_string(),
            )
            .await
            .map_err(|error| error.to_string())?;
        if reply.0 == 0 {
            Ok(())
        } else {
            Err(format!("dopus.open refused: rc {}: {}", reply.0, reply.1))
        }
    })
    .await
    .unwrap_or_else(|_| Err("forward request timed out".into()));
    let _ = tokio::time::timeout(Duration::from_millis(500), client.close()).await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use application::iced::futures::StreamExt;
    use std::future::Future;

    fn cmd(from: &str, headers: &[(&str, &str)]) -> IncomingCommand {
        IncomingCommand {
            generation: 0,
            from: from.to_string(),
            command: "dopus.ping".into(),
            id: Some("1".into()),
            args: serde_json::Value::Null,
            body: String::new(),
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    fn command(id: u64) -> Delivery {
        Delivery::Command(Command {
            id,
            verb: "dopus.ping".into(),
            body: "{}".into(),
            caller_key: "anon".into(),
        })
    }

    #[test]
    fn caller_keys_follow_editd_rules() {
        assert_eq!(
            caller_key(&cmd("ctl-90", &[("broker_origin", "local")])),
            "local:ctl-90"
        );
        assert_eq!(caller_key(&cmd("", &[("broker_origin", "local")])), "anon");
        assert_eq!(
            caller_key(&cmd(
                "x",
                &[
                    ("broker_origin", "mesh"),
                    ("broker_service", "svc"),
                    ("broker_peer", "beta")
                ]
            )),
            "mesh:svc@beta"
        );
        assert_eq!(caller_key(&cmd("x", &[])), "anon");
    }

    #[test]
    fn the_worker_done_receipt_is_authoritative() {
        let (handle, _effects) = BusHandle::response_sink();
        assert_eq!(handle.wait_done(Duration::from_millis(10)), Ok(Vec::new()));
    }

    #[test]
    fn outbox_coalesces_replaceable_edges_and_keeps_owned_frames() {
        let (tx, _rx) = channel(DELIVERY_BOUND);
        let mut outbox = Outbox::default();
        assert!(deliver(&tx, &mut outbox, Delivery::Connected));
        assert!(deliver(&tx, &mut outbox, Delivery::Disconnected));
        assert_eq!(
            outbox.frames.len(),
            1,
            "lifecycle edges coalesce to the latest"
        );
        assert!(matches!(outbox.frames[0], Delivery::Disconnected));
        for id in 0..3 {
            assert!(deliver(&tx, &mut outbox, command(id)));
        }
        assert!(deliver(&tx, &mut outbox, Delivery::Settings));
        assert!(deliver(&tx, &mut outbox, Delivery::Settings));
        let settings = outbox
            .frames
            .iter()
            .filter(|frame| matches!(frame, Delivery::Settings))
            .count();
        assert_eq!(settings, 1, "settings wakes coalesce");
        assert_eq!(outbox.commands(), 3, "owned commands are never dropped");
    }

    #[test]
    fn outbox_admits_commands_only_while_fifo_slots_remain() {
        let (tx, _rx) = channel(DELIVERY_BOUND);
        let mut outbox = Outbox::default();
        for id in 0..PENDING_BOUND {
            assert!(deliver(&tx, &mut outbox, command(id as u64)));
        }
        assert!(
            !deliver(&tx, &mut outbox, command(PENDING_BOUND as u64 + 1)),
            "beyond the accepted bound the worker must admit refusal, never queue"
        );
        // Replaceable results still fit: reserved for lifecycle/results.
        assert!(deliver(&tx, &mut outbox, Delivery::Stopped {
            faults: Vec::new()
        }));
    }

    #[test]
    fn outbox_retains_frames_under_saturation_and_pumps_them_in_order() {
        let (tx, mut rx) = channel(2);
        let mut outbox = Outbox::default();
        assert!(deliver(&tx, &mut outbox, command(1)));
        assert!(deliver(&tx, &mut outbox, command(2)));
        // The channel is at capacity: the third command stays retained.
        assert!(deliver(&tx, &mut outbox, command(3)));
        assert_eq!(outbox.commands(), 1, "retained behind the full channel");
        let mut got = Vec::new();
        application::iced::futures::executor::block_on(async {
            for _ in 0..2 {
                if let Some(frame) = rx.next().await {
                    got.push(frame);
                }
            }
        });
        pump(&tx, &mut outbox);
        assert!(outbox.is_empty());
        application::iced::futures::executor::block_on(async {
            if let Some(frame) = rx.next().await {
                got.push(frame);
            }
        });
        let ids: Vec<u64> = got
            .iter()
            .map(|frame| match frame {
                Delivery::Command(command) => command.id,
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(ids, [1, 2, 3], "FIFO delivery survives saturation");
    }

    #[test]
    fn windowed_worker_admits_theme_applies_and_drains_a_clean_receipt() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (dtx, mut drx) = channel(DELIVERY_BOUND);
            let (etx, erx) = tokio::sync::mpsc::unbounded_channel();
            let (ready_tx, ready_rx) = std::sync::mpsc::channel();
            let worker = run_settings(
                "dopus-test".into(),
                "ws://127.0.0.1:1/ws".into(),
                dtx,
                erx,
                ready_tx,
            );
            tokio::pin!(worker);
            // Readiness precedes any await inside run_settings: one poll
            // reaches it, then the ready channel is filled.
            let mut polled = false;
            std::future::poll_fn(|cx| {
                if !polled {
                    polled = true;
                    let _ = worker.as_mut().poll(cx);
                }
                std::task::Poll::Ready(())
            })
            .await;
            let (client, ui, _bootstrap) = ready_rx.recv().unwrap().unwrap();
            drop(client);
            // A keyboard-style theme apply (no reply id): the fenced CAS
            // runs through the real worker/lane machinery and, with no
            // broker, must report the refusal — never a faked success.
            let request = ThemeRequest {
                reply_id: None,
                binding: settings::Binding {
                    instance: "fixture".into(),
                    profile: "default".into(),
                },
                expected_incarnation: "fixture".into(),
                expected_revision: settings::Revision(1),
                operation_id: "dopus-theme-test-1".into(),
                changes: BTreeMap::from([(
                    "appearance.mode".into(),
                    serde_json::json!("dark"),
                )]),
                scheme: "ocean".into(),
                mode: "dark".into(),
            };
            etx.send(Effect::ThemeApply(request)).unwrap();
            let outcome = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    tokio::select! {
                        biased;
                        _ = &mut worker => panic!("the worker exited before the theme completion"),
                        frame = drx.next() => match frame {
                            Some(Delivery::ThemeApplied(result)) => break result,
                            Some(_) => {}
                            None => panic!("delivery channel closed"),
                        },
                    }
                }
            })
            .await
            .expect("the theme completion must arrive");
            assert!(outcome.is_err(), "no authority reachable: the CAS must refuse");
            // Closing the GUI endpoint drains the worker; the receipt is
            // clean because the refusal is a RESULT, not a worker fault.
            drop(ui);
            let faults = tokio::time::timeout(Duration::from_secs(5), &mut worker)
                .await
                .expect("the worker must shut down within its bounded drain");
            assert!(faults.is_empty(), "{faults:?}");
        });
    }
}

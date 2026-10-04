// The constants, the command and request types, `PortControl`, `PortIngress`
// and the admissions, `PortStarter` / `PortWorker` / `prepare`,
// `connect_loop`, `WorkerClient` and its `SupervisedClient` impl,
// `PendingReply`, `worker_loop`, the broker-claim checks, the dispatcher
// (over comp-model's `classify`), and the responders, reply loop, publisher,
// gap and wire-limit code. The command channel is `crate::channel` (a std
// bounded channel plus an engine waker); the snapshot context is
// `crate::service::PortContext`; topic payloads come from comp-model as
// `TopicMessage` and are framed as `BusMessage` here; the noded URL is the
// caller's.

//! The `comp` Bus citizen worker and its bounded ingress. The worker owns a
//! thread with a current-thread tokio runtime and the supervised Bus
//! client; the engine owns the other end of the command channel and
//! answers on its own loop ([`crate::service::PortService`]).

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    future::Future,
    io::Read as _,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering},
        mpsc::{self, Receiver, TrySendError},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use bus::BusMessage;
use bus::{ConnState, SupervisedClient, SupervisedError};
use serde_json::{Value, json};
use tokio::{
    sync::{Semaphore, mpsc as tokio_mpsc, watch},
    task::JoinSet,
};

use comp_model::observation::{
    LossCause, LossInterval, ObservationRecord, PROPS_TOPIC_SUFFIX, PanelRequest, TopicMessage,
    topic_name,
};
use comp_model::reply::{
    ControlReply, MAX_REPLY_BODY_BYTES, MAX_REPLY_WIRE_BYTES, error, too_large, with_error_code,
};
use comp_model::request::{
    InputOp, LONG_VERB_MAX, LONG_VERB_SLACK, LongOp, PING_BODY, Request, WindowOp,
    body_is_malformed, classify,
};
use comp_model::snapshot::{BROKER_CONNECTED, BROKER_RETRYING, CompSnapshot, dispatch_read};
use surfaces::SeatKind;

use crate::channel::{CommandSender, CommandSource, Waker, command_channel};
use crate::outbox::{ObservationOutbox, ObservationProducer, outbox};
use crate::service::{PortContext, PortService};

pub use comp_model::catalogue::{NESTED_SERVICE, REGISTRY_TOPIC, SERVICE};
pub use comp_model::reply::validate_service_name;

pub const PORT_QUEUE_CAPACITY: usize = 16;
/// compd's read-only truth verb: the engine's own view of the registry and
/// focus, beside (not inside) the frozen `comp.*` surface, for the Bus-truth
/// comparator. Answered in control order, so it is fenced like a mutation.
pub const TRUTH_VERB: &str = "compd.truth";
const PORT_REPLY_CAPACITY: usize = 16;
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(2);
/// Verbs that reply after a wait hold their own permits, so a few long
/// waits can never starve ordinary reads and controls.
pub const LONG_VERB_PERMITS: usize = 8;
const REPLY_SEND_TIMEOUT: Duration = Duration::from_secs(2);
const PUBLISH_TIMEOUT: Duration = Duration::from_secs(2);
const GAP_RETRY_INITIAL: Duration = Duration::from_secs(1);
const GAP_RETRY_MAX: Duration = Duration::from_secs(30);
const PORT_SHUTDOWN_GRACE: Duration = Duration::from_millis(300);
const CLIENT_SHUTDOWN_BUDGET: Duration = Duration::from_millis(250);
const DEREGISTER_BUDGET: Duration = Duration::from_millis(200);
const CLOSE_BUDGET: Duration = Duration::from_millis(50);

/// The broker URL when the caller names none: `MIXOS_NODED_URL`, else the
/// node config (`node.conf.mix`), else the loopback broker.
pub fn default_noded_url() -> String {
    bus::noded_url()
}

pub enum PortCommand {
    Panel(PortPanelRequest),
    /// The registered services, from a registry diff (full set, not a delta).
    ServicesLive(BTreeSet<String>),
    Snapshot(PortRequest),
    Watch(PortReply),
    PointerWatch(PortReply),
    Set(PortSetRequest),
    Window(PortWindowRequest),
    Input(PortInputRequest),
    Long(PortLongRequest),
    WatchState { active: bool, order: u64 },
    /// [`TRUTH_VERB`].
    Truth(PortReply),
}

pub struct PortRequest {
    pub order: u64,
    pub reply: tokio::sync::oneshot::Sender<Arc<CompSnapshot>>,
    /// The read's own path or prefix, so the snapshot can be scoped to it;
    /// `None` asks for the whole tree.
    pub scope: Option<String>,
}

pub struct PortReply {
    pub order: u64,
    pub reply: tokio::sync::oneshot::Sender<ControlReply>,
}

pub struct PortPanelRequest {
    pub order: u64,
    pub op: PanelRequest,
    pub reply: Option<tokio::sync::oneshot::Sender<ControlReply>>,
}

pub struct PortSetRequest {
    pub order: u64,
    pub path: String,
    pub value: Value,
    /// Optional role-generation fence for `windows.s<id>.*` writes.
    pub generation: Option<u64>,
    pub reply: Option<tokio::sync::oneshot::Sender<ControlReply>>,
}

pub struct PortWindowRequest {
    pub order: u64,
    pub op: WindowOp,
    pub reply: Option<tokio::sync::oneshot::Sender<ControlReply>>,
}

pub struct PortInputRequest {
    pub order: u64,
    pub agent_epoch: u64,
    pub op: InputOp,
    pub reply: Option<tokio::sync::oneshot::Sender<ControlReply>>,
}

pub struct PortLongRequest {
    pub order: u64,
    pub agent_epoch: u64,
    pub op: Option<LongOp>,
    pub reply: Option<tokio::sync::oneshot::Sender<ControlReply>>,
    /// The queue slot this request holds until the engine has taken it:
    /// dropping it there frees the slot while the verb waits, so long
    /// waits never fill the bounded ingress.
    pub slot: Option<QueueSlot>,
    /// When the worker admitted it; the verb's deadline runs from here.
    pub admitted: std::time::Instant,
}

pub enum PortControl {
    Panel(PortPanelRequest),
    Watch(PortReply),
    PointerWatch(PortReply),
    Set(PortSetRequest),
    Window(PortWindowRequest),
    Input(PortInputRequest),
    Long(PortLongRequest),
    WatchState { active: bool, order: u64 },
    Truth(PortReply),
}

impl PortControl {
    pub fn uses_agent(&self) -> bool {
        match self {
            Self::Input(request) => matches!(request.op, InputOp::OnSeat { seat: SeatKind::Agent, .. }),
            Self::Long(request) => match request.op.as_ref() {
                Some(LongOp::Sequence(steps) | LongOp::SeatedSequence { steps, .. }) => steps.iter().any(|step|
                    matches!(step.op, InputOp::OnSeat { seat: SeatKind::Agent, .. })),
                _ => false,
            },
            _ => false,
        }
    }

    /// Refuse old admissions even if they were still in the ingress channel at
    /// the lifecycle boundary. Other controls keep their relative order.
    pub fn refuse_cleared_agent(&mut self, epoch: u64) -> bool {
        if !self.uses_agent() { return false; }
        let reply = match self {
            Self::Input(request) if request.agent_epoch != epoch => request.reply.take(),
            Self::Long(request) if request.agent_epoch != epoch => {
                request.slot.take();
                request.reply.take()
            }
            _ => return false,
        };
        if let Some(reply) = reply {
            let _ = reply.send(ControlReply::refused("input_cleared", json!({"seat":"agent", "released":true})));
        }
        true
    }

    pub fn order(&self) -> u64 {
        match self {
            Self::Panel(request) => request.order,
            Self::Watch(request) => request.order,
            Self::PointerWatch(request) => request.order,
            Self::Set(request) => request.order,
            Self::Window(request) => request.order,
            Self::Input(request) => request.order,
            Self::Long(request) => request.order,
            Self::WatchState { order, .. } => *order,
            Self::Truth(request) => request.order,
        }
    }
}

/// The ingress refused an admission: the bounded queue is full or the
/// engine end is gone. The caller answers `busy` either way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueueFull;

#[derive(Clone)]
pub struct PortIngress {
    sender: CommandSender,
    queue_depth: Arc<AtomicUsize>,
    control_order: Arc<AtomicU64>,
    agent_epoch: Arc<AtomicU64>,
    pending_idle_order: Arc<AtomicU64>,
    pending_active_order: Arc<AtomicU64>,
}

impl PortIngress {
    pub fn request_panel(&self, op: PanelRequest) -> Result<ControlAdmission, QueueFull> {
        let (reply, receive) = tokio::sync::oneshot::channel();
        self.admit(PortCommand::Panel(PortPanelRequest {
            order: self.next_control_order(), op, reply: Some(reply),
        }), receive)
    }
    /// Whole-tree snapshot; production reads go through the scoped form.
    pub fn request_snapshot(&self) -> Result<SnapshotAdmission, QueueFull> {
        self.request_snapshot_scoped(None)
    }

    /// A snapshot request that names the read's own path or prefix, so the
    /// merged `ReadScopes` can skip subtrees nobody asked for; `None` reads
    /// the whole tree.
    pub fn request_snapshot_scoped(
        &self,
        scope: Option<String>,
    ) -> Result<SnapshotAdmission, QueueFull> {
        let (reply, receive) = tokio::sync::oneshot::channel();
        self.admit(PortCommand::Snapshot(PortRequest {
            order: self.next_control_order(), reply, scope,
        }), receive)
            .map(SnapshotAdmission)
    }

    pub fn request_watch(&self) -> Result<ControlAdmission, QueueFull> {
        let (reply, receive) = tokio::sync::oneshot::channel();
        let order = self.next_control_order();
        self.admit(PortCommand::Watch(PortReply { order, reply }), receive)
    }
    pub fn request_truth(&self) -> Result<ControlAdmission, QueueFull> {
        let (reply, receive) = tokio::sync::oneshot::channel();
        let order = self.next_control_order();
        self.admit(PortCommand::Truth(PortReply { order, reply }), receive)
    }

    pub fn request_pointer_watch(&self) -> Result<ControlAdmission, QueueFull> {
        let (reply, receive) = tokio::sync::oneshot::channel();
        let order = self.next_control_order();
        self.admit(
            PortCommand::PointerWatch(PortReply { order, reply }),
            receive,
        )
    }

    pub fn request_set(&self, path: String, value: Value) -> Result<ControlAdmission, QueueFull> {
        self.request_set_fenced(path, value, None)
    }

    pub fn request_set_fenced(
        &self,
        path: String,
        value: Value,
        generation: Option<u64>,
    ) -> Result<ControlAdmission, QueueFull> {
        let (reply, receive) = tokio::sync::oneshot::channel();
        self.admit(
            PortCommand::Set(PortSetRequest {
                order: self.next_control_order(),
                path,
                value,
                generation,
                reply: Some(reply),
            }),
            receive,
        )
    }

    pub fn request_window(&self, op: WindowOp) -> Result<ControlAdmission, QueueFull> {
        let (reply, receive) = tokio::sync::oneshot::channel();
        self.admit(
            PortCommand::Window(PortWindowRequest {
                order: self.next_control_order(),
                op,
                reply: Some(reply),
            }),
            receive,
        )
    }

    pub fn request_input(&self, op: InputOp) -> Result<ControlAdmission, QueueFull> {
        let (reply, receive) = tokio::sync::oneshot::channel();
        self.admit(
            PortCommand::Input(PortInputRequest {
                order: self.next_control_order(),
                agent_epoch: self.agent_epoch.load(Ordering::Acquire),
                op,
                reply: Some(reply),
            }),
            receive,
        )
    }

    /// Admit a long verb. The queue slot rides inside the command and is
    /// released when the protocol thread takes it, not when the reply
    /// arrives; the reply wait is bounded by the verb's own budget.
    pub fn request_long(&self, op: LongOp) -> Result<LongAdmission, QueueFull> {
        let timeout = op.budget().min(LONG_VERB_MAX) + LONG_VERB_SLACK;
        let (reply, receive) = tokio::sync::oneshot::channel();
        let slot = self.reserve_slot()?;
        let command = PortCommand::Long(PortLongRequest {
            order: self.next_control_order(),
            agent_epoch: self.agent_epoch.load(Ordering::Acquire),
            op: Some(op),
            reply: Some(reply),
            slot: Some(slot),
            admitted: std::time::Instant::now(),
        });
        // A refused send drops the command, and with it the slot.
        self.sender.try_send(command).map_err(|_| QueueFull)?;
        Ok(LongAdmission { receive, timeout })
    }

    /// Best effort: a full queue drops the set, and the next registry diff or
    /// the liveness probe on the holder's layers repair it.
    pub fn services_live(&self, live: std::collections::BTreeSet<String>) {
        if self.sender.try_send(PortCommand::ServicesLive(live)).is_err() {
            tracing::debug!("registry update dropped: compositor port queue full");
        }
    }

    pub fn set_watch_state(&self, active: bool) {
        let order = self.next_control_order();
        if let Err(TrySendError::Full(_)) = self
            .sender
            .try_send(PortCommand::WatchState { active, order })
        {
            if active {
                self.pending_active_order.fetch_max(order, Ordering::AcqRel);
            } else {
                self.pending_idle_order.fetch_max(order, Ordering::AcqRel);
            }
        }
    }

    fn next_control_order(&self) -> u64 {
        self.control_order
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                Some(current.saturating_add(1))
            })
            .unwrap_or(u64::MAX)
            .saturating_add(1)
    }

    fn admit<T>(
        &self,
        command: PortCommand,
        receive: tokio::sync::oneshot::Receiver<T>,
    ) -> Result<Admission<T>, QueueFull> {
        let depth = self.reserve_slot()?;
        match self.sender.try_send(command) {
            Ok(()) => Ok(Admission { receive, depth }),
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => Err(QueueFull),
        }
    }

    fn reserve_slot(&self) -> Result<QueueSlot, QueueFull> {
        let mut depth = self.queue_depth.load(Ordering::Acquire);
        loop {
            if depth >= PORT_QUEUE_CAPACITY {
                return Err(QueueFull);
            }
            match self.queue_depth.compare_exchange_weak(
                depth,
                depth + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(QueueSlot(Arc::clone(&self.queue_depth))),
                Err(observed) => depth = observed,
            }
        }
    }

    /// Admitted commands not yet taken by the engine (`port.queue_depth`).
    pub fn depth(&self) -> usize {
        self.queue_depth.load(Ordering::Acquire)
    }
}

pub struct Admission<T> {
    receive: tokio::sync::oneshot::Receiver<T>,
    depth: QueueSlot,
}

pub struct LongAdmission {
    receive: tokio::sync::oneshot::Receiver<ControlReply>,
    timeout: Duration,
}

impl LongAdmission {
    pub async fn receive(self) -> Result<ControlReply, ()> {
        match tokio::time::timeout(self.timeout, self.receive).await {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(_)) | Err(_) => Err(()),
        }
    }

    /// How long the worker waits for the engine's answer.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }
}

impl<T> Admission<T> {
    pub async fn receive(self) -> Result<T, ()> {
        let Self { receive, depth } = self;
        let result = match tokio::time::timeout(SNAPSHOT_TIMEOUT, receive).await {
            Ok(Ok(snapshot)) => Ok(snapshot),
            Ok(Err(_)) | Err(_) => Err(()),
        };
        drop(depth);
        result
    }
}

pub struct SnapshotAdmission(Admission<Arc<CompSnapshot>>);

impl SnapshotAdmission {
    pub async fn receive(self) -> Result<Arc<CompSnapshot>, ()> {
        self.0.receive().await
    }
}

pub type ControlAdmission = Admission<ControlReply>;

/// The engine's half of a prepared port.
pub struct PortWiring {
    /// Drain this from the engine loop whenever the waker fires.
    pub service: PortService,
    /// Offer observation records here (`offer_next`).
    pub observation_producer: ObservationProducer,
    pub context: Arc<PortContext>,
}

pub struct PortStarter {
    service: String,
    noded_url: String,
    ingress: PortIngress,
    broker: Arc<AtomicU8>,
    reply_timeouts: Arc<AtomicU64>,
    publish_timeouts: Arc<AtomicU64>,
    observations: ObservationOutbox,
    observation_notifier: Arc<tokio::sync::Notify>,
    lost_count: Arc<AtomicU64>,
}

pub struct PortWorker {
    shutdown: watch::Sender<bool>,
    ingress: Option<PortIngress>,
    completion: Mutex<Receiver<()>>,
    thread: Option<JoinHandle<()>>,
}

/// What the engine says about itself in `info.*`.
#[derive(Clone, Debug)]
pub struct PortIdentity {
    /// `comp` on KMS, `comp-nested` nested (or `--bus-service`).
    pub service: String,
    pub version: String,
    /// `kms` / `nested`.
    pub backend: &'static str,
    /// The renderer id (`info.engine`).
    pub engine: &'static str,
    /// The broker to dial (see [`default_noded_url`]).
    pub noded_url: String,
}

/// Build both halves of the `comp` port. `waker` is called on every
/// command the worker hands the engine; the engine then calls
/// [`PortService::service`]. Nothing runs until [`PortStarter::start`].
pub fn prepare(identity: PortIdentity, waker: Waker) -> Result<(PortWiring, PortStarter), String> {
    validate_service_name(&identity.service)?;
    let context = Arc::new(PortContext::new(
        &identity.service,
        &identity.version,
        identity.backend,
        identity.engine,
        &random_instance_id()?,
    ));
    let (sender, source) = command_channel(PORT_QUEUE_CAPACITY, waker.clone());
    let (wiring, starter) = wire(identity.service, identity.noded_url, context, sender, source, waker);
    Ok((wiring, starter))
}

fn wire(
    service: String,
    noded_url: String,
    context: Arc<PortContext>,
    sender: CommandSender,
    source: CommandSource,
    waker: Waker,
) -> (PortWiring, PortStarter) {
    let (observation_producer, observations) =
        outbox(Arc::clone(&context.lost_count), Arc::clone(&context.event_seq));
    let observation_notifier = observation_producer.notifier();
    let ingress = PortIngress {
        sender,
        queue_depth: Arc::clone(&context.queue_depth),
        control_order: Arc::new(AtomicU64::new(0)),
        agent_epoch: Arc::clone(&context.agent_epoch),
        pending_idle_order: Arc::clone(&context.pending_idle_order),
        pending_active_order: Arc::clone(&context.pending_active_order),
    };
    let broker = Arc::clone(&context.broker);
    let reply_timeouts = Arc::clone(&context.reply_timeouts);
    let publish_timeouts = Arc::clone(&context.publish_timeouts);
    let lost_count = Arc::clone(&context.lost_count);
    (
        PortWiring {
            service: PortService::new(source, Arc::clone(&context), waker),
            observation_producer,
            context,
        },
        PortStarter {
            service,
            noded_url,
            ingress,
            broker,
            reply_timeouts,
            publish_timeouts,
            observations,
            observation_notifier,
            lost_count,
        },
    )
}

/// Both halves wired to an in-memory channel, for tests and for an engine
/// that drives the worker loop itself.
pub fn test_wiring(service: &str) -> (PortWiring, PortStarter) {
    let context = Arc::new(PortContext::new(service, "0.0.0-test", "nested", "test", "fixture"));
    let (sender, source) = command_channel(PORT_QUEUE_CAPACITY, crate::channel::no_waker());
    wire(
        service.to_string(),
        default_noded_url(),
        context,
        sender,
        source,
        crate::channel::no_waker(),
    )
}

impl PortStarter {
    /// The ingress the worker admits through (tests drive it directly).
    pub fn ingress(&self) -> &PortIngress {
        &self.ingress
    }

    pub fn start(self) -> Result<PortWorker, String> {
        let (shutdown, shutdown_rx) = watch::channel(false);
        let (completion_tx, completion) = mpsc::sync_channel(1);
        let thread_ingress = self.ingress.clone();
        let service = self.service;
        let noded_url = self.noded_url;
        let broker = self.broker;
        let reply_timeouts = self.reply_timeouts;
        let publish_timeouts = self.publish_timeouts;
        let observations = self.observations;
        let observation_notifier = self.observation_notifier;
        let lost_count = self.lost_count;
        let thread = thread::Builder::new()
            .name("compd-port".into())
            .spawn(move || {
                let _completion = CompletionOnDrop(completion_tx);
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        tracing::error!(%error, "failed to build compositor Bus runtime");
                        return;
                    }
                };
                let connect_service = service.clone();
                let connect_url = noded_url;
                runtime.block_on(worker_loop(
                    service,
                    thread_ingress,
                    broker,
                    reply_timeouts,
                    publish_timeouts,
                    observations,
                    observation_notifier,
                    lost_count,
                    shutdown_rx,
                    move || {
                        let service = connect_service.clone();
                        let url = connect_url.clone();
                        async move {
                            SupervisedClient::connect_options(&service, &url)
                                .fatal_on_registration_rejection(true)
                                .connect()
                                .await
                                .map_err(|error| classify_connect_error(&service, error))
                        }
                    },
                ));
            })
            .map_err(|error| format!("failed to spawn compositor Bus worker: {error}"))?;
        Ok(PortWorker {
            shutdown,
            ingress: Some(self.ingress),
            completion: Mutex::new(completion),
            thread: Some(thread),
        })
    }
}

impl PortWorker {
    pub fn begin_shutdown(&mut self) {
        let _ = self.shutdown.send(true);
        self.ingress.take();
    }

    pub fn finish(mut self) {
        self.begin_shutdown();
        let Some(thread) = self.thread.take() else {
            return;
        };
        let completion = self
            .completion
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match completion.recv_timeout(PORT_SHUTDOWN_GRACE) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                if thread.join().is_err() {
                    tracing::error!("compositor Bus worker panicked during shutdown");
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                tracing::warn!(
                    grace_ms = PORT_SHUTDOWN_GRACE.as_millis(),
                    "compositor Bus worker did not stop in time and was detached"
                );
                drop(thread);
            }
        }
    }
}

struct CompletionOnDrop(mpsc::SyncSender<()>);

impl Drop for CompletionOnDrop {
    fn drop(&mut self) {
        let _ = self.0.try_send(());
    }
}

/// One admitted ingress entry; dropping it frees the slot.
pub struct QueueSlot(Arc<AtomicUsize>);

impl Drop for QueueSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Debug)]
enum ConnectAttemptError {
    Retry(String),
    RegistrationRejected {
        service: String,
        rc: u8,
        message: String,
    },
}

enum ConnectOutcome<C> {
    Connected(C),
    RegistrationRejected {
        service: String,
        rc: u8,
        message: String,
    },
    Shutdown,
}

async fn connect_loop<F, Fut, C>(
    shutdown: &mut watch::Receiver<bool>,
    broker: &AtomicU8,
    mut connector: F,
    minimum_delay: Duration,
) -> ConnectOutcome<C>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<C, ConnectAttemptError>>,
{
    let mut attempt = 0_u32;
    loop {
        broker.store(BROKER_RETRYING, Ordering::Release);
        let result = tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return ConnectOutcome::Shutdown;
                }
                continue;
            }
            result = connector() => result,
        };
        match result {
            Ok(client) => return ConnectOutcome::Connected(client),
            Err(ConnectAttemptError::RegistrationRejected {
                service,
                rc,
                message,
            }) => {
                return ConnectOutcome::RegistrationRejected {
                    service,
                    rc,
                    message,
                };
            }
            Err(ConnectAttemptError::Retry(message)) => {
                tracing::debug!(attempt, error = %message, "compositor Bus connect failed; retrying");
                let exponential = 250_u64.saturating_mul(1_u64 << attempt.min(16)).min(30_000);
                let delay = minimum_delay.max(Duration::from_millis(exponential));
                attempt = attempt.saturating_add(1);
                tokio::select! {
                    changed = shutdown.changed() => {
                        if changed.is_err() || *shutdown.borrow() {
                            return ConnectOutcome::Shutdown;
                        }
                    }
                    _ = tokio::time::sleep(delay) => {}
                }
            }
        }
    }
}

type WorkerFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

trait WorkerClient: Send + Sync + 'static {
    fn incoming(&self) -> Option<tokio_mpsc::UnboundedReceiver<bus::IncomingCommand>>;
    fn state(&self) -> ConnState;
    fn subscribe_state(&self) -> watch::Receiver<ConnState>;
    fn respond_parts<'a>(&'a self, reply: &'a PendingReply)
    -> WorkerFuture<'a, Result<(), String>>;
    fn publish<'a>(
        &'a self,
        headers: &'a BTreeMap<String, String>,
        wire: &'a str,
    ) -> WorkerFuture<'a, Result<(), String>>;
    fn deregister(&self) -> WorkerFuture<'_, Result<(), String>>;
    fn close(&self) -> WorkerFuture<'_, ()>;
    /// Subscribe once; the supervised client replays it on every reconnect.
    fn subscribe_topic<'a>(&'a self, topic: &'a str) -> WorkerFuture<'a, Result<(), String>>;
}

impl WorkerClient for SupervisedClient {
    fn incoming(&self) -> Option<tokio_mpsc::UnboundedReceiver<bus::IncomingCommand>> {
        SupervisedClient::incoming(self)
    }

    fn state(&self) -> ConnState {
        SupervisedClient::state(self)
    }

    fn subscribe_state(&self) -> watch::Receiver<ConnState> {
        SupervisedClient::subscribe_state(self)
    }

    fn respond_parts<'a>(
        &'a self,
        reply: &'a PendingReply,
    ) -> WorkerFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.respond_parts(
                &reply.from,
                &reply.command,
                reply.id.as_deref(),
                reply.rc,
                &reply.body,
            )
            .await
            .map_err(|error| error.to_string())
        })
    }

    fn publish<'a>(
        &'a self,
        headers: &'a BTreeMap<String, String>,
        wire: &'a str,
    ) -> WorkerFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let (rc, body, _) = self
                .call_with_headers_raw("noded", "topic.publish", headers, wire)
                .await
                .map_err(|error| error.to_string())?;
            if rc == 0 {
                Ok(())
            } else {
                Err(format!("topic.publish rejected with rc {rc}: {body}"))
            }
        })
    }

    fn deregister(&self) -> WorkerFuture<'_, Result<(), String>> {
        Box::pin(async move {
            SupervisedClient::deregister(self)
                .await
                .map_err(|error| error.to_string())
        })
    }

    fn close(&self) -> WorkerFuture<'_, ()> {
        Box::pin(SupervisedClient::close(self))
    }

    fn subscribe_topic<'a>(&'a self, topic: &'a str) -> WorkerFuture<'a, Result<(), String>> {
        Box::pin(async move {
            SupervisedClient::subscribe_topic(self, topic)
                .await
                .map_err(|error| error.to_string())
        })
    }
}

struct PendingReply {
    from: String,
    command: String,
    id: Option<String>,
    rc: u8,
    body: Arc<str>,
}

impl PendingReply {
    fn new(command: bus::IncomingCommand, (rc, body): (u8, Arc<str>)) -> Self {
        let (rc, body) = with_error_code(rc, body);
        Self {
            from: command.from,
            command: command.command,
            id: command.id,
            rc,
            body,
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn worker_loop<F, Fut, C>(
    service: String,
    ingress: PortIngress,
    broker: Arc<AtomicU8>,
    reply_timeouts: Arc<AtomicU64>,
    publish_timeouts: Arc<AtomicU64>,
    observations: ObservationOutbox,
    observation_notifier: Arc<tokio::sync::Notify>,
    lost_count: Arc<AtomicU64>,
    mut shutdown: watch::Receiver<bool>,
    connector: F,
) where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<C, ConnectAttemptError>>,
    C: WorkerClient,
{
    let outcome = connect_loop(&mut shutdown, &broker, connector, Duration::ZERO).await;
    let client = match outcome {
        ConnectOutcome::Connected(client) => Arc::new(client),
        ConnectOutcome::RegistrationRejected {
            service,
            rc,
            message,
        } => {
            tracing::error!(service = %service, rc, %message, "Bus registration rejected; compositor continues without a port");
            return;
        }
        ConnectOutcome::Shutdown => return,
    };
    let Some(mut incoming) = client.incoming() else {
        tracing::error!(service = %service, "compositor Bus incoming stream was already taken");
        return;
    };
    let mut states = client.subscribe_state();
    apply_connection_state(&broker, *states.borrow());
    let mut responders = JoinSet::new();
    let responder_permits = Arc::new(Semaphore::new(PORT_QUEUE_CAPACITY));
    let long_permits = Arc::new(Semaphore::new(LONG_VERB_PERMITS));
    let (reply_sender, reply_receiver) = tokio_mpsc::channel(PORT_REPLY_CAPACITY);
    let reply_task = tokio::spawn(reply_loop(
        Arc::clone(&client),
        Arc::from(service.as_str()),
        reply_receiver,
        Arc::clone(&reply_timeouts),
    ));
    let publisher_task = tokio::spawn(publisher_loop(
        Arc::clone(&client),
        Arc::from(service.as_str()),
        observations,
        Arc::clone(&observation_notifier),
        Arc::clone(&lost_count),
        Arc::clone(&publish_timeouts),
        shutdown.clone(),
    ));
    // Holder cleanup on Bus departure. The supervised client replays a
    // subscription once it has succeeded; until then every (re)connect tries
    // again. Best effort: the liveness probe bounds what a missed departure
    // can leave behind.
    let registry_subscribed = Arc::new(AtomicBool::new(false));
    let subscribe_registry = |client: &Arc<C>, subscribed: &Arc<AtomicBool>| {
        let client = Arc::clone(client);
        let subscribed = Arc::clone(subscribed);
        tokio::spawn(async move {
            match client.subscribe_topic(REGISTRY_TOPIC).await {
                Ok(()) => subscribed.store(true, Ordering::Release),
                Err(error) => tracing::warn!(%error, "registry subscription failed; retried on the next connect"),
            }
        })
    };
    let mut registry_task = subscribe_registry(&client, &registry_subscribed);

    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            changed = states.changed() => {
                if changed.is_err() {
                    break;
                }
                let state = *states.borrow_and_update();
                apply_connection_state(&broker, state);
                observation_notifier.notify_one();
                if state == ConnState::Connected && !registry_subscribed.load(Ordering::Acquire) {
                    registry_task.abort();
                    registry_task = subscribe_registry(&client, &registry_subscribed);
                }
                if state == ConnState::Fatal {
                    tracing::error!(service = %service, "Bus registration rejected during reconnect; compositor continues without a port");
                    break;
                }
            }
            command = incoming.recv() => {
                let Some(command) = command else {
                    let state = client.state();
                    apply_connection_state(&broker, state);
                    if state == ConnState::Fatal {
                        tracing::error!(service = %service, "Bus registration rejected during reconnect; compositor continues without a port");
                    }
                    break;
                };
                dispatch_incoming(
                    &ingress,
                    &mut responders,
                    &responder_permits,
                    &long_permits,
                    &reply_sender,
                    &reply_timeouts,
                    &service,
                    command,
                );
            }
            completed = responders.join_next(), if !responders.is_empty() => {
                if let Some(Err(error)) = completed {
                    tracing::debug!(%error, "compositor Bus responder task stopped");
                }
            }
        }
    }

    registry_task.abort();
    responders.abort_all();
    while responders.join_next().await.is_some() {}
    drop(reply_sender);
    reply_task.abort();
    let _ = reply_task.await;
    // Port shutdown is deliberately bounded: once requested, a retained gap
    // may be abandoned rather than extending compositor teardown indefinitely.
    publisher_task.abort();
    let _ = publisher_task.await;
    graceful_client_shutdown(client.as_ref()).await;
}

fn classify_connect_error(service: &str, error: SupervisedError) -> ConnectAttemptError {
    if let Some((rc, message)) = error.registration_rejection() {
        ConnectAttemptError::RegistrationRejected {
            service: service.to_string(),
            rc,
            message: message.to_string(),
        }
    } else {
        ConnectAttemptError::Retry(error.to_string())
    }
}

async fn graceful_client_shutdown<C: WorkerClient>(client: &C) {
    debug_assert_eq!(CLIENT_SHUTDOWN_BUDGET, DEREGISTER_BUDGET + CLOSE_BUDGET);
    match tokio::time::timeout(DEREGISTER_BUDGET, client.deregister()).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            tracing::debug!(%error, "compositor Bus deregister did not complete cleanly")
        }
        Err(_) => tracing::debug!(
            timeout_ms = DEREGISTER_BUDGET.as_millis(),
            "compositor Bus deregister timed out"
        ),
    }
    if tokio::time::timeout(CLOSE_BUDGET, client.close())
        .await
        .is_err()
    {
        tracing::debug!(
            timeout_ms = CLOSE_BUDGET.as_millis(),
            "compositor Bus close timed out"
        );
    }
}

fn apply_connection_state(broker: &AtomicU8, state: ConnState) {
    broker.store(
        if state == ConnState::Connected {
            BROKER_CONNECTED
        } else {
            BROKER_RETRYING
        },
        Ordering::Release,
    );
}

/// The pre-long-verb entry the existing tests drive: a fresh long pool per
/// call, so only the tests that exercise long verbs see pool pressure.
#[cfg(test)]
fn handle_incoming(
    ingress: &PortIngress,
    responders: &mut JoinSet<()>,
    responder_permits: &Arc<Semaphore>,
    reply_sender: &tokio_mpsc::Sender<PendingReply>,
    reply_timeouts: &Arc<AtomicU64>,
    service: &str,
    command: bus::IncomingCommand,
) {
    dispatch_incoming(
        ingress,
        responders,
        responder_permits,
        &Arc::new(Semaphore::new(LONG_VERB_PERMITS)),
        reply_sender,
        reply_timeouts,
        service,
        command,
    );
}

/// A `from: noded` message is the local broker only when noded did not stamp
/// it as relayed from the mesh. ABSENT IS THE NORMAL PRODUCTION CASE: noded's
/// `build_topic_notice` never stamps `broker_origin`, on any broker version,
/// so every real `topic.active`/`topic.idle` arrives without it. Requiring
/// `local` here would silently break the props publishing lifecycle. Accept
/// absent or `local`; refuse anything else. Every case-variant spelling of
/// the header must say `local`, independent of noded's own stripping.
fn from_local_broker(command: &bus::IncomingCommand) -> bool {
    command
        .headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("broker_origin"))
        .all(|(_, origin)| origin.eq_ignore_ascii_case("local"))
}

const FOREIGN_BROKER_WARN_INTERVAL: Duration = Duration::from_secs(10);

/// At most one warning per interval: a peer replaying these must not flood
/// the journal, but the first one is always visible.
fn warn_foreign_broker_claim(command: &bus::IncomingCommand) {
    static LAST: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
    let now = std::time::Instant::now();
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if last.is_some_and(|at| now.duration_since(at) < FOREIGN_BROKER_WARN_INTERVAL) {
        return;
    }
    *last = Some(now);
    let header = |key: &str| {
        command
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(key))
            .map_or("", |(_, value)| value.as_str())
    };
    tracing::warn!(
        broker_origin = header("broker_origin"),
        source_peer = header("source_peer"),
        command = %command.command,
        topic = command.topic().unwrap_or(""),
        "ignored a non-local message claiming to be the broker (from: noded)"
    );
}

/// One incoming command through the dispatch boundary: the broker's own
/// lifecycle and registry notices first (only the local broker may speak as
/// `noded`), then the verb, routed by comp-model's `classify` in family
/// order. Every reply is queued, never awaited, so a slow engine or a
/// black-holed caller cannot stall the worker.
#[allow(clippy::too_many_arguments)]
fn dispatch_incoming(
    ingress: &PortIngress,
    responders: &mut JoinSet<()>,
    responder_permits: &Arc<Semaphore>,
    long_permits: &Arc<Semaphore>,
    reply_sender: &tokio_mpsc::Sender<PendingReply>,
    reply_timeouts: &Arc<AtomicU64>,
    service: &str,
    command: bus::IncomingCommand,
) {
    while let Some(completed) = responders.try_join_next() {
        if let Err(error) = completed {
            tracing::debug!(%error, "compositor Bus responder task stopped");
        }
    }
    let malformed = body_is_malformed(&command.body);
    // The broker stamps `broker_service` on everything it originates and may
    // send no `from` (see the registry diff below), so either names it.
    let from_noded = command.from == "noded" || command.header("broker_service") == Some("noded");
    let broker_lifecycle = from_noded
        && matches!(command.command.as_str(), "topic.active" | "topic.idle")
        && command.headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("name") && value == &topic_name(service, PROPS_TOPIC_SUFFIX)
        });
    // noded's registry diff is a topic delivery of its own publish: the live
    // broker (noded 0.18.2) sends it with NO `from` and stamps the publisher
    // as `broker_service`. Matching on `from` alone (this port's first check)
    // never saw a holder leave. `from: noded` is kept for a broker that sets
    // it.
    let broker_registry = command.topic() == Some(REGISTRY_TOPIC) && from_noded;
    // Only the LOCAL broker speaks as `noded`. Defence in depth: mesh ingress
    // strips `from` and responses never reach this dispatch, so the one
    // reachable forgery is a client of a pre-0.18 noded that registered the
    // name `noded`; its routed messages arrive as `broker_origin: mesh`.
    if (broker_lifecycle || broker_registry) && !from_local_broker(&command) {
        warn_foreign_broker_claim(&command);
        return;
    }
    if broker_lifecycle {
        ingress.set_watch_state(command.command == "topic.active");
        return;
    }
    // The broker's registry: a holder service that left takes its panel
    // holds with it.
    if broker_registry {
        if let Ok(body) = serde_json::from_str::<Value>(&command.body)
            && body["path"] == "services.registered"
            && let Some(live) = body["new"].as_array().and_then(|names| {
                names
                    .iter()
                    .map(|name| name.as_str().map(str::to_owned))
                    .collect::<Option<BTreeSet<String>>>()
            })
        {
            ingress.services_live(live);
        }
        return;
    }
    // Any other topic delivery is a pub/sub fan-out, never a request: it is
    // dropped unanswered whatever its publisher (a `props.changed` on the
    // registry topic stamped by another service would otherwise fall through
    // to verb dispatch and be answered `unknown_verb`).
    if command.is_topic_delivery() {
        return;
    }
    // compd's truth verb is not part of the frozen `comp.*` surface, so it is
    // routed before `classify` (which answers every other name `unknown_verb`).
    if command.command == TRUTH_VERB {
        let Ok(permit) = Arc::clone(responder_permits).try_acquire_owned() else {
            queue_reply(reply_sender, reply_timeouts, PendingReply::new(command, error("busy")));
            return;
        };
        match ingress.request_truth() {
            Ok(admission) => spawn_control_responder(
                responders,
                reply_sender,
                reply_timeouts,
                command,
                admission,
                permit,
            ),
            Err(QueueFull) => {
                queue_reply(reply_sender, reply_timeouts, PendingReply::new(command, error("busy")))
            }
        }
        return;
    }
    let request = match classify(&command.command, &command.args, malformed) {
        Ok(request) => request,
        Err(reply) => {
            queue_reply(reply_sender, reply_timeouts, PendingReply::new(command, reply));
            return;
        }
    };
    let busy = |command| PendingReply::new(command, error("busy"));
    match request {
        Request::Ping => {
            queue_reply(
                reply_sender,
                reply_timeouts,
                PendingReply::new(command, (0, Arc::from(PING_BODY))),
            );
        }
        Request::Long(op) => spawn_long_verb(
            ingress,
            responders,
            long_permits,
            reply_sender,
            reply_timeouts,
            command,
            op,
        ),
        Request::Read { verb, scope } => {
            let Ok(permit) = Arc::clone(responder_permits).try_acquire_owned() else {
                queue_reply(reply_sender, reply_timeouts, busy(command));
                return;
            };
            // The read's own subtree scopes the snapshot.
            let Ok(admission) = ingress.request_snapshot_scoped(scope) else {
                queue_reply(reply_sender, reply_timeouts, busy(command));
                drop(permit);
                return;
            };
            let reply_sender = reply_sender.clone();
            let reply_timeouts = Arc::clone(reply_timeouts);
            responders.spawn(async move {
                let _permit = permit;
                let reply = match admission.receive().await {
                    Ok(snapshot) => {
                        let args = command.args.clone();
                        tokio::task::spawn_blocking(move || dispatch_read(&snapshot, verb, &args))
                            .await
                            .unwrap_or_else(|_| error("busy"))
                    }
                    Err(()) => error("busy"),
                };
                queue_reply(&reply_sender, &reply_timeouts, PendingReply::new(command, reply));
            });
        }
        request => {
            let Ok(permit) = Arc::clone(responder_permits).try_acquire_owned() else {
                queue_reply(reply_sender, reply_timeouts, busy(command));
                return;
            };
            let admission = match request {
                Request::Watch => ingress.request_watch(),
                Request::PointerWatch => ingress.request_pointer_watch(),
                Request::Set {
                    path,
                    value,
                    generation,
                } => ingress.request_set_fenced(path, value, generation),
                Request::Window(op) => ingress.request_window(op),
                Request::Input(op) => ingress.request_input(op),
                Request::Panel(mut op) => {
                    // Only the broker's stamp names the holder service.
                    op.sender.clone_from(&command.from);
                    ingress.request_panel(op)
                }
                Request::Ping | Request::Long(_) | Request::Read { .. } => {
                    unreachable!("routed above")
                }
            };
            match admission {
                Ok(admission) => spawn_control_responder(
                    responders,
                    reply_sender,
                    reply_timeouts,
                    command,
                    admission,
                    permit,
                ),
                Err(QueueFull) => queue_reply(reply_sender, reply_timeouts, busy(command)),
            }
        }
    }
}

/// Admit a long verb under the long pool and reply when it resolves.
#[allow(clippy::too_many_arguments)]
fn spawn_long_verb(
    ingress: &PortIngress,
    responders: &mut JoinSet<()>,
    long_permits: &Arc<Semaphore>,
    reply_sender: &tokio_mpsc::Sender<PendingReply>,
    reply_timeouts: &Arc<AtomicU64>,
    command: bus::IncomingCommand,
    op: LongOp,
) {
    let permit = match Arc::clone(long_permits).try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            queue_reply(
                reply_sender,
                reply_timeouts,
                PendingReply::new(command, error("busy")),
            );
            return;
        }
    };
    let admission = match ingress.request_long(op) {
        Ok(admission) => admission,
        Err(QueueFull) => {
            queue_reply(
                reply_sender,
                reply_timeouts,
                PendingReply::new(command, error("busy")),
            );
            return;
        }
    };
    let reply_sender = reply_sender.clone();
    let reply_timeouts = Arc::clone(reply_timeouts);
    responders.spawn(async move {
        let _permit = permit;
        let reply = admission
            .receive()
            .await
            .unwrap_or(ControlReply::Busy)
            .into_wire();
        queue_reply(
            &reply_sender,
            &reply_timeouts,
            PendingReply::new(command, reply),
        );
    });
}

fn spawn_control_responder(
    responders: &mut JoinSet<()>,
    reply_sender: &tokio_mpsc::Sender<PendingReply>,
    reply_timeouts: &Arc<AtomicU64>,
    command: bus::IncomingCommand,
    admission: ControlAdmission,
    permit: tokio::sync::OwnedSemaphorePermit,
) {
    let reply_sender = reply_sender.clone();
    let reply_timeouts = Arc::clone(reply_timeouts);
    responders.spawn(async move {
        let _permit = permit;
        let reply = admission
            .receive()
            .await
            .unwrap_or(ControlReply::Busy)
            .into_wire();
        queue_reply(
            &reply_sender,
            &reply_timeouts,
            PendingReply::new(command, reply),
        );
    });
}

fn queue_reply(
    sender: &tokio_mpsc::Sender<PendingReply>,
    reply_timeouts: &AtomicU64,
    reply: PendingReply,
) {
    if let Err(error) = sender.try_send(reply)
        && matches!(error, tokio_mpsc::error::TrySendError::Full(_))
    {
        reply_timeouts.fetch_add(1, Ordering::AcqRel);
    }
}

async fn reply_loop<C: WorkerClient>(
    client: Arc<C>,
    service: Arc<str>,
    mut replies: tokio_mpsc::Receiver<PendingReply>,
    reply_timeouts: Arc<AtomicU64>,
) {
    while let Some(reply) = replies.recv().await {
        let Some(reply) = enforce_reply_wire_limit(&service, reply) else {
            tracing::debug!(
                service = %service,
                "compositor Bus reply headers exceed the broker WebSocket frame cap"
            );
            continue;
        };
        match tokio::time::timeout(REPLY_SEND_TIMEOUT, client.respond_parts(&reply)).await {
            Err(_) => {
                reply_timeouts.fetch_add(1, Ordering::AcqRel);
                tracing::debug!(
                    command = %reply.command,
                    timeout_ms = REPLY_SEND_TIMEOUT.as_millis(),
                    "compositor Bus reply timed out"
                );
            }
            Ok(Err(error)) => {
                tracing::debug!(%error, command = %reply.command, "compositor Bus reply failed");
            }
            Ok(Ok(())) => {}
        }
    }
}

async fn publisher_loop<C: WorkerClient>(
    client: Arc<C>,
    service: Arc<str>,
    observations: ObservationOutbox,
    observation_notifier: Arc<tokio::sync::Notify>,
    lost_count: Arc<AtomicU64>,
    publish_timeouts: Arc<AtomicU64>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut pending_gap: Option<LossInterval> = None;
    let mut gap_retry_delay = None;
    // The armed deadline outlives a wake that attempts nothing: `sleep_until`
    // it, so elapsed backoff is never discarded by a stale Notify permit.
    let mut gap_retry_deadline: Option<tokio::time::Instant> = None;
    let mut retry_gap_without_data = false;
    let mut connection_states = client.subscribe_state();
    loop {
        let mut lane_empty = false;
        let mut disconnected = false;
        let mut gap_failed_this_pass = false;
        let mut saw_record = false;
        let connection_edge = connection_states.has_changed().unwrap_or(false);
        if connection_edge {
            connection_states.borrow_and_update();
        }

        for _ in 0..observations.capacity {
            let carried = match observations.records.try_recv() {
                Ok(record) => {
                    saw_record = true;
                    record
                }
                Err(crossbeam_channel::TryRecvError::Empty) => {
                    lane_empty = true;
                    break;
                }
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    lane_empty = true;
                    disconnected = true;
                    break;
                }
            };

            if let Some(loss) = carried.preceding_loss {
                merge_pending_gap(&mut pending_gap, loss);
            }
            let record = carried.record;
            let topic_suffix = record.topic_suffix();
            let gap_result = publish_pending_gap_for_topic(
                client.as_ref(),
                &service,
                &lost_count,
                &mut pending_gap,
                topic_suffix,
            )
            .await;
            if gap_result.is_err() {
                publish_timeouts.fetch_add(1, Ordering::AcqRel);
                let (discarded, loss) = discard_publication_backlog(Some(record), &observations);
                lost_count.fetch_add(discarded, Ordering::AcqRel);
                if let Some(loss) = loss {
                    merge_pending_gap(&mut pending_gap, loss);
                }
                arm_gap_retry(&mut gap_retry_delay);
                gap_retry_deadline =
                    gap_retry_delay.map(|delay| tokio::time::Instant::now() + delay);
                gap_failed_this_pass = true;
                break;
            }
            if pending_gap.is_none() {
                gap_retry_delay = None;
                gap_retry_deadline = None;
            }

            let topic = topic_name(&service, topic_suffix);
            let message = bus_message(record.wire());
            if publish_message(client.as_ref(), &topic, &message)
                .await
                .is_ok()
            {
                continue;
            }

            publish_timeouts.fetch_add(1, Ordering::AcqRel);
            let (discarded, loss) = discard_publication_backlog(Some(record), &observations);
            lost_count.fetch_add(discarded, Ordering::AcqRel);
            if let Some(loss) = loss {
                merge_pending_gap(&mut pending_gap, loss);
            }
            arm_gap_retry(&mut gap_retry_delay);
            gap_retry_deadline = gap_retry_delay.map(|delay| tokio::time::Instant::now() + delay);
            gap_failed_this_pass = true;
            break;
        }

        lane_empty |= observations.records.is_empty();
        if lane_empty
            && pending_gap.is_some()
            && !gap_failed_this_pass
            && (saw_record || connection_edge || retry_gap_without_data)
            && publish_pending_gap(client.as_ref(), &service, &lost_count, &mut pending_gap)
                .await
                .is_err()
        {
            publish_timeouts.fetch_add(1, Ordering::AcqRel);
            let (discarded, loss) = discard_publication_backlog(None, &observations);
            lost_count.fetch_add(discarded, Ordering::AcqRel);
            if let Some(loss) = loss {
                merge_pending_gap(&mut pending_gap, loss);
            }
            arm_gap_retry(&mut gap_retry_delay);
            gap_retry_deadline = gap_retry_delay.map(|delay| tokio::time::Instant::now() + delay);
        }
        if pending_gap.is_none() {
            gap_retry_delay = None;
            gap_retry_deadline = None;
        }

        if disconnected {
            if !*shutdown.borrow() {
                // Fail fast, but say so where an operator can see it: a
                // silent task death here would leave the worker running while
                // publication stops and the lane overflows.
                tracing::error!(
                    "observation producer disconnected before port shutdown; publisher task aborting"
                );
            }
            assert!(
                *shutdown.borrow(),
                "observation producer disconnected before port shutdown"
            );
            // WaylandRuntime signals port shutdown before its protocol state
            // drops the sole producer. A pending gap may remain only here,
            // under the accepted bounded-shutdown posture above.
            break;
        }
        retry_gap_without_data =
            match wait_for_publisher_wake(&observation_notifier, &mut shutdown, gap_retry_deadline)
                .await
            {
                PublisherWake::Notified => false,
                PublisherWake::RetryTimer => true,
                PublisherWake::Shutdown => break,
            };
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PublisherWake {
    Notified,
    RetryTimer,
    Shutdown,
}

fn arm_gap_retry(delay: &mut Option<Duration>) {
    *delay = Some(
        delay
            .map(|current| current.saturating_mul(2).min(GAP_RETRY_MAX))
            .unwrap_or(GAP_RETRY_INITIAL),
    );
}

async fn wait_for_publisher_wake(
    notifier: &tokio::sync::Notify,
    shutdown: &mut watch::Receiver<bool>,
    retry_deadline: Option<tokio::time::Instant>,
) -> PublisherWake {
    if let Some(deadline) = retry_deadline {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_ok() && !*shutdown.borrow() {
                    PublisherWake::Notified
                } else {
                    PublisherWake::Shutdown
                }
            }
            _ = notifier.notified() => PublisherWake::Notified,
            _ = tokio::time::sleep_until(deadline) => PublisherWake::RetryTimer,
        }
    } else {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_ok() && !*shutdown.borrow() {
                    PublisherWake::Notified
                } else {
                    PublisherWake::Shutdown
                }
            }
            _ = notifier.notified() => PublisherWake::Notified,
        }
    }
}

fn merge_pending_gap(pending: &mut Option<LossInterval>, loss: LossInterval) {
    if let Some(pending) = pending {
        pending.merge(loss);
    } else {
        *pending = Some(loss);
    }
}

async fn publish_pending_gap_for_topic<C: WorkerClient>(
    client: &C,
    service: &str,
    lost_count: &AtomicU64,
    pending: &mut Option<LossInterval>,
    topic_suffix: &str,
) -> Result<(), ()> {
    let Some(mut gap) = *pending else {
        return Ok(());
    };
    if !gap.topics.contains(topic_suffix) {
        return Ok(());
    }
    let topic = topic_name(service, topic_suffix);
    let message = gap_message(topic_suffix, gap, lost_count.load(Ordering::Acquire));
    publish_message(client, &topic, &message).await?;
    gap.topics.remove(topic_suffix);
    *pending = (!gap.topics.is_empty()).then_some(gap);
    Ok(())
}

async fn publish_pending_gap<C: WorkerClient>(
    client: &C,
    service: &str,
    lost_count: &AtomicU64,
    pending: &mut Option<LossInterval>,
) -> Result<(), ()> {
    let Some(gap) = *pending else {
        return Ok(());
    };
    let mut remaining = gap;
    for suffix in gap.topics.iter() {
        let topic = topic_name(service, suffix);
        let message = gap_message(suffix, gap, lost_count.load(Ordering::Acquire));
        if publish_message(client, &topic, &message).await.is_err() {
            *pending = Some(remaining);
            return Err(());
        }
        remaining.topics.remove(suffix);
    }
    *pending = None;
    Ok(())
}

fn discard_publication_backlog(
    failed: Option<ObservationRecord>,
    observations: &ObservationOutbox,
) -> (u64, Option<LossInterval>) {
    let mut discarded = 0_u64;
    let mut loss: Option<LossInterval> = None;
    let mut absorb = |interval: LossInterval| {
        if let Some(current) = loss.as_mut() {
            current.merge(interval);
        } else {
            loss = Some(interval);
        }
    };
    if let Some(failed) = failed {
        discarded = 1;
        absorb(LossInterval::from_record(&failed, LossCause::PublisherLoss));
    }
    for _ in 0..observations.capacity {
        let Ok(record) = observations.records.try_recv() else {
            break;
        };
        if let Some(preceding) = record.preceding_loss {
            absorb(preceding);
        }
        discarded = discarded.saturating_add(1);
        absorb(LossInterval::from_record(
            &record.record,
            LossCause::PublisherLoss,
        ));
    }
    (discarded, loss)
}

async fn publish_message<C: WorkerClient>(
    client: &C,
    topic: &str,
    message: &BusMessage,
) -> Result<(), ()> {
    let mut headers = BTreeMap::new();
    headers.insert("name".to_string(), topic.to_string());
    headers.insert("retain".to_string(), "false".to_string());
    let wire = message.to_wire();
    match tokio::time::timeout(PUBLISH_TIMEOUT, client.publish(&headers, &wire)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => {
            tracing::debug!(%error, topic, "compositor Bus topic publication failed");
            Err(())
        }
        Err(_) => {
            tracing::debug!(
                topic,
                timeout_ms = PUBLISH_TIMEOUT.as_millis(),
                "compositor Bus topic publication timed out"
            );
            Err(())
        }
    }
}

fn gap_message(topic_suffix: &str, gap: LossInterval, lost_count: u64) -> BusMessage {
    let mut message = BusMessage::new()
        .with_header("command", topic_suffix)
        .with_header("event_seq", &gap.last_lost_seq.to_string());
    message.body = json!({
        "gap": true,
        "lost_count": lost_count,
        "cause": gap.cause.as_str(),
    })
    .to_string();
    message
}

/// Exact byte count produced by `NodedClient::respond_parts` for this reply.
/// The body stays borrowed: only the small canonical header block is assembled.
fn reply_wire_bytes(service: &str, reply: &PendingReply) -> usize {
    let mut message = BusMessage::new()
        .with_header("command", &reply.command)
        .with_header("from", service)
        .with_header("to", &reply.from)
        .with_header("type", "response")
        .with_header("rc", &reply.rc.to_string());
    if let Some(id) = reply.id.as_deref() {
        message = message.with_header("id", id);
    }
    let header_and_framing = message.to_wire().len();
    header_and_framing
        .checked_add(reply.body.len())
        .and_then(|bytes| {
            bytes.checked_add(usize::from(
                !reply.body.is_empty() && !reply.body.ends_with('\n'),
            ))
        })
        .unwrap_or(usize::MAX)
}

fn enforce_reply_wire_limit(service: &str, mut reply: PendingReply) -> Option<PendingReply> {
    if reply_wire_bytes(service, &reply) > MAX_REPLY_WIRE_BYTES {
        let (rc, body) = too_large(MAX_REPLY_BODY_BYTES);
        (reply.rc, reply.body) = with_error_code(rc, body);
    }
    (reply_wire_bytes(service, &reply) <= MAX_REPLY_WIRE_BYTES).then_some(reply)
}

fn random_instance_id() -> Result<String, String> {
    let mut bytes = [0_u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut source| source.read_exact(&mut bytes))
        .map_err(|error| format!("failed to seed compositor Bus instance id: {error}"))?;
    let mut id = String::with_capacity(32);
    for byte in bytes {
        write!(&mut id, "{byte:02x}").map_err(|error| error.to_string())?;
    }
    Ok(id)
}

/// Frame a comp-model topic payload as a Bus message (same sorted headers,
/// same body).
fn bus_message(message: TopicMessage) -> BusMessage {
    BusMessage {
        headers: message.headers,
        body: message.body,
    }
}

#[cfg(test)]
#[path = "port_tests.rs"]
mod tests;

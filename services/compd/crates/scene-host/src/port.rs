//! The scene host's own Bus registration.
//!
//! A second Bus client beside the comp port, because the contract's
//! host is Quoin's `shell` service, not `comp`: it answers
//! `shell.scene.*`, publishes `<service>.scene.changed`, and sends scene
//! events to the documents' citizens. It runs on its own worker thread with a
//! current-thread tokio runtime, as comp-service does, and hands requests to the
//! engine over a bounded channel plus the engine's waker. Nothing polls.
//!
//! The name: `shell` when free. When the broker refuses it, the
//! refusal is logged loudly and the host registers under the configured
//! override (`--scene-service NAME` / preference `scene_service`) or not at
//! all. It never silently takes a different name.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use crate::appearance::Look;
use application::native_actor::{Accepted, Faults, Reply as NativeReply, TaskSet, cancel, reap};
use application::native_queue::Admission;
use application::presentation::native::{
    Event as SettingsEvent, Lane as SettingsLane, Progress, Session, Ui as SettingsUi,
    Worker as SettingsWorker, bridge,
};
use bus::native_client::BoundedIncomingEvent;
use bus::{ConnState, IncomingCommand, SupervisedClient};
use serde_json::{Value, json};
use tokio::sync::{mpsc as tokio_mpsc, watch};

use crate::verb::SceneVerb;

/// The contract's host name.
pub const DEFAULT_SERVICE: &str = "shell";
/// The broker's registry topic (comp-model `REGISTRY_TOPIC`).
pub const REGISTRY_TOPIC: &str = "noded.props.changed";
/// Requests the engine has not taken yet; past this the worker answers
/// rc 11 `QUEUE_FULL` itself.
pub const INBOUND_CAPACITY: usize = 64;
pub const DESCRIPTION_CAPACITY: usize = 8;
/// Scene events in flight to citizens (contract: at most 128 pending).
pub const MAX_PENDING_EVENTS: usize = 128;
/// A citizen's handler answers within this, or the event is abandoned.
pub const EVENT_TIMEOUT: Duration = Duration::from_secs(2);
const SEND_TIMEOUT: Duration = Duration::from_secs(2);
const RETRY_INITIAL: Duration = Duration::from_secs(1);
const SHUTDOWN_BUDGET: Duration = Duration::from_secs(2);
const SHUTDOWN_GRACE: Duration = Duration::from_millis(2300);
const RUNTIME_GRACE: Duration = Duration::from_millis(100);
const DEREGISTER_BUDGET: Duration = Duration::from_millis(200);

/// Wakes the engine's loop; called once per delivered message.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// How the host registers and where the broker is.
#[derive(Clone, Debug)]
pub struct HostConfig {
    /// Version of the executable embedding this host, supplied by compd.
    pub owner_version: String,
    /// The name to fall back to when `shell` is refused; `None` refuses.
    pub service_override: Option<String>,
    /// The comp port's URL resolution (`comp_service::default_noded_url()`:
    /// `MIXOS_NODED_URL`, else the node config, else the loopback broker).
    pub noded_url: String,
}

/// One admitted scene request.
#[derive(Clone, Debug)]
pub struct Request {
    pub verb: SceneVerb,
    pub from: String,
    pub command: String,
    pub id: Option<String>,
    pub body: String,
    pub headers: BTreeMap<String, String>,
    /// The broker connection it was admitted on
    /// (`SupervisedClient::connection_generation`). A request from an older
    /// connection is refused unapplied, and no reply crosses a reconnect.
    pub generation: u64,
}

/// What the worker hands the engine.
#[derive(Debug)]
pub enum Inbound {
    /// Registered (or re-registered after the first) under this name.
    Registered(String),
    /// Initial registration was refused, or the established connection ended
    /// terminally. Scene RPCs are unavailable; settings resources remain active.
    Refused(String),
    Request(Request),
    /// The broker's full set of registered services, from a registry diff.
    Live(BTreeSet<String>),
    Registrations(BTreeMap<String, String>),
    Presentation(PresentationNotice),
}

#[derive(Clone, Debug, PartialEq)]
pub struct PresentationNotice {
    pub service: String,
    pub registration: String,
    pub sequence: u64,
    pub generation: u64,
    pub provenance: Value,
    pub value: Value,
}

enum Outbound {
    Description(NativeReply),
    Reply {
        to: String,
        command: String,
        id: Option<String>,
        rc: u8,
        body: String,
        generation: u64,
    },
    /// A topic wire: `<service>.scene.changed`, or with `panel`
    /// `<service>.panel.changed`, retained as Quoin's is (a loader that
    /// subscribes late still reads the current panels).
    Publish {
        panel: bool,
        wire: String,
    },
    Participants { topic: String, wire: String, generation: u64 },
    ParticipantsReady(Arc<Mutex<Option<ParticipantPublication>>>),
}

struct ParticipantPublication {topic:String,wire:String,generation:u64}

struct Event {
    to: String,
    verb: String,
    body: Value,
}

struct PendingEvent(Arc<AtomicUsize>);
impl Drop for PendingEvent {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// The engine's end of the port.
pub struct Port {
    inbound: Receiver<Inbound>,
    descriptions: Mutex<tokio_mpsc::Receiver<Accepted>>,
    outbound: tokio_mpsc::UnboundedSender<Outbound>,
    sink: EventSink,
    shutdown: watch::Sender<bool>,
    completion: Receiver<()>,
    thread: Option<thread::JoinHandle<()>>,
    /// The registered client, once there is one: the engine reads the live
    /// connection generation from it directly (an atomic load), so a
    /// reconnect the worker has not yet reported still fences.
    client: Arc<OnceLock<Arc<SupervisedClient>>>,
    settings_ui: Option<SettingsUi<Look>>,
    registry: RegistryMailbox,
}

#[derive(Default)]
struct RegistryPending {
    names: Option<(u64, BTreeSet<String>)>,
    registrations: Option<(u64, BTreeMap<String, String>)>,
    presentations: BTreeMap<String, PresentationNotice>,
    observations: Option<watch::Sender<crate::participant_wait::History>>,
    participant_publication: Arc<Mutex<Option<ParticipantPublication>>>,
}
impl RegistryPending {
    fn take(&mut self)->Option<(u64,BTreeSet<String>)> {self.names.take()}
}
type RegistryMailbox = Arc<Mutex<RegistryPending>>;

#[derive(Default)]
struct PresentationSubscriptions {
    generation: u64,
    // Include a subscribe before sending it: a timed-out reply may still
    // have installed broker state. Only an acknowledged unsubscribe removes it.
    possible: BTreeSet<String>,
    desired: BTreeSet<String>,
    dirty: bool,
}
impl PresentationSubscriptions {
    fn reset(&mut self, generation:u64) {
        *self = Self {generation,..Self::default()};
    }
    fn desire(&mut self, next:BTreeSet<String>) {
        self.desired = next;
        self.dirty = true;
    }
    fn subscribing(&mut self, generation:u64, service:&str)->Result<(),String> {
        if self.generation != generation {return Err("subscription generation retired".into());}
        if self.possible.len() >= 128 && !self.possible.contains(service) {
            return Err("presentation subscription capacity".into());
        }
        self.possible.insert(service.to_owned());
        Ok(())
    }
    fn unsubscribed(&mut self, generation:u64, service:&str) {
        if self.generation == generation {self.possible.remove(service);}
    }
}

/// The names to try, in order: `shell`, then the override when it differs.
pub fn candidate_names(service_override: Option<&str>) -> Vec<String> {
    let mut names = vec![DEFAULT_SERVICE.to_owned()];
    if let Some(name) = service_override
        .map(str::trim)
        .filter(|name| !name.is_empty() && *name != DEFAULT_SERVICE)
    {
        names.push(name.to_owned());
    }
    names
}

impl Port {
    /// Start the worker. The broker connection is the worker's: a missing
    /// noded is retried with backoff and never blocks the compositor.
    pub fn start(config: HostConfig, waker: Waker) -> Result<Self, String> {
        Self::start_at(
            config,
            waker,
            config::path(config::Dir::Var).join("compd/cache/settings"),
        )
    }

    fn start_at(
        config: HostConfig,
        waker: Waker,
        cache_directory: std::path::PathBuf,
    ) -> Result<Self, String> {
        let binding =
            settings::session::binding().map_err(|error| format!("settings session: {error:?}"))?;
        let consumer = settings::consumer::Consumer::for_shell(binding)
            .map_err(|error| format!("shell settings: {error:?}"))?;
        let settings_worker =
            SettingsWorker::offline_with_cache(cache_directory, crate::appearance::build);
        let (ui, lane) = bridge(Session::new(consumer), settings_worker);
        let registry = RegistryMailbox::default();
        registry.lock().unwrap().observations = Some(watch::channel(crate::participant_wait::History::default()).0);
        let worker_registry = Arc::clone(&registry);
        let (inbound_tx, inbound) = mpsc::sync_channel(INBOUND_CAPACITY);
        let (description_tx, descriptions) = tokio_mpsc::channel(DESCRIPTION_CAPACITY);
        let (outbound, outbound_rx) = tokio_mpsc::unbounded_channel();
        let (events, events_rx) = tokio_mpsc::unbounded_channel();
        let (shutdown, shutdown_rx) = watch::channel(false);
        let (completion_tx, completion) = mpsc::sync_channel(1);
        let pending_events = Arc::new(AtomicUsize::new(0));
        let pending = Arc::clone(&pending_events);
        let client: Arc<OnceLock<Arc<SupervisedClient>>> = Arc::new(OnceLock::new());
        let published = Arc::clone(&client);
        let thread = thread::Builder::new()
            .name("compd-scenes".into())
            .spawn(move || {
                let _completion = CompletionOnDrop(completion_tx);
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        tracing::error!(%error, "scene host: failed to build its Bus runtime");
                        return;
                    }
                };
                let delivery = Delivery {
                    sender: inbound_tx,
                    descriptions: description_tx,
                    waker,
                };
                runtime.block_on(worker(
                    config,
                    delivery,
                    outbound_rx,
                    events_rx,
                    pending,
                    shutdown_rx,
                    published,
                    lane,
                    worker_registry,
                ));
                runtime.shutdown_timeout(RUNTIME_GRACE);
            })
            .map_err(|error| format!("failed to spawn the scene host's Bus worker: {error}"))?;
        let sink = EventSink {
            events,
            pending: pending_events,
        };
        Ok(Self {
            inbound,
            descriptions: Mutex::new(descriptions),
            outbound,
            sink,
            shutdown,
            completion,
            thread: Some(thread),
            client,
            settings_ui: Some(ui),
            registry,
        })
    }

    pub(crate) fn take_settings_ui(&mut self) -> SettingsUi<Look> {
        self.settings_ui
            .take()
            .expect("settings UI taken only once")
    }
    pub(crate) fn settings_generation(&self) -> Option<u64> {
        self.client
            .get()
            .and_then(|client| settings::native::live_generation(client))
    }
    pub(crate) fn registered_service_name(&self) -> Option<&str> {
        self.client.get().map(|client| client.service_name())
    }
    pub(crate) fn try_description(&self) -> Option<Accepted> {
        self.descriptions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .try_recv()
            .ok()
    }
    pub(crate) fn description_is_current(&self, request: &Accepted) -> bool {
        self.client
            .get()
            .is_some_and(|client| request.is_current(client))
    }
    pub(crate) fn reply_description(&self, request: Accepted, rc: u8, body: String) {
        let reply = request.reply(rc, body, std::time::Instant::now() + SEND_TIMEOUT);
        if let Err(error) = self.outbound.send(Outbound::Description(reply))
            && let Outbound::Description(reply) = error.0
        {
            tracing::warn!("scene host: description reply retired after worker closure");
            reply.retire().finish();
        }
    }

    /// The live broker connection's generation; `None` before registering.
    pub fn connection_generation(&self) -> Option<u64> {
        self.client
            .get()
            .map(|client| client.connection_generation())
    }

    /// The next message from the worker, without blocking.
    pub fn try_recv(&self) -> Option<Inbound> {
        self.inbound.try_recv().ok().or_else(|| {
            let (generation, services) = self
                .registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()?;
            (self.settings_generation() == Some(generation)).then_some(Inbound::Live(services))
        }).or_else(|| self.try_observation())
    }

    pub(crate) fn try_observation(&self) -> Option<Inbound> {
            let mut pending = self.registry.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some((generation, registrations)) = pending.registrations.take() {
                return (self.settings_generation() == Some(generation)).then_some(Inbound::Registrations(registrations));
            }
            let (_, notice) = pending.presentations.pop_first()?;
            (self.settings_generation() == Some(notice.generation)).then_some(Inbound::Presentation(notice))
    }

    pub fn reply(&self, request: &Request, rc: u8, body: String) {
        let _ = self.outbound.send(Outbound::Reply {
            generation: request.generation,
            to: request.from.clone(),
            command: request.command.clone(),
            id: request.id.clone(),
            rc,
            body,
        });
    }

    /// Publish one `<service>.scene.changed` wire, in call order.
    pub fn publish(&self, wire: String) {
        let _ = self.outbound.send(Outbound::Publish { panel: false, wire });
    }

    /// Publish one `<service>.panel.changed` wire, retained.
    pub fn publish_panel(&self, wire: String) {
        let _ = self.outbound.send(Outbound::Publish { panel: true, wire });
    }

    pub(crate) fn publish_participants(&self, value: Value) {
        if let Some(observations) = &self.registry.lock().unwrap_or_else(std::sync::PoisonError::into_inner).observations {
            observations.send_if_modified(|history|history.observe(value.clone(),std::time::Instant::now()));
        }
        let (Some(service),Some(generation)) = (self.registered_service_name(),self.settings_generation()) else {return;};
        let topic = format!("{service}.participants.changed");
        let mut message = bus::wire::BusMessage::new();
        message.set("command", &topic);
        message.body = value.to_string();
        let pending=self.registry.lock().unwrap_or_else(std::sync::PoisonError::into_inner).participant_publication.clone();
        let first=pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
            .replace(ParticipantPublication {topic,wire:message.to_wire(),generation}).is_none();
        if first {let _ = self.outbound.send(Outbound::ParticipantsReady(pending));}
    }

    /// A scene event: a directed request to `citizen`, verb = the handler
    /// name, body `{scene,node,kind,value?,item?}`. False when the
    /// contract's 128 pending events are already in flight (dropped, logged).
    pub fn emit(&self, citizen: &str, verb: &str, body: Value) -> bool {
        self.sink.emit(citizen, verb, body)
    }

    /// A sender for scene events that any thread may hold (the iced
    /// surfaces' message handlers).
    pub fn event_sink(&self) -> EventSink {
        self.sink.clone()
    }

    /// Deregister and stop, bounded.
    pub fn finish(mut self) {
        freeze_descriptions(&self.descriptions);
        let _ = self.shutdown.send(true);
        let Some(thread) = self.thread.take() else {
            return;
        };
        match self.completion.recv_timeout(SHUTDOWN_GRACE) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                if thread.join().is_err() {
                    tracing::error!("scene host Bus worker panicked during shutdown");
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                tracing::warn!("scene host Bus worker did not stop in time and was detached");
            }
        }
    }
}

fn freeze_descriptions(descriptions: &Mutex<tokio_mpsc::Receiver<Accepted>>) {
    let mut descriptions = descriptions
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Closing admission before draining also protects the detached worker:
    // no later accepted request can enter this receiver.
    descriptions.close();
    while let Ok(request) = descriptions.try_recv() {
        tracing::debug!("scene host: queued description retired during engine shutdown");
        request.retire().finish();
    }
}

/// Sends scene events through the worker, bounded at
/// [`MAX_PENDING_EVENTS`] in flight.
#[derive(Clone)]
pub struct EventSink {
    events: tokio_mpsc::UnboundedSender<Event>,
    pending: Arc<AtomicUsize>,
}

impl EventSink {
    /// False when 128 events are already pending (dropped, logged) or the
    /// worker is gone.
    pub fn emit(&self, citizen: &str, verb: &str, body: Value) -> bool {
        if self.pending.fetch_add(1, Ordering::AcqRel) >= MAX_PENDING_EVENTS {
            self.pending.fetch_sub(1, Ordering::AcqRel);
            tracing::warn!(
                citizen,
                verb,
                "scene event dropped: {MAX_PENDING_EVENTS} events already pending"
            );
            return false;
        }
        let event = Event {
            to: citizen.to_owned(),
            verb: verb.to_owned(),
            body,
        };
        if self.events.send(event).is_err() {
            self.pending.fetch_sub(1, Ordering::AcqRel);
            return false;
        }
        true
    }
}

struct CompletionOnDrop(SyncSender<()>);

impl Drop for CompletionOnDrop {
    fn drop(&mut self) {
        let _ = self.0.try_send(());
    }
}

#[derive(Clone)]
struct Delivery {
    sender: SyncSender<Inbound>,
    descriptions: tokio_mpsc::Sender<Accepted>,
    waker: Waker,
}

impl Delivery {
    /// False when the engine's queue is full or gone (the message is
    /// dropped; every caller answers or logs that itself).
    fn send(&self, message: Inbound) -> bool {
        let sent = self.sender.try_send(message).is_ok();
        if sent {
            (self.waker)();
        }
        sent
    }
}

/// Where one incoming frame goes.
#[derive(Debug, PartialEq)]
pub enum Route {
    AppDescribe,
    ParticipantWait,
    Scene(SceneVerb),
    Live(BTreeSet<String>),
    Registrations(BTreeMap<String, String>),
    /// A registry claim not from the local broker; dropped, logged.
    Forged,
    /// A request for a verb this host does not serve.
    Unknown,
    /// A topic delivery or a reply: nothing to answer.
    Ignore,
}

/// Route one incoming frame for the host registered as `service`.
pub fn route(service: &str, command: &IncomingCommand) -> Route {
    if command.from == "noded" && command.topic() == Some(REGISTRY_TOPIC) {
        if !from_local_broker(command) {
            return Route::Forged;
        }
        if let Some(registrations) = serde_json::from_str::<Value>(&command.body).ok()
            .filter(|body| body["path"] == "services.incarnations")
            .and_then(|body| decode_registrations(&body["new"])) {
            return Route::Registrations(registrations);
        }
        return serde_json::from_str::<Value>(&command.body)
            .ok()
            .filter(|body| body["path"] == "services.registered")
            .and_then(|body| {
                body["new"]
                    .as_array()?
                    .iter()
                    .map(|name| name.as_str().map(str::to_owned))
                    .collect::<Option<BTreeSet<String>>>()
            })
            .map_or(Route::Ignore, Route::Live);
    }
    if command.is_topic_delivery() || command.command.is_empty() {
        return Route::Ignore;
    }
    if command.command == "app.describe" {
        return Route::AppDescribe;
    }
    if command.command == "app.participants.wait" {return Route::ParticipantWait;}
    match SceneVerb::parse(service, &command.command) {
        Some(verb) => Route::Scene(verb),
        None if command.id.is_some() => Route::Unknown,
        None => Route::Ignore,
    }
}

/// Only the local broker speaks as `noded`.
fn from_local_broker(command: &IncomingCommand) -> bool {
    command
        .headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("broker_origin"))
        .all(|(_, origin)| origin.eq_ignore_ascii_case("local"))
}

enum Connected {
    Client(Arc<SupervisedClient>, String),
    Refused(String),
    Stopped,
}

async fn connect(config: HostConfig, mut shutdown: watch::Receiver<bool>) -> Connected {
    let names = candidate_names(config.service_override.as_deref());
    let mut refusals = Vec::new();
    for name in &names {
        let client = Arc::new(
            SupervisedClient::connect_options(name, &config.noded_url)
                .fatal_on_registration_rejection(true)
                .bounded_incoming(INBOUND_CAPACITY)
                .start(),
        );
        let mut lifecycle = client.subscribe_state();
        loop {
            // A Connected edge may have coalesced with subsequent loss or
            // rejection. Once established, this identity must never fall back.
            if client.connection_generation() > 0 {
                return Connected::Client(client, name.clone());
            }
            let state = *lifecycle.borrow_and_update();
            if matches!(state, ConnState::Fatal | ConnState::ShuttingDown) {
                // Registration may have completed between the first generation
                // sample and this terminal state read.
                if client.connection_generation() > 0 {
                    return Connected::Client(client, name.clone());
                }
                let refusal = client
                    .registration_rejection()
                    .map(|refusal| format!("rc {}: {}", refusal.rc, refusal.message));
                if tokio::time::timeout(DEREGISTER_BUDGET, client.close())
                    .await
                    .is_err()
                {
                    return Connected::Refused(format!(
                        "{name}: rejected supervisor did not retire; no fallback attempted"
                    ));
                }
                let Some(refusal) = refusal else {
                    return Connected::Refused(format!(
                        "{name}: supervisor stopped without an initial registration refusal"
                    ));
                };
                tracing::error!("SCENE HOST: the broker refused the Bus name `{name}` ({refusal})");
                refusals.push(format!("{name}: {refusal}"));
                break;
            }
            tokio::select! {
                changed = lifecycle.changed() => {
                    if changed.is_err() { return Connected::Refused(format!("{name}: supervisor lifecycle closed")); }
                }
                _ = shutdown.changed() => return Connected::Stopped,
            }
        }
        if let Some(next) = names
            .iter()
            .skip_while(|candidate| *candidate != name)
            .nth(1)
        {
            tracing::error!("SCENE HOST: falling back to the configured override name `{next}`");
        }
    }
    let reason = if names.len() == 1 {
        format!(
            "{}; no --scene-service / scene_service override is configured, so the scene host is OFF",
            refusals.join("; ")
        )
    } else {
        format!("{}; the scene host is OFF", refusals.join("; "))
    };
    tracing::error!("SCENE HOST: {reason}");
    Connected::Refused(reason)
}

#[allow(clippy::too_many_arguments)]
async fn worker(
    config: HostConfig,
    delivery: Delivery,
    mut outbound: tokio_mpsc::UnboundedReceiver<Outbound>,
    mut events: tokio_mpsc::UnboundedReceiver<Event>,
    pending_events: Arc<AtomicUsize>,
    mut shutdown: watch::Receiver<bool>,
    published: Arc<OnceLock<Arc<SupervisedClient>>>,
    mut lane: SettingsLane<Look>,
    registry_mailbox: RegistryMailbox,
) {
    let waker = Arc::clone(&delivery.waker);
    let notify = move |needed| {
        if needed {
            waker();
        }
    };
    let mut connecting = Some(Box::pin(connect(config.clone(), shutdown.clone())));
    // The existing connection attempt must not stop the resource worker. A
    // refused service still receives offline presentation work until shutdown.
    let connected = loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() { break None; }
            }
            progress = lane.drive() => {
                match progress {
                    Progress::Wake => notify(true),
                    Progress::UiClosed => break None,
                    Progress::Updated => {}
                }
            }
            result = async { connecting.as_mut().expect("guarded connection").await }, if connecting.is_some() => {
                connecting = None;
                match result {
                    Connected::Client(client, service) => break Some((client, service)),
                    Connected::Refused(reason) => { let _ = delivery.send(Inbound::Refused(reason)); }
                    Connected::Stopped => break None,
                }
            }
        }
    };
    // Cancellation drops the unpublished candidate and signals its supervisor
    // before the common cache drain and bounded runtime shutdown.
    drop(connecting.take());
    let deadline = if let Some((client, service)) = &connected {
        let _ = published.set(Arc::clone(client));
        notify(lane.connect(Arc::clone(client)));
        tracing::info!(
            "scene host: registered as `{service}` via {}",
            config.noded_url
        );
        if !delivery.send(Inbound::Registered(service.clone())) {
            tracing::warn!("scene host: the engine's queue refused the registration notice");
        }
        serve(
            client,
            service,
            &delivery,
            &mut outbound,
            &mut events,
            pending_events,
            &mut shutdown,
            &mut lane,
            &registry_mailbox,
            &notify,
        )
        .await
    } else {
        std::time::Instant::now() + SHUTDOWN_BUDGET
    };
    if let Err(error) = lane.flush_cache(deadline).await {
        tracing::warn!(?error, "scene host settings cache drain failed");
    }
    if let Some((client, _)) = connected {
        let close_deadline = tokio::time::Instant::from_std(deadline);
        let deregister_deadline =
            close_deadline.min(tokio::time::Instant::now() + DEREGISTER_BUDGET);
        match tokio::time::timeout_at(deregister_deadline, client.deregister()).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::debug!(%error, "scene host deregister did not complete cleanly")
            }
            Err(_) => tracing::warn!("scene host deregister timed out"),
        }
        if tokio::time::timeout_at(close_deadline, client.close())
            .await
            .is_err()
        {
            tracing::warn!("scene host Bus close exceeded shutdown budget");
        }
    }
    notify(lane.publish(SettingsEvent::Wake));
}

#[allow(clippy::too_many_arguments)]
async fn serve(
    client: &Arc<SupervisedClient>,
    service: &str,
    delivery: &Delivery,
    outbound: &mut tokio_mpsc::UnboundedReceiver<Outbound>,
    events: &mut tokio_mpsc::UnboundedReceiver<Event>,
    pending_events: Arc<AtomicUsize>,
    shutdown: &mut watch::Receiver<bool>,
    lane: &mut SettingsLane<Look>,
    registry_mailbox: &RegistryMailbox,
    notify: &impl Fn(bool),
) -> std::time::Instant {
    let Some(mut incoming) = client.incoming_bounded() else {
        tracing::error!("scene host: the Bus client has no incoming lane; the host is OFF");
        let _ = delivery.send(Inbound::Refused("no incoming lane".into()));
        return std::time::Instant::now() + SHUTDOWN_BUDGET;
    };
    let mut lifecycle = client.subscribe_state();
    let mut lifecycle_open = true;
    let mut incoming_open = true;
    let initial_state = *lifecycle.borrow_and_update();
    if matches!(initial_state, ConnState::Fatal | ConnState::ShuttingDown) {
        let _ = delivery.send(Inbound::Refused(terminal_reason(client)));
    }
    let topics = (
        format!("{service}.scene.changed"),
        format!("{service}.panel.changed"),
    );
    let mut flights = tokio::task::JoinSet::new();
    let mut replies = tokio::task::JoinSet::new();
    let mut sends = tokio::task::JoinSet::new();
    let description_admission = Admission::new(DESCRIPTION_CAPACITY);
    let mut description_replies = TaskSet::new(DESCRIPTION_CAPACITY);
    let mut description_faults = Faults::default();
    let mut registry = None;
    let mut registrations = BTreeMap::new();
    let mut subscriptions = None;
    let subscription_state = Arc::new(Mutex::new(PresentationSubscriptions::default()));
    subscription_state.lock().unwrap().reset(client.connection_generation());
    let mut registry_retries = 0;
    let mut registry_subscription = (initial_state == ConnState::Connected)
        .then(|| Box::pin(registry_subscribe(Arc::clone(client))));
    loop {
        // Reconcile one acknowledged snapshot at a time. Registry updates
        // only coalesce desired state and cannot cancel an unfinished removal.
        if subscriptions.is_none() {
            let mut state = subscription_state.lock().unwrap();
            if state.dirty && settings::native::live_generation(client) == Some(state.generation) {
                state.dirty = false;
                subscriptions = Some(Box::pin(presentation_subscriptions(Arc::clone(client),
                    Arc::clone(&subscription_state),state.generation,state.possible.clone(),state.desired.clone())));
            }
        }
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            progress = lane.drive() => {
                match progress {
                    Progress::Wake => notify(true),
                    Progress::UiClosed => break,
                    Progress::Updated => {}
                }
            }
            changed = lifecycle.changed(), if lifecycle_open => {
                if changed.is_err() { lifecycle_open = false; }
                let state = *lifecycle.borrow_and_update();
                notify(lane.publish(SettingsEvent::Wake));
                registry = None;
                subscriptions = None;
                subscription_state.lock().unwrap().reset(client.connection_generation());
                registrations.clear();
                {
                    let mut pending=registry_mailbox.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    pending.presentations.clear();
                    pending.registrations=Some((client.connection_generation(),BTreeMap::new()));
                    if let Some(observations)=&pending.observations {observations.send_replace(crate::participant_wait::History::default());}
                }
                registry_subscription = (lifecycle_open && state == ConnState::Connected)
                    .then(|| Box::pin(registry_subscribe(Arc::clone(client))));
                if !lifecycle_open || matches!(state, ConnState::Fatal | ConnState::ShuttingDown) {
                    let _ = delivery.send(Inbound::Refused(terminal_reason(client)));
                }
                registry_retries = 0;
            }
            subscribed = async { registry_subscription.as_mut().expect("guarded registry subscription").await }, if registry_subscription.is_some() => {
                registry_subscription = None;
                if subscribed {
                    // Subscribe before read so a departure in the gap cannot
                    // leave a successful but obsolete full-set baseline.
                    registry = Some(Box::pin(registry_read(Arc::clone(client), Duration::ZERO)));
                    registry_retries = 0;
                }
            }
            result = async { registry.as_mut().expect("guarded registry read").await }, if registry.is_some() => {
                registry = None;
                if let Some((generation, services, identities)) = result {
                    if settings::native::live_generation(client) == Some(generation) {
                        let mut pending = registry_mailbox.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        pending.names = Some((generation, services));
                        pending.registrations = Some((generation, identities.clone()));
                        subscription_state.lock().unwrap().desire(identities.keys().cloned().collect());
                        registrations = identities;
                        (delivery.waker)();
                    }
                } else if settings::native::live_generation(client).is_some() && registry_retries < 3 {
                    let delay = RETRY_INITIAL * (1 << registry_retries);
                    registry_retries += 1;
                    registry = Some(Box::pin(registry_read(Arc::clone(client), delay)));
                } else {
                    tracing::warn!("scene host: registry recovery paused until next lifecycle or loss event");
                }
            }
            command = incoming.recv(), if incoming_open && replies.len() < INBOUND_CAPACITY => {
                match command {
                    Some(BoundedIncomingEvent::Command(command)) => {
                        if let Some(notice) = presentation_notice(&command, &registrations) {
                            if settings::native::live_generation(client) == Some(command.generation) {
                                let mut pending = registry_mailbox.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                                if pending.presentations.len() < 128 || pending.presentations.contains_key(&notice.service) {
                                    let newer = pending.presentations.get(&notice.service).is_none_or(|old| old.registration != notice.registration || old.sequence < notice.sequence);
                                    if newer {pending.presentations.insert(notice.service.clone(), notice); (delivery.waker)();}
                                }
                            }
                        } else if let Route::Registrations(identities) = route(service, &command) {
                            if settings::native::live_generation(client) == Some(command.generation) {
                                registry = None;
                                let mut pending = registry_mailbox.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                                pending.names = Some((command.generation, identities.keys().cloned().collect()));
                                pending.registrations = Some((command.generation, identities.clone()));
                                subscription_state.lock().unwrap().desire(identities.keys().cloned().collect());
                                registrations = identities;
                                (delivery.waker)();
                            }
                        } else if let Some(wake) = lane.delivery(&command) {
                            notify(wake);
                        } else if let Route::Live(services) = route(service, &command) {
                            if settings::native::live_generation(client) == Some(command.generation) {
                                // Each registry notice is a full set. It supersedes
                                // an older read and cannot be dropped by scene RPCs.
                                registry = None;
                                registry_mailbox.lock().unwrap_or_else(std::sync::PoisonError::into_inner).names = Some((command.generation, services));
                                (delivery.waker)();
                            }
                        } else {
                            let refused = if route(service,&command) == Route::ParticipantWait {
                                admit_participant_wait(client,registry_mailbox,command,&description_admission,&mut description_replies)
                            } else if route(service, &command) == Route::AppDescribe {
                                admit_description(client, delivery, command, &description_admission)
                            } else { admit(service, delivery, command) };
                            if let Some((command, rc, body)) = refused {
                                let client = Arc::clone(client);
                                replies.spawn(async move { refuse(&client, &command, rc, body).await; });
                            }
                        }
                    }
                    Some(BoundedIncomingEvent::Overflow { .. }) => {
                        notify(lane.publish(SettingsEvent::Lost));
                        registry = Some(Box::pin(registry_read(Arc::clone(client), Duration::ZERO)));
                        registry_retries = 0;
                        tracing::warn!("scene host: incoming queue overflow; recovering settings and registry");
                    }
                    None => {
                        incoming_open = false;
                        registry = None;
                        registry_subscription = None;
                        notify(lane.publish(SettingsEvent::Wake));
                        let _ = delivery.send(Inbound::Refused("Bus incoming lane closed; retaining settings resources".into()));
                    }
                }
            }
            Some(message) = outbound.recv(), if sends.is_empty() => {
                if let Outbound::Description(reply) = message {
                    submit_description_reply(reply, &mut description_replies, &mut description_faults);
                    continue;
                }
                let (client, topics) = (Arc::clone(client), topics.clone());
                // Preserve publish/reply order without blocking settings work.
                sends.spawn(async move { send(&client, &topics, message).await; });
            }
            result = async {subscriptions.as_mut().expect("guarded presentation subscription").await}, if subscriptions.is_some() => {
                subscriptions = None;
                // Updates during the flight already set dirty. Do not start
                // an unbounded retry loop for an unchanged failed snapshot.
                if let Err(error) = result {tracing::warn!(%error, "presentation observation subscription incomplete; awaits next native lifecycle event");}
            }
            Some(event) = events.recv() => {
                let client = Arc::clone(client);
                let pending = Arc::clone(&pending_events);
                let pending = PendingEvent(pending);
                flights.spawn(async move {
                    let _pending = pending;
                    let call = client.call(&event.to, &event.verb, event.body);
                    match tokio::time::timeout(EVENT_TIMEOUT, call).await {
                        Ok(Ok(_)) => {}
                        Ok(Err(error)) => tracing::debug!(%error, to = %event.to, verb = %event.verb, "scene event failed"),
                        Err(_) => tracing::debug!(to = %event.to, verb = %event.verb, "scene event timed out"),
                    }
                });
            }
            Some(_) = flights.join_next(), if !flights.is_empty() => {}
            Some(_) = replies.join_next(), if !replies.is_empty() => {}
            Some(_) = sends.join_next(), if !sends.is_empty() => {}
            Some(result) = description_replies.join_next(), if !description_replies.is_empty() => {
                reap("Quoin description", result, &mut description_faults, record_description_reply);
            }
        }
    }
    let deadline = std::time::Instant::now() + SHUTDOWN_BUDGET;
    notify(lane.publish(SettingsEvent::Wake));
    drain_outbound(
        client,
        &topics,
        outbound,
        &mut sends,
        &mut description_replies,
        &mut description_faults,
    )
    .await;
    flights.abort_all();
    replies.abort_all();
    sends.abort_all();
    while !description_replies.is_empty() {
        match tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            description_replies.join_next(),
        )
        .await
        {
            Ok(Some(result)) => reap(
                "Quoin description",
                result,
                &mut description_faults,
                record_description_reply,
            ),
            Ok(None) => break,
            Err(_) => {
                description_faults.push("description reply drain timed out".into());
                break;
            }
        }
    }
    cancel(
        "Quoin description",
        description_replies,
        &mut description_faults,
        record_description_reply,
    );
    tracing::debug!(faults = ?description_faults.recent(), counts = ?description_admission.counts(), "Quoin description shutdown");
    deadline
}

async fn drain_outbound(
    client: &SupervisedClient,
    topics: &(String, String),
    outbound: &mut tokio_mpsc::UnboundedReceiver<Outbound>,
    sends: &mut tokio::task::JoinSet<()>,
    description_replies: &mut TaskSet<Result<(), String>>,
    description_faults: &mut Faults,
) {
    // Freeze admission so every queued description has a terminal path even
    // when the ordinary send drain expires.
    outbound.close();
    // Finish the earlier active send before later queued messages. A bounded
    // drain that expires cancels the remaining sequence, preserving its order.
    if tokio::time::timeout(Duration::from_millis(50), async {
        while sends.join_next().await.is_some() {}
        while let Ok(message) = outbound.try_recv() {
            match message {
                Outbound::Description(reply) => {
                    submit_description_reply(reply, description_replies, description_faults)
                }
                message => send(client, topics, message).await,
            }
        }
    })
    .await
    .is_err()
    {
        tracing::warn!("scene host ordered outbound drain timed out");
    }
    // Ordinary messages retain the existing discard policy after the short
    // ordered drain. Descriptions still use the same tasks and reap path.
    while let Ok(message) = outbound.try_recv() {
        if let Outbound::Description(reply) = message {
            submit_description_reply(reply, description_replies, description_faults);
        }
    }
}

fn record_description_reply(result: Result<(), String>, faults: &mut Faults) {
    if let Err(error) = result {
        faults.push(error);
    }
}

fn submit_description_reply(
    reply: NativeReply,
    tasks: &mut TaskSet<Result<(), String>>,
    faults: &mut Faults,
) {
    if let Err(reply) = tasks.try_spawn_with(reply, NativeReply::into_task) {
        reply.retire().finish();
        faults.push("description reply task invariant failed; unsent reply retired".into());
    }
}

fn admit_participant_wait(client:&Arc<SupervisedClient>,mailbox:&RegistryMailbox,command:IncomingCommand,
    admission:&Admission,tasks:&mut TaskSet<Result<(),String>>)->Option<(IncomingCommand,u8,Value)> {
    if command.id.is_none() || settings::native::live_generation(client)!=Some(command.generation) {return None;}
    let Some(spec)=crate::participant_wait::parse(&command.body) else {
        return Some((command,10,json!({"error_code":"INVALID_PARTICIPANT_WAIT"})));
    };
    let Some(receipts)=mailbox.lock().unwrap_or_else(std::sync::PoisonError::into_inner).observations.as_ref().map(watch::Sender::subscribe) else {
        return Some((command,10,json!({"error_code":"PARTICIPANT_OWNER_UNAVAILABLE"})));
    };
    let Some(permit)=admission.try_acquire() else {return Some((command,11,json!({"error_code":"QUEUE_FULL"})));};
    let request=Accepted::new(client.clone(),command,permit,std::time::Instant::now());
    if let Err(request)=tasks.try_spawn_with(request,move |request|crate::participant_wait::run(request,spec,receipts)) {
        let (permit,command)=request.into_task(|_,command,_|command); permit.finish();
        return Some((command,11,json!({"error_code":"QUEUE_FULL"})));
    }
    None
}

fn admit_description(
    client: &Arc<SupervisedClient>,
    delivery: &Delivery,
    command: IncomingCommand,
    admission: &Admission,
) -> Option<(IncomingCommand, u8, Value)> {
    if command.id.is_none() || settings::native::live_generation(client) != Some(command.generation)
    {
        return None;
    }
    if let Err(error) = application::describe::validate_request(&command.body) {
        return Some((command, 10, crate::description::refusal(&error)));
    }
    let Some(permit) = admission.try_acquire() else {
        return Some((
            command,
            11,
            json!({"error_code":"QUEUE_FULL","message":"description capacity exhausted"}),
        ));
    };
    let request = Accepted::new(client.clone(), command, permit, std::time::Instant::now());
    match delivery.descriptions.try_send(request) {
        Ok(()) => {
            (delivery.waker)();
            None
        }
        Err(error) => {
            let (permit, command) = error.into_inner().into_task(|_, command, _| command);
            permit.finish();
            Some((
                command,
                11,
                json!({"error_code":"QUEUE_FULL","message":"description engine unavailable"}),
            ))
        }
    }
}

fn terminal_reason(client: &SupervisedClient) -> String {
    client.registration_rejection().map_or_else(
        || "Bus supervisor stopped; retaining settings resources".into(),
        |refusal| {
            format!(
                "registration refused (rc {}): {}; retaining settings resources",
                refusal.rc, refusal.message
            )
        },
    )
}

async fn registry_read(
    client: Arc<SupervisedClient>,
    delay: Duration,
) -> Option<(u64, BTreeSet<String>, BTreeMap<String,String>)> {
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
    let generation = settings::native::live_generation(&client)?;
    let services = tokio::time::timeout(
        SEND_TIMEOUT,
        client.call(
            "noded",
            "noded.props.get",
            json!({"path":"services.incarnations"}),
        ),
    )
    .await
    .ok()?
    .ok()?;
    let registrations = decode_registrations(&services)?;
    let services = registrations.keys().cloned().collect();
    Some((generation, services, registrations))
}

fn decode_registrations(value:&Value)->Option<BTreeMap<String,String>> {
    let rows = value.as_array()?;
    if rows.len() > 128 {return None;}
    let mut result = BTreeMap::new();
    for row in rows {
        let service = row["service"].as_str()?;
        let incarnation = row["incarnation"].as_str()?;
        if service.is_empty() || service.len() > 256 || incarnation.len() != 32 || !incarnation.bytes().all(|byte|byte.is_ascii_hexdigit()) {return None;}
        if result.insert(service.to_owned(), incarnation.to_owned()).is_some() {return None;}
    }
    Some(result)
}

fn presentation_notice(command:&IncomingCommand, registrations:&BTreeMap<String,String>)->Option<PresentationNotice> {
    let service = command.topic()?.strip_suffix(".presentation.changed")?;
    if command.header("broker_origin") != Some("local") || command.header("broker_service") != Some(service)
        || command.header("broker_registration") != registrations.get(service).map(String::as_str)
        || command.body.len() > 64 * 1024 {return None;}
    let value:Value = serde_json::from_str(&command.body).ok()?;
    if value["contract"] != "application.presentation.v1" || value["service"] != service {return None;}
    let mut envelope = bus::wire::BusMessage::new();
    envelope.headers = command.headers.clone();
    let principal = bus::native_session::read_principal(&envelope).ok()??;
    if principal.assurance != bus::native_session::Assurance::LocalUnix
        && principal.assurance != bus::native_session::Assurance::SessionBound {return None;}
    if value["pid"].as_u64() != Some(u64::from(principal.peer_pid)) {return None;}
    Some(PresentationNotice {service:service.to_owned(), registration:registrations.get(service)?.clone(),
        sequence:command.header("topic_seq")?.parse().ok()?, generation:command.generation,
        provenance:json!({"origin":"local","assurance":principal.assurance,"owner_node":principal.owner_node,
            "peer_pid":principal.peer_pid,"broker_epoch":principal.broker_epoch,"connection_id":principal.connection_id}), value})
}

async fn presentation_subscriptions(client:Arc<SupervisedClient>, state:Arc<Mutex<PresentationSubscriptions>>,
    generation:u64, previous:BTreeSet<String>, next:BTreeSet<String>)->Result<(),String> {
    if settings::native::live_generation(&client) != Some(generation) {return Err("native connection unavailable".into());}
    let deadline = tokio::time::Instant::now() + SEND_TIMEOUT;
    for service in previous.difference(&next) {
        let headers = BTreeMap::from([("name".into(),format!("{service}.presentation.changed"))]);
        let reply=tokio::time::timeout_at(deadline,client.call_with_headers_raw_at_generation(generation,"noded","topic.unsubscribe",&headers,"")).await.map_err(|_|"unsubscribe deadline")?.map_err(|_|"unsubscribe failed")?;
        if reply.0 != 0 {return Err("native presentation unsubscribe refused".into());}
        state.lock().unwrap().unsubscribed(generation,service);
    }
    // Idempotent re-subscribe recovers sent calls with uncertain replies.
    // Registry updates coalesce without cancelling this snapshot; no poller
    // or unbounded timer retry owns the list.
    for service in &next {
        state.lock().unwrap().subscribing(generation,service)?;
        let headers = BTreeMap::from([("name".into(),format!("{service}.presentation.changed"))]);
        let reply = tokio::time::timeout_at(deadline,client.call_with_headers_raw_at_generation(generation,"noded","topic.subscribe",&headers,"")).await.map_err(|_|"subscribe deadline")?.map_err(|_|"subscribe failed")?;
        if reply.0 != 0 {return Err("native presentation subscription refused".into());}
    }
    Ok(())
}

async fn registry_subscribe(client: Arc<SupervisedClient>) -> bool {
    for attempt in 0..3 {
        if settings::native::live_generation(&client).is_none() {
            return false;
        }
        if matches!(
            tokio::time::timeout(SEND_TIMEOUT, client.subscribe_topic(REGISTRY_TOPIC)).await,
            Ok(Ok(()))
        ) {
            return true;
        }
        if attempt < 2 {
            tokio::time::sleep(RETRY_INITIAL * (1 << attempt)).await;
        }
    }
    tracing::warn!(
        "scene host: registry subscription failed; retry waits for next lifecycle event"
    );
    false
}

/// Answer a request the engine never sees.
async fn refuse(client: &SupervisedClient, command: &IncomingCommand, rc: u8, body: Value) {
    let text = body.to_string();
    let reply = client.respond(command, rc, &text);
    match tokio::time::timeout(SEND_TIMEOUT, reply).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::debug!(%error, "scene host refusal not delivered"),
        Err(_) => tracing::debug!("scene host refusal timed out"),
    }
}

fn admit(
    service: &str,
    delivery: &Delivery,
    command: IncomingCommand,
) -> Option<(IncomingCommand, u8, Value)> {
    match route(service, &command) {
        Route::AppDescribe | Route::ParticipantWait => unreachable!("observation ingress has its own bounded owner"),
        Route::Scene(verb) => {
            let request = Request {
                verb,
                from: command.from.clone(),
                command: command.command.clone(),
                id: command.id.clone(),
                body: command.body.clone(),
                headers: command.headers.clone(),
                generation: command.generation,
            };
            if !delivery.send(Inbound::Request(request)) {
                let body = json!({"error_code":"QUEUE_FULL","message":"scene host queue full"});
                return Some((command, 11, body));
            }
        }
        Route::Live(live) => {
            if !delivery.send(Inbound::Live(live)) {
                tracing::warn!("scene host: registry update dropped, engine queue full");
            }
        }
        Route::Registrations(_) => {},
        Route::Forged => {
            tracing::warn!(from = %command.from, "scene host: ignoring a registry claim not from the local broker")
        }
        Route::Unknown => {
            let message = format!("{} is not a scene host verb", command.command);
            return Some((
                command,
                10,
                json!({"error_code":"UNKNOWN_VERB","message":message}),
            ));
        }
        Route::Ignore => {}
    }
    None
}

async fn send(
    client: &SupervisedClient,
    (scene_topic, panel_topic): &(String, String),
    message: Outbound,
) {
    let message=match message {
        Outbound::ParticipantsReady(pending)=> {
            let Some(ParticipantPublication {topic,wire,generation})=pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() else {return;};
            Outbound::Participants {topic,wire,generation}
        }
        message=>message,
    };
    let topic = match &message {
        Outbound::Participants { topic, .. } => topic.as_str(),
        Outbound::Publish { panel: true, .. } => panel_topic.as_str(),
        _ => scene_topic.as_str(),
    };
    let result = match &message {
        Outbound::Description(_) => {
            unreachable!("description replies require the bounded task set")
        }
        Outbound::ParticipantsReady(_) => unreachable!("coalesced publication resolved before send"),
        Outbound::Reply {
            generation,
            command,
            ..
        } if *generation != client.connection_generation() => {
            // Its caller's connection is gone; a reply on the new one would
            // reach whoever holds that name now, under a stale id.
            tracing::debug!(command = %command, "scene host reply dropped: admitted on an earlier connection");
            Ok(())
        }
        Outbound::Reply {
            generation,
            to,
            command,
            id,
            rc,
            body,
            ..
        } => tokio::time::timeout(
            SEND_TIMEOUT,
            client.respond_parts(*generation, to, command, id.as_deref(), *rc, body),
        )
        .await
        .map_err(|_| "timed out".to_string())
        .and_then(|result| result.map_err(|error| error.to_string())),
        Outbound::Publish { panel, wire } => {
            let mut headers = BTreeMap::new();
            headers.insert("name".to_string(), topic.to_string());
            headers.insert("retain".to_string(), panel.to_string());
            match tokio::time::timeout(
                SEND_TIMEOUT,
                client.call_with_headers_raw("noded", "topic.publish", &headers, wire),
            )
            .await
            {
                Err(_) => Err("timed out".to_string()),
                Ok(Err(error)) => Err(error.to_string()),
                Ok(Ok((0, _, _))) => Ok(()),
                Ok(Ok((rc, body, _))) => {
                    Err(format!("topic.publish rejected with rc {rc}: {body}"))
                }
            }
        }
        Outbound::Participants { wire, generation, .. } => {
            let headers = BTreeMap::from([("name".into(),topic.into()),("retain".into(),"true".into())]);
            tokio::time::timeout(SEND_TIMEOUT,
                client.call_with_headers_raw_at_generation(*generation,"noded","topic.publish",&headers,wire))
                .await.map_err(|_|"timed out".to_owned())
                .and_then(|reply| reply.map_err(|error|error.to_string()))
                .and_then(|(rc,body,_)| if rc == 0 {Ok(())} else {Err(format!("topic.publish rejected with rc {rc}: {body}"))})
        }
    };
    if let Err(error) = result {
        match message {
            Outbound::Description(_) => unreachable!("description handled above"),
            Outbound::ParticipantsReady(_) => unreachable!("publication resolved before send"),
            Outbound::Reply { to, command, .. } => {
                tracing::warn!(%error, to = %to, command = %command, "scene host reply not delivered")
            }
            Outbound::Publish { .. } => tracing::warn!(%error, topic, "scene host publish failed"),
            Outbound::Participants { .. } => tracing::warn!(%error, topic, "participant observation publish failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn received_command(
        incoming: &mut bus::native_client::BoundedIncomingReceiver,
    ) -> IncomingCommand {
        match tokio::time::timeout(Duration::from_secs(5), incoming.recv())
            .await
            .unwrap()
            .unwrap()
        {
            BoundedIncomingEvent::Command(command) => command,
            BoundedIncomingEvent::Overflow { .. } => panic!("unexpected native ingress overflow"),
        }
    }

    #[tokio::test]
    async fn shutdown_description_behind_a_stalled_send_uses_the_reaped_reply_lane() {
        use bus::native_client::NodedClient;
        let broker = term_test_broker::Broker::start_stable();
        let client = Arc::new(
            SupervisedClient::connect_options("shell-shutdown-description", &broker.url)
                .bounded_incoming(DESCRIPTION_CAPACITY)
                .connect()
                .await
                .unwrap(),
        );
        assert!(settings::native::live_generation(&client).is_some());
        let stalled = SupervisedClient::connect_options("shell-stalled-send", &broker.url)
            .bounded_incoming(1)
            .connect()
            .await
            .unwrap();
        assert!(settings::native::live_generation(&stalled).is_some());
        let mut stalled_incoming = stalled.incoming_bounded().unwrap();
        let mut incoming = client.incoming_bounded().unwrap();
        let caller = Arc::new(NodedClient::connect_anonymous(&broker.url).await.unwrap());
        let calling = caller.clone();
        let description = tokio::spawn(async move {
            calling
                .call_with_headers_raw(
                    "shell-shutdown-description",
                    "app.describe",
                    &BTreeMap::new(),
                    "{}",
                )
                .await
        });
        let command = received_command(&mut incoming).await;
        let admission = Admission::new(DESCRIPTION_CAPACITY);
        let request = Accepted::new(
            client.clone(),
            command,
            admission.try_acquire().unwrap(),
            std::time::Instant::now(),
        );
        let (outbound_tx, mut outbound) = tokio_mpsc::unbounded_channel();
        assert!(
            outbound_tx
                .send(Outbound::Description(request.reply(
                    10,
                    json!({"error_code":"SHUTDOWN","message":"host stopping"}).to_string(),
                    std::time::Instant::now() + SEND_TIMEOUT
                )))
                .is_ok()
        );
        let mut sends = tokio::task::JoinSet::new();
        let sending = client.clone();
        sends.spawn(async move {
            let _ = sending
                .call("shell-stalled-send", "test.hold", json!({}))
                .await;
        });
        // The actual recipient has the ordinary call and deliberately retains
        // it unanswered; no scheduler delay is used to infer that send state.
        let held = received_command(&mut stalled_incoming).await;
        assert_eq!(held.command, "test.hold");
        let mut replies = TaskSet::new(DESCRIPTION_CAPACITY);
        let mut faults = Faults::default();
        tokio::time::timeout(
            SHUTDOWN_BUDGET,
            drain_outbound(
                &client,
                &(
                    "shell-shutdown-description.scene.changed".into(),
                    "shell-shutdown-description.panel.changed".into(),
                ),
                &mut outbound,
                &mut sends,
                &mut replies,
                &mut faults,
            ),
        )
        .await
        .unwrap();
        assert_eq!(sends.len(), 1, "ordinary recipient has not replied");
        assert_eq!(
            replies.len(),
            1,
            "queued description entered the bounded task set"
        );
        assert_eq!(admission.counts().active, 1);
        let completed = tokio::time::timeout(SHUTDOWN_BUDGET, replies.join_next())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            admission.counts().finished,
            0,
            "credit survives task completion until reap"
        );
        reap(
            "Quoin description",
            completed,
            &mut faults,
            record_description_reply,
        );
        let response = tokio::time::timeout(SHUTDOWN_BUDGET, description)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(response.0, 10);
        assert_eq!(
            serde_json::from_str::<Value>(&response.1).unwrap()["error_code"],
            "SHUTDOWN"
        );
        assert_eq!(faults.count(), 0);
        assert_eq!(admission.counts().active, 0);
        assert_eq!(admission.counts().finished, 1);
        assert_eq!(admission.counts().abandoned, 0);
        sends.abort_all();
        while sends.join_next().await.is_some() {}
        caller.close().await;
        stalled.close().await;
        client.close().await;
    }

    #[tokio::test]
    async fn frozen_description_ingress_retires_queued_commands_and_rejects_a_late_worker() {
        use bus::native_client::NodedClient;
        let broker = term_test_broker::Broker::start_stable();
        let client = Arc::new(
            SupervisedClient::connect_options("shell-frozen-description", &broker.url)
                .bounded_incoming(DESCRIPTION_CAPACITY)
                .connect()
                .await
                .unwrap(),
        );
        assert!(settings::native::live_generation(&client).is_some());
        let mut incoming = client.incoming_bounded().unwrap();
        let caller = Arc::new(NodedClient::connect_anonymous(&broker.url).await.unwrap());
        let (sender, _inbound) = mpsc::sync_channel(INBOUND_CAPACITY);
        let (descriptions, receiver) = tokio_mpsc::channel(DESCRIPTION_CAPACITY);
        let receiver = Mutex::new(receiver);
        let delivery = Delivery {
            sender,
            descriptions,
            waker: Arc::new(|| {}),
        };
        let admission = Admission::new(DESCRIPTION_CAPACITY);
        let mut waiting = Vec::new();
        for _ in 0..2 {
            let calling = caller.clone();
            waiting.push(tokio::spawn(async move {
                calling
                    .call_with_headers_raw(
                        "shell-frozen-description",
                        "app.describe",
                        &BTreeMap::new(),
                        "{}",
                    )
                    .await
            }));
            let command = received_command(&mut incoming).await;
            assert!(admit_description(&client, &delivery, command, &admission).is_none());
        }
        assert_eq!(admission.counts().active, 2);
        freeze_descriptions(&receiver);
        assert!(delivery.descriptions.is_closed());
        assert_eq!(admission.counts().active, 0);
        assert_eq!(admission.counts().finished, 2);
        assert_eq!(admission.counts().abandoned, 0);
        // Retain the real worker's sender after freeze, as happens if worker
        // shutdown is detached. Its next native command cannot enter ingress.
        let calling = caller.clone();
        let late = tokio::spawn(async move {
            calling
                .call_with_headers_raw(
                    "shell-frozen-description",
                    "app.describe",
                    &BTreeMap::new(),
                    " { } ",
                )
                .await
        });
        let command = received_command(&mut incoming).await;
        let original = (
            command.from.clone(),
            command.id.clone(),
            command.body.clone(),
            command.generation,
        );
        let (command, rc, body) =
            admit_description(&client, &delivery, command, &admission).unwrap();
        assert_eq!(
            (
                command.from.clone(),
                command.id.clone(),
                command.body.clone(),
                command.generation
            ),
            original
        );
        assert_eq!(rc, 11);
        assert_eq!(admission.counts().active, 0);
        assert_eq!(
            admission.counts().finished,
            3,
            "rejected insertion explicitly retires its acquired credit"
        );
        assert_eq!(admission.counts().abandoned, 0);
        refuse(&client, &command, rc, body).await;
        let response = tokio::time::timeout(SHUTDOWN_BUDGET, late)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(response.0, 11);
        assert_eq!(
            serde_json::from_str::<Value>(&response.1).unwrap()["error_code"],
            "QUEUE_FULL"
        );
        assert!(receiver.lock().unwrap().try_recv().is_err());
        for task in waiting {
            task.abort();
            let _ = task.await;
        }
        caller.close().await;
        client.close().await;
    }

    #[tokio::test]
    async fn description_reply_timeout_is_recorded_before_credit_finishes() {
        let broker = term_test_broker::Broker::start_stable();
        let client = Arc::new(
            SupervisedClient::connect_options("shell-description-credit", &broker.url)
                .connect()
                .await
                .unwrap(),
        );
        let admission = Admission::new(1);
        let mut command = incoming("caller", "app.describe", Some("credit"), &[], "{}");
        command.generation = client.connection_generation();
        let now = std::time::Instant::now();
        let request = Accepted::new(
            client.clone(),
            command,
            admission.try_acquire().unwrap(),
            now,
        );
        let mut tasks = TaskSet::new(1);
        let mut faults = Faults::default();
        submit_description_reply(request.reply(0, "{}".into(), now), &mut tasks, &mut faults);
        let completed = tasks.join_next().await.unwrap();
        assert_eq!(admission.counts().active, 1);
        assert_eq!(admission.counts().finished, 0);
        reap(
            "Quoin description",
            completed,
            &mut faults,
            record_description_reply,
        );
        assert_eq!(faults.count(), 1);
        assert_eq!(faults.recent().back().unwrap(), "Bus reply timed out");
        assert_eq!(admission.counts().active, 0);
        assert_eq!(admission.counts().finished, 1);
        assert_eq!(admission.counts().abandoned, 0);
        client.close().await;
    }

    #[tokio::test]
    async fn native_embedded_descriptions_answer_without_output_or_window_and_use_actual_fallback_name()
     {
        use bus::native_client::NodedClient;
        let broker = term_test_broker::Broker::start_stable();
        for fallback in [false, true] {
            let owner = if fallback {
                Some(
                    SupervisedClient::connect_options("shell", &broker.url)
                        .connect()
                        .await
                        .unwrap(),
                )
            } else {
                None
            };
            let wake = Arc::new(tokio::sync::Notify::new());
            let notify = wake.clone();
            let waker: Waker = Arc::new(move || notify.notify_one());
            let root = tempfile::tempdir().unwrap();
            let port = Port::start_at(
                HostConfig {
                    owner_version: "compd-fixture".into(),
                    service_override: Some("shell-description-overridden".into()),
                    noded_url: broker.url.clone(),
                },
                waker.clone(),
                root.path().join("cache"),
            )
            .unwrap();
            tokio::time::timeout(Duration::from_secs(10), async {
                while port.settings_generation().is_none() {
                    wake.notified().await;
                }
            })
            .await
            .unwrap();
            let service = port.registered_service_name().unwrap().to_owned();
            assert_eq!(
                service,
                if fallback {
                    "shell-description-overridden"
                } else {
                    "shell"
                }
            );
            let mut engine = crate::host::SceneHost::from_port(
                port,
                waker,
                "compd-fixture".into(),
                crate::host::Host::default(),
            );
            let caller = Arc::new(NodedClient::connect_anonymous(&broker.url).await.unwrap());
            for body in ["", "{}", " { } "] {
                let calling = caller.clone();
                let target = service.clone();
                let body = body.to_owned();
                let mut request = tokio::spawn(async move {
                    calling
                        .call_with_headers_raw(&target, "app.describe", &BTreeMap::new(), &body)
                        .await
                });
                let result = tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        tokio::select! {
                            result = &mut request => break result.unwrap().unwrap(),
                            () = wake.notified() => { assert!(engine.service_descriptions() <= DESCRIPTION_CAPACITY); }
                        }
                    }
                }).await.unwrap();
                assert_eq!(result.0, 0);
                let value: Value = serde_json::from_str(&result.1).unwrap();
                application::describe::validate(&value).unwrap();
                assert_eq!(value["service"], service);
                assert_eq!(value["version"], "compd-fixture");
                assert_eq!(value["pid"], std::process::id());
                assert!(value["app_id"].is_null());
                assert_eq!(value["embedded"], true);
            }
            for body in [
                "{".into(),
                "null".into(),
                "[]".into(),
                "{\"extra\":true}".into(),
                format!(
                    "{{{}}}",
                    " ".repeat(application::describe::MAX_REQUEST_BYTES)
                ),
            ] {
                let (rc, body, _) = tokio::time::timeout(
                    Duration::from_secs(5),
                    caller.call_with_headers_raw(&service, "app.describe", &BTreeMap::new(), &body),
                )
                .await
                .unwrap()
                .unwrap();
                let value: Value = serde_json::from_str(&body).unwrap();
                assert_eq!(rc, 10);
                assert_eq!(value["error_code"], "ARGUMENT");
                assert!(value["describe_code"].is_string());
                assert_eq!(
                    engine.service_descriptions(),
                    0,
                    "invalid request reached the engine"
                );
            }
            caller.close().await;
            engine.finish();
            if let Some(owner) = owner {
                owner.close().await;
            }
        }
    }

    fn install_settings_fonts() {
        static FONTS: std::sync::Once = std::sync::Once::new();
        FONTS.call_once(|| {
            toolkit::fonts::install(
                toolkit::fonts::FontSet::new().sans(
                    include_bytes!("../../../../../vendor/font/Inter-VariableFont_opsz,wght.ttf")
                        .as_slice(),
                ),
                None,
            )
            .unwrap();
        });
    }

    fn settings_drive(
        port: &Port,
        wake: &Receiver<()>,
        session: &mut SettingsUi<Look>,
        panels: &mut crate::panels::Panels,
        revision: Option<u64>,
    ) -> usize {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let mut changes = 0;
        loop {
            changes += session
                .drain_with(
                    || port.settings_generation(),
                    |presentation| {
                        panels.set_preferences(presentation.content().preferences.clone());
                    },
                )
                .len();
            let ready =
                match revision {
                    Some(revision) => {
                        session.session().host().kind()
                            == Some(settings::fallback::PresentationKind::Current)
                            && session.session().host().consumer().applied().is_some_and(
                                |snapshot| snapshot.revision == settings::Revision(revision),
                            )
                    }
                    None => {
                        session.session().host().kind()
                            == Some(settings::fallback::PresentationKind::Embedded)
                    }
                };
            if ready {
                return changes;
            }
            wake.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .expect("Quoin settings did not converge before deadline");
        }
    }

    fn settings_port(
        url: String,
        cache_directory: std::path::PathBuf,
    ) -> (Port, Receiver<()>, SettingsUi<Look>) {
        install_settings_fonts();
        let (notify, wake) = mpsc::channel();
        let mut port = Port::start_at(
            HostConfig {
                owner_version: "compd-fixture".into(),
                service_override: None,
                noded_url: url,
            },
            Arc::new(move || {
                let _ = notify.send(());
            }),
            cache_directory,
        )
        .unwrap();
        let mut session = port.take_settings_ui();
        session.reconcile(port.settings_generation());
        (port, wake, session)
    }

    #[test]
    #[ignore = "requires settings_test.mix isolated environment"]
    fn settings_offline_port_prepares_fallback_while_connecting() {
        // Own an unresponsive endpoint that never completes the WS upgrade,
        // without selecting a port
        // somebody else can claim between binding and the connection attempt.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let cache = tempfile::tempdir().unwrap();
        let (port, wake, mut session) = settings_port(
            format!("ws://{}/ws", listener.local_addr().unwrap()),
            cache.path().join("settings"),
        );
        let mut panels = crate::panels::Panels::default();
        assert_eq!(
            settings_drive(&port, &wake, &mut session, &mut panels, None),
            1
        );
        panels.ensure("fixture", (1280.0, 800.0));
        assert_eq!(
            panels.state("fixture", crate::seat::Edge::Bottom)["mode"],
            "docked"
        );
        assert_eq!(port.settings_generation(), None);
        assert_eq!(
            session
                .session()
                .host()
                .presentation()
                .unwrap()
                .content()
                .chrome(decor::ChromeStyle::Mac)
                .tokens,
            decor::TokenSource::Prepared
        );
        let closing = std::time::Instant::now();
        port.finish();
        assert!(closing.elapsed() < SHUTDOWN_GRACE + Duration::from_millis(100));
    }

    #[tokio::test]
    #[ignore = "requires exact-revision binaries and isolated settings_test.mix broker"]
    async fn settings_native_port_activates_and_retains_last_good_after_real_loss() {
        use std::process::{Child, Command, Stdio};
        struct Authority(Child);
        impl Drop for Authority {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let root = tempfile::tempdir().unwrap();
        let binary = std::env::var("MIXOS_TEST_SETTINGSD").unwrap();
        assert!(
            Command::new(&binary)
                .args(["seed", "--allow-create", "--instance", "fixture", "--root"])
                .arg(root.path())
                .status()
                .unwrap()
                .success()
        );
        let log = std::fs::File::create(root.path().join("settingsd.log")).unwrap();
        let _authority = Authority(
            Command::new(&binary)
                .args(["serve", "--instance", "fixture", "--root"])
                .arg(root.path())
                .stdout(Stdio::from(log.try_clone().unwrap()))
                .stderr(Stdio::from(log))
                .spawn()
                .unwrap(),
        );
        let url = std::env::var("MIXOS_NODED_URL").unwrap();
        let cache_directory = root.path().join("cache/settings");
        let (port, wake, mut session) = settings_port(url.clone(), cache_directory.clone());
        let mut panels = crate::panels::Panels::default();
        panels.ensure("fixture", (1280.0, 800.0));
        assert_eq!(
            settings_drive(&port, &wake, &mut session, &mut panels, Some(1)),
            1
        );
        let before = session
            .session()
            .host()
            .presentation()
            .unwrap()
            .content()
            .clone();
        let controller = SupervisedClient::connect_options("quoin-settings-controller", &url)
            .connect()
            .await
            .unwrap();
        let current = session.session().host().consumer().current().unwrap();
        let applied = controller
            .call(
                "settingsd",
                "settings.apply",
                json!({
                    "binding": current.binding, "expected_incarnation": current.incarnation,
                    "expected_revision":"1", "operation_id":"quoin-live-fixture",
                    "changes":{"appearance.mode":"dark","ui.text_scale":1.5,
                        "shell.panels.bottom":{"edge":"bottom","mode":"dock","thickness":80},
                        "shell.panels.right":{"edge":"right","mode":"overlay","thickness":160}}
                }),
            )
            .await
            .unwrap();
        assert_eq!(applied["status"], "changed");
        assert_eq!(
            settings_drive(&port, &wake, &mut session, &mut panels, Some(2)),
            1
        );
        assert_eq!(
            panels.state("fixture", crate::seat::Edge::Bottom)["mode"],
            "docked"
        );
        assert_eq!(
            panels.state("fixture", crate::seat::Edge::Bottom)["settings"]["requested_px"],
            80.0
        );
        assert_eq!(
            panels.state("fixture", crate::seat::Edge::Right)["mode"],
            "pinned"
        );
        panels.ensure("future", (1920.0, 1080.0));
        assert_eq!(
            panels.state("future", crate::seat::Edge::Right)["width_px"],
            160.0
        );
        assert!(panels.zones("future").is_empty());
        let after = session
            .session()
            .host()
            .presentation()
            .unwrap()
            .content()
            .clone();
        assert_ne!(
            after.prepared.tokens().palette,
            before.prepared.tokens().palette
        );
        for style in decor::ChromeStyle::ALL {
            assert_eq!(
                after.chrome(style).deco.metrics.title_size_px,
                before.chrome(style).deco.metrics.title_size_px * 1.5
            );
        }
        let current = session.session().host().consumer().current().unwrap();
        let app_only = controller
            .call(
                "settingsd",
                "settings.apply",
                json!({
                    "binding":current.binding, "expected_incarnation":current.incarnation,
                    "expected_revision":"2", "operation_id":"quoin-unrelated-app",
                    "changes":{"apps.ced":{"mode":"light"}}
                }),
            )
            .await
            .unwrap();
        assert_eq!(app_only["status"], "changed");
        assert_eq!(
            settings_drive(&port, &wake, &mut session, &mut panels, Some(3)),
            0,
            "an unrelated app override acknowledges without staging shell resources"
        );
        let current = session.session().host().consumer().current().unwrap();
        let invalid = controller.call("settingsd", "settings.apply", json!({
            "binding":current.binding, "expected_incarnation":current.incarnation,
            "expected_revision":"3", "operation_id":"quoin-duplicate-edge",
            "changes":{"shell.panels.duplicate":{"edge":"bottom","mode":"dock","thickness":100}}
        })).await.unwrap();
        assert_eq!(invalid["status"], "changed");
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while session.session().host().consumer().fault().is_none() {
            let changes = session.drain_with(
                || port.settings_generation(),
                |presentation| {
                    panels.set_preferences(presentation.content().preferences.clone());
                },
            );
            assert!(
                changes.is_empty(),
                "failed whole preparation must not activate either resource"
            );
            if session.session().host().consumer().fault().is_none() {
                wake.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                    .expect("Quoin invalid-policy diagnostic did not arrive before deadline");
            }
        }
        assert_eq!(
            session
                .session()
                .host()
                .consumer()
                .applied()
                .unwrap()
                .revision,
            settings::Revision(3)
        );
        assert_eq!(
            panels.state("fixture", crate::seat::Edge::Bottom)["mode"],
            "docked"
        );
        assert_eq!(
            session
                .session()
                .host()
                .presentation()
                .unwrap()
                .content()
                .prepared
                .tokens()
                .palette,
            after.prepared.tokens().palette
        );
        let client = port.client.get().unwrap();
        client.close().await;
        assert_eq!(port.settings_generation(), None);
        let change = session.handle_with(SettingsEvent::Wake, port.settings_generation(), |_| {});
        assert!(change.is_none());
        assert_eq!(
            session.session().host().kind(),
            Some(settings::fallback::PresentationKind::LastGood)
        );
        assert_eq!(
            session
                .session()
                .host()
                .consumer()
                .applied()
                .unwrap()
                .revision,
            settings::Revision(3)
        );
        controller.close().await;
        port.finish();

        // The newest successfully activated snapshot survives connection loss
        // and shutdown, and is validated again by an offline worker.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let offline_url = format!("ws://{}/ws", listener.local_addr().unwrap());
        let (port, wake, mut cached) = settings_port(offline_url.clone(), cache_directory.clone());
        let mut cached_panels = crate::panels::Panels::default();
        drive_fallback(
            &port,
            &wake,
            &mut cached,
            &mut cached_panels,
            settings::fallback::PresentationKind::Cached,
        );
        assert_eq!(
            cached
                .session()
                .host()
                .consumer()
                .applied()
                .unwrap()
                .revision,
            settings::Revision(3)
        );
        assert_eq!(
            cached
                .session()
                .host()
                .presentation()
                .unwrap()
                .content()
                .prepared
                .tokens()
                .palette,
            after.prepared.tokens().palette
        );
        assert_eq!(port.settings_generation(), None);
        assert!(!cached.session().host().consumer().evidence().confirmed);
        port.finish();

        let files: Vec<_> = std::fs::read_dir(&cache_directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .collect();
        assert_eq!(files.len(), 1);
        std::fs::write(&files[0], b"{broken cache").unwrap();
        let (port, wake, mut embedded) = settings_port(offline_url, cache_directory);
        drive_fallback(
            &port,
            &wake,
            &mut embedded,
            &mut cached_panels,
            settings::fallback::PresentationKind::Embedded,
        );
        assert!(!embedded.session().fallback_diagnostics().is_empty());
        assert!(!embedded.session().host().consumer().evidence().confirmed);
        port.finish();
    }

    fn drive_fallback(
        port: &Port,
        wake: &Receiver<()>,
        session: &mut SettingsUi<Look>,
        panels: &mut crate::panels::Panels,
        kind: settings::fallback::PresentationKind,
    ) {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            session.drain_with(
                || port.settings_generation(),
                |presentation| {
                    panels.set_preferences(presentation.content().preferences.clone());
                },
            );
            if session.session().host().kind() == Some(kind) {
                return;
            }
            wake.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .expect("offline Quoin fallback did not converge");
        }
    }

    #[tokio::test]
    #[ignore = "requires isolated settings_test.mix broker"]
    async fn settings_initial_refusal_uses_only_configured_override_and_keeps_fallback() {
        install_settings_fonts();
        let url = std::env::var("MIXOS_NODED_URL").unwrap();
        let owner = SupervisedClient::connect_options("shell", &url)
            .connect()
            .await
            .unwrap();
        let owner_generation = owner.connection_generation();
        let root = tempfile::tempdir().unwrap();
        for (index, service_override) in [Some("shell-cache-fixture".to_owned()), None]
            .into_iter()
            .enumerate()
        {
            let (notify, wake) = mpsc::channel();
            let mut port = Port::start_at(
                HostConfig {
                    owner_version: "compd-fixture".into(),
                    noded_url: url.clone(),
                    service_override,
                },
                Arc::new(move || {
                    let _ = notify.send(());
                }),
                root.path().join(index.to_string()),
            )
            .unwrap();
            let mut session = port.take_settings_ui();
            session.reconcile(port.settings_generation());
            let mut panels = crate::panels::Panels::default();
            drive_fallback(
                &port,
                &wake,
                &mut session,
                &mut panels,
                settings::fallback::PresentationKind::Embedded,
            );
            let deadline = std::time::Instant::now() + Duration::from_secs(20);
            loop {
                match port.try_recv() {
                    Some(Inbound::Registered(name)) => {
                        assert_eq!(index, 0);
                        assert_eq!(name, "shell-cache-fixture");
                        assert!(
                            port.connection_generation()
                                .is_some_and(|generation| generation > 0)
                        );
                        break;
                    }
                    Some(Inbound::Refused(reason)) => {
                        assert_eq!(index, 1);
                        assert!(reason.contains("no --scene-service"));
                        assert_eq!(port.connection_generation(), None);
                        break;
                    }
                    _ => {
                        wake.recv_timeout(
                            deadline.saturating_duration_since(std::time::Instant::now()),
                        )
                        .expect("initial scene-host refusal did not settle");
                    }
                }
            }
            assert_eq!(
                session.session().host().kind(),
                Some(settings::fallback::PresentationKind::Embedded)
            );
            port.finish();
            assert_eq!(
                owner.state(),
                ConnState::Connected,
                "the existing shell owner must remain untouched"
            );
            owner.call("noded", "noded.ping", json!({})).await.unwrap();
            assert_eq!(
                owner.connection_generation(),
                owner_generation,
                "fallback must preserve the existing owner's actual connection"
            );
        }
        owner.close().await;
    }

    #[test]
    fn page_request_from_worker_wakes_idle_loop_and_schedules_frame() {
        use dispatcher::state::state::{Dispatch, RedrawReason};
        use smithay::reexports::calloop::EventLoop;
        use smithay::reexports::calloop::ping::make_ping;
        use smithay::reexports::wayland_server::Display;

        struct Data {
            dispatch: Dispatch,
            host: crate::host::Host,
            frames: u32,
        }
        let mut event_loop: EventLoop<'static, Data> = EventLoop::try_new().unwrap();
        let display: Display<Dispatch> = Display::new().unwrap();
        let mut dispatch =
            dispatcher::wire::wire::new_dispatch(&display.handle(), None, event_loop.handle());
        let (redraw, source) = make_ping().unwrap();
        dispatch.redraw.set_ping(redraw);
        dispatch.redraw.rendering("kms");
        dispatch.redraw.frame("kms", false);
        event_loop
            .handle()
            .insert_source(source, |_, _, data: &mut Data| {
                assert!(data.dispatch.redraw.pending());
                data.dispatch.redraw.rendering("kms");
                assert!(
                    data.dispatch
                        .redraw
                        .frame("kms", false)
                        .contains(RedrawReason::Publish)
                );
                data.frames += 1;
            })
            .unwrap();

        let mut host = crate::host::Host::default();
        host.panels.ensure("DP-1", (1280.0, 800.0));
        let request = |verb, body: Value| Request {
            verb,
            from: "scenes".into(),
            command: "shell.panel.page.set".into(),
            id: Some("1".into()),
            body: body.to_string(),
            headers: BTreeMap::from([("broker_origin".into(), "local".into())]),
            generation: 1,
        };
        let mut no_layout =
            |_: &crate::store::SceneStore, _: &str, _: Option<&str>| Err(Value::Null);
        for name in ["calendar", "notifications"] {
            let source = format!(
                "---\nscene: 1\nname: {name}\ncitizen: scenes\nwindow: {{\"kind\":\"edge\",\"edge\":\"right\"}}\n---\n```mix\nroot: {{widget: \"column\", children: []}}\n```\n"
            );
            let answer = host.answer(
                &request(SceneVerb::Load, json!({"source":source})),
                "DP-1",
                Some(1),
                &mut no_layout,
            );
            assert_eq!(answer.rc, 0);
        }
        host.panels.sync(&host.store);
        host.panels
            .page_set("DP-1", crate::seat::Edge::Right, "scene-calendar")
            .unwrap();

        // This is the production worker Delivery, including its after-enqueue
        // Ping. There are no input, frame clock, or watchdog sources to help it.
        let (sender, inbound) = mpsc::sync_channel(INBOUND_CAPACITY);
        let (ping, source) = make_ping().unwrap();
        let delivery = Arc::new(Delivery {
            sender,
            descriptions: tokio_mpsc::channel(DESCRIPTION_CAPACITY).0,
            waker: Arc::new(move || ping.ping()),
        });
        event_loop
            .handle()
            .insert_source(source, move |_, _, data: &mut Data| {
                let Inbound::Request(request) = inbound.try_recv().unwrap() else {
                    panic!("expected page request")
                };
                let answer = data.host.answer(&request, "DP-1", Some(1), &mut no_layout);
                assert_eq!(answer.rc, 0);
                // lib::service uses exactly this changed verdict to request pixels.
                assert!(answer.changed);
                data.dispatch.schedule_redraw(RedrawReason::Publish);
            })
            .unwrap();
        let mut data = Data {
            dispatch,
            host,
            frames: 0,
        };
        for (index, page) in ["scene-notifications", "scene-calendar"]
            .into_iter()
            .enumerate()
        {
            let delivery = Arc::clone(&delivery);
            let request = request(SceneVerb::PanelPageSet, json!({"edge":"right", "id":page}));
            let worker =
                std::thread::spawn(move || assert!(delivery.send(Inbound::Request(request))));
            event_loop
                .dispatch(Duration::from_secs(1), &mut data)
                .unwrap();
            assert_eq!(
                data.host.panels.state("DP-1", crate::seat::Edge::Right)["page"],
                page
            );
            assert!(data.dispatch.redraw.needs("kms"));
            event_loop
                .dispatch(Duration::from_secs(1), &mut data)
                .unwrap();
            worker.join().unwrap();
            assert_eq!(data.frames, index as u32 + 1);
        }
    }

    fn incoming(
        from: &str,
        command: &str,
        id: Option<&str>,
        headers: &[(&str, &str)],
        body: &str,
    ) -> IncomingCommand {
        IncomingCommand {
            generation: 0,
            from: from.into(),
            command: command.into(),
            id: id.map(str::to_owned),
            args: Value::Null,
            body: body.into(),
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    #[test]
    fn shell_first_then_only_a_distinct_override() {
        assert_eq!(candidate_names(None), ["shell"]);
        assert_eq!(candidate_names(Some("shell")), ["shell"]);
        assert_eq!(candidate_names(Some("  ")), ["shell"]);
        assert_eq!(
            candidate_names(Some("shell-nested")),
            ["shell", "shell-nested"]
        );
    }

    #[test]
    fn native_presentation_admission_requires_actual_peer_pid_and_local_origin() {
        use bus::native_session::{Assurance,BrokerPrincipal,HexBytes,PrincipalVersion};
        let registration="0123456789abcdef0123456789abcdef";
        let registrations=BTreeMap::from([("term".into(),registration.into())]);
        let principal=BrokerPrincipal {version:PrincipalVersion::V1,assurance:Assurance::LocalUnix,
            owner_node:"fixture".into(),unix_uid:1000,unix_gid:1000,peer_pid:7,
            broker_epoch:HexBytes([1;16]),connection_id:HexBytes([2;16]),session:None};
        let mut command=incoming("term","term.presentation.changed",None,
            &[("topic","term.presentation.changed"),("topic_seq","1"),("broker_origin","local"),
                ("broker_service","term"),("broker_registration",registration)],
            r#"{"contract":"application.presentation.v1","service":"term","pid":7}"#);
        let mut envelope=bus::wire::BusMessage::new();
        bus::native_session::stamp_principal(&mut envelope,Some(&principal)).unwrap();
        command.headers.extend(envelope.headers);
        assert!(presentation_notice(&command,&registrations).is_some());
        command.body=r#"{"contract":"application.presentation.v1","service":"term","pid":8,"meta":{"peer_pid":7}}"#.into();
        assert!(presentation_notice(&command,&registrations).is_none(),"publisher metadata cannot assert a native PID");
        command.body=r#"{"contract":"application.presentation.v1","service":"term","pid":7}"#.into();
        command.headers.insert("broker_origin".into(),"mesh".into());
        assert!(presentation_notice(&command,&registrations).is_none(),"even matching boot/clock payloads cannot make mesh receipts local");
        command.headers.insert("broker_origin".into(),"local".into());
        command.headers.insert("broker_registration".into(),"retired".into());
        assert!(presentation_notice(&command,&registrations).is_none());
    }

    #[test]
    fn interrupted_subscription_removal_survives_coalesced_registry_updates() {
        let mut state=PresentationSubscriptions::default(); state.reset(1);
        state.subscribing(1,"old").unwrap();
        state.desire(BTreeSet::new());
        // Reconciliation has begun but no unsubscribe ACK has arrived.
        state.desire(BTreeSet::from(["new".into()]));
        assert!(state.possible.contains("old"),"desired state cannot erase pending removal");
        assert_eq!(state.possible.difference(&state.desired).cloned().collect::<Vec<_>>(),vec!["old"]);
        state.unsubscribed(1,"old"); state.subscribing(1,"new").unwrap();
        assert_eq!(state.possible,state.desired);
        // A sent subscribe with a lost reply is also retained for later removal.
        state.subscribing(1,"uncertain").unwrap();
        state.desire(BTreeSet::new());
        assert!(state.possible.contains("uncertain"));
        state.reset(2); state.subscribing(2,"old").unwrap();
        state.unsubscribed(1,"old");
        assert!(state.possible.contains("old"),"retired ACK cannot strip a new-generation subscription");
        assert!(state.subscribing(1,"retired").is_err());
        for index in 0..127 {state.subscribing(2,&format!("owner-{index}")).unwrap();}
        assert!(state.subscribing(2,"over-capacity").is_err());
        assert_eq!(state.possible.len(),128);
    }

    #[test]
    fn frames_route_to_verbs_registry_sets_or_nothing() {
        let describe = incoming(
            "caller",
            "app.describe",
            Some("describe"),
            &[("broker_origin", "local")],
            "{}",
        );
        assert_eq!(route("shell", &describe), Route::AppDescribe);
        assert_eq!(route("shell-overridden", &describe), Route::AppDescribe);
        let schema = incoming("caller", "shell.scene.describe", Some("schema"), &[], "{}");
        assert_eq!(route("shell", &schema), Route::Scene(SceneVerb::Describe));
        let load = incoming(
            "loader",
            "shell.scene.load",
            Some("1"),
            &[("broker_origin", "local")],
            "{}",
        );
        assert_eq!(route("shell", &load), Route::Scene(SceneVerb::Load));
        let unknown = incoming("loader", "shell.no.such.verb", Some("2"), &[], "{}");
        assert_eq!(route("shell", &unknown), Route::Unknown);
        let fire_and_forget = incoming("loader", "shell.no.such.verb", None, &[], "{}");
        assert_eq!(route("shell", &fire_and_forget), Route::Ignore);
        let registry = r#"{"path":"services.registered","new":["comp","scenes"]}"#;
        let live = incoming(
            "noded",
            "",
            None,
            &[("topic", REGISTRY_TOPIC), ("broker_origin", "local")],
            registry,
        );
        assert_eq!(
            route("shell", &live),
            Route::Live(BTreeSet::from(["comp".to_string(), "scenes".to_string()]))
        );
        let forged = incoming(
            "noded",
            "",
            None,
            &[("topic", REGISTRY_TOPIC), ("broker_origin", "mesh")],
            registry,
        );
        assert_eq!(route("shell", &forged), Route::Forged);
        let other_path = incoming(
            "noded",
            "",
            None,
            &[("topic", REGISTRY_TOPIC)],
            r#"{"path":"x","new":[]}"#,
        );
        assert_eq!(route("shell", &other_path), Route::Ignore);
        let delivery = incoming(
            "x",
            "shell.scene.load",
            None,
            &[("topic", "x.changed")],
            "{}",
        );
        assert_eq!(
            route("shell", &delivery),
            Route::Ignore,
            "a topic delivery is never a request"
        );
    }
}

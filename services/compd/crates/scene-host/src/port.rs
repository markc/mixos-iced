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
use application::presentation::native::{
    Event as SettingsEvent, Jobs, Mailbox, Worker as SettingsWorker,
};
use bus::native_client::BoundedIncomingEvent;
use bus::{IncomingCommand, SupervisedClient, SupervisedError};
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
/// Scene events in flight to citizens (contract: at most 128 pending).
pub const MAX_PENDING_EVENTS: usize = 128;
/// A citizen's handler answers within this, or the event is abandoned.
pub const EVENT_TIMEOUT: Duration = Duration::from_secs(2);
const SEND_TIMEOUT: Duration = Duration::from_secs(2);
const RETRY_INITIAL: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(30);
const SHUTDOWN_GRACE: Duration = Duration::from_millis(300);
const DEREGISTER_BUDGET: Duration = Duration::from_millis(200);

/// Wakes the engine's loop; called once per delivered message.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// How the host registers and where the broker is.
#[derive(Clone, Debug)]
pub struct HostConfig {
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
    /// The worker gave up registering (the broker refused the name); the host is off.
    Refused(String),
    Request(Request),
    /// The broker's full set of registered services, from a registry diff.
    Live(BTreeSet<String>),
}

enum Outbound {
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
    Publish { panel: bool, wire: String },
}

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
    outbound: tokio_mpsc::UnboundedSender<Outbound>,
    sink: EventSink,
    shutdown: watch::Sender<bool>,
    completion: Receiver<()>,
    thread: Option<thread::JoinHandle<()>>,
    /// The registered client, once there is one: the engine reads the live
    /// connection generation from it directly (an atomic load), so a
    /// reconnect the worker has not yet reported still fences.
    client: Arc<OnceLock<Arc<SupervisedClient>>>,
    binding: settings::Binding,
    settings_jobs: watch::Sender<Option<Jobs>>,
    settings_mailbox: Mailbox<Look>,
    registry: RegistryMailbox,
}

type RegistryMailbox = Arc<Mutex<Option<(u64, BTreeSet<String>)>>>;

struct SettingsLane {
    binding: settings::Binding,
    jobs: watch::Receiver<Option<Jobs>>,
    mailbox: Mailbox<Look>,
    registry: RegistryMailbox,
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
        let binding =
            settings::session::binding().map_err(|error| format!("settings session: {error:?}"))?;
        let (settings_jobs, jobs) = watch::channel(None);
        let settings_mailbox = Mailbox::default();
        let registry = RegistryMailbox::default();
        let lane = SettingsLane {
            binding: binding.clone(),
            jobs,
            mailbox: settings_mailbox.clone(),
            registry: Arc::clone(&registry),
        };
        let (inbound_tx, inbound) = mpsc::sync_channel(INBOUND_CAPACITY);
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
                ));
            })
            .map_err(|error| format!("failed to spawn the scene host's Bus worker: {error}"))?;
        let sink = EventSink {
            events,
            pending: pending_events,
        };
        Ok(Self {
            inbound,
            outbound,
            sink,
            shutdown,
            completion,
            thread: Some(thread),
            client,
            binding,
            settings_jobs,
            settings_mailbox,
            registry,
        })
    }

    pub(crate) fn settings_binding(&self) -> settings::Binding {
        self.binding.clone()
    }
    pub(crate) fn settings_jobs(&self, jobs: Jobs) {
        self.settings_jobs.send_replace(Some(jobs));
    }
    pub(crate) fn take_settings(&self) -> Vec<SettingsEvent<Look>> {
        self.settings_mailbox.take()
    }
    pub(crate) fn settings_generation(&self) -> Option<u64> {
        self.client
            .get()
            .and_then(|client| settings::native::live_generation(client))
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
        })
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
    Scene(SceneVerb),
    Live(BTreeSet<String>),
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

async fn connect(config: &HostConfig, shutdown: &mut watch::Receiver<bool>) -> Connected {
    let names = candidate_names(config.service_override.as_deref());
    let mut refusals = Vec::new();
    for name in &names {
        let mut delay = RETRY_INITIAL;
        loop {
            let attempt = SupervisedClient::connect_options(name, &config.noded_url)
                .fatal_on_registration_rejection(true)
                .bounded_incoming(INBOUND_CAPACITY)
                .connect();
            let result = tokio::select! {
                result = attempt => result,
                _ = shutdown.changed() => return Connected::Stopped,
            };
            match result {
                Ok(client) => return Connected::Client(Arc::new(client), name.clone()),
                Err(error) => match registration_refusal(&error) {
                    Some(refusal) => {
                        tracing::error!(
                            "SCENE HOST: the broker refused the Bus name `{name}` ({refusal}); \
                             compd does NOT host Mix Scenes under it"
                        );
                        refusals.push(format!("{name}: {refusal}"));
                        break;
                    }
                    None => {
                        tracing::warn!(%error, "scene host: broker unavailable at {}; retrying in {delay:?}", config.noded_url);
                        tokio::select! {
                            _ = tokio::time::sleep(delay) => {}
                            _ = shutdown.changed() => return Connected::Stopped,
                        }
                        delay = (delay * 2).min(RETRY_MAX);
                    }
                },
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

fn registration_refusal(error: &SupervisedError) -> Option<String> {
    error
        .registration_rejection()
        .map(|(rc, message)| format!("rc {rc}: {message}"))
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
    mut lane: SettingsLane,
) {
    let settings_send = |event| {
        if lane.mailbox.publish(event) {
            (delivery.waker)();
        }
    };
    let mut settings_worker = SettingsWorker::offline(crate::appearance::build);
    let mut connect_shutdown = shutdown.clone();
    let mut connecting = Some(Box::pin(connect(&config, &mut connect_shutdown)));
    // The existing connection attempt must not stop the resource worker. A
    // refused service still receives offline presentation work until shutdown.
    let (client, service) = loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() { return; }
            }
            changed = lane.jobs.changed() => {
                if changed.is_err() { return; }
                if let Some(jobs) = lane.jobs.borrow_and_update().clone() { settings_worker.replace(jobs); }
            }
            event = settings_worker.next() => {
                if let Some(event) = event.take() { settings_send(event); }
            }
            result = async { connecting.as_mut().expect("guarded connection").await }, if connecting.is_some() => {
                connecting = None;
                match result {
                    Connected::Client(client, service) => break (client, service),
                    Connected::Refused(reason) => { let _ = delivery.send(Inbound::Refused(reason)); }
                    Connected::Stopped => return,
                }
            }
        }
    };
    let _ = published.set(Arc::clone(&client));
    settings_worker.connect(Arc::clone(&client));
    if let Some(jobs) = lane.jobs.borrow_and_update().clone() {
        settings_worker.replace(jobs);
    }
    settings_send(SettingsEvent::Wake);
    tracing::info!(
        "scene host: registered as `{service}` via {}",
        config.noded_url
    );
    if !delivery.send(Inbound::Registered(service.clone())) {
        tracing::warn!("scene host: the engine's queue refused the registration notice");
    }
    let Some(mut incoming) = client.incoming_bounded() else {
        tracing::error!("scene host: the Bus client has no incoming lane; the host is OFF");
        let _ = delivery.send(Inbound::Refused("no incoming lane".into()));
        return;
    };
    let mut lifecycle = client.subscribe_state();
    let topics = (
        format!("{service}.scene.changed"),
        format!("{service}.panel.changed"),
    );
    let mut flights = tokio::task::JoinSet::new();
    let mut replies = tokio::task::JoinSet::new();
    let mut sends = tokio::task::JoinSet::new();
    let mut registry = Some(Box::pin(registry_read(Arc::clone(&client), Duration::ZERO)));
    let mut registry_retries = 0;
    let mut registry_subscription = Some(Box::pin(registry_subscribe(Arc::clone(&client))));
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            changed = lane.jobs.changed() => {
                if changed.is_err() { break; }
                if let Some(jobs) = lane.jobs.borrow_and_update().clone() { settings_worker.replace(jobs); }
            }
            changed = lifecycle.changed() => {
                if changed.is_err() { break; }
                lifecycle.borrow_and_update();
                settings_send(SettingsEvent::Wake);
                registry = None;
                registry_subscription = Some(Box::pin(registry_subscribe(Arc::clone(&client))));
                registry_retries = 0;
            }
            subscribed = async { registry_subscription.as_mut().expect("guarded registry subscription").await }, if registry_subscription.is_some() => {
                registry_subscription = None;
                if subscribed {
                    // Subscribe before read so a departure in the gap cannot
                    // leave a successful but obsolete full-set baseline.
                    registry = Some(Box::pin(registry_read(Arc::clone(&client), Duration::ZERO)));
                    registry_retries = 0;
                }
            }
            result = async { registry.as_mut().expect("guarded registry read").await }, if registry.is_some() => {
                registry = None;
                if let Some((generation, services)) = result {
                    if settings::native::live_generation(&client) == Some(generation) {
                        *lane.registry.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some((generation, services));
                        (delivery.waker)();
                    }
                } else if settings::native::live_generation(&client).is_some() && registry_retries < 3 {
                    let delay = RETRY_INITIAL * (1 << registry_retries);
                    registry_retries += 1;
                    registry = Some(Box::pin(registry_read(Arc::clone(&client), delay)));
                } else {
                    tracing::warn!("scene host: registry recovery paused until next lifecycle or loss event");
                }
            }
            event = settings_worker.next() => {
                if let Some(event) = event.take() { settings_send(event); }
            }
            command = incoming.recv(), if replies.len() < INBOUND_CAPACITY => {
                match command {
                    Some(BoundedIncomingEvent::Command(command)) => {
                        if let Some(decoded) = settings::native::Decoded::from_command(&lane.binding, &command) {
                            settings_send(SettingsEvent::Delivery(decoded));
                        } else if let Route::Live(services) = route(&service, &command) {
                            if settings::native::live_generation(&client) == Some(command.generation) {
                                // Each registry notice is a full set. It supersedes
                                // an older read and cannot be dropped by scene RPCs.
                                registry = None;
                                *lane.registry.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some((command.generation, services));
                                (delivery.waker)();
                            }
                        } else {
                            if let Some((command, rc, body)) = admit(&service, &delivery, command) {
                                let client = Arc::clone(&client);
                                replies.spawn(async move { refuse(&client, &command, rc, body).await; });
                            }
                        }
                    }
                    Some(BoundedIncomingEvent::Overflow { .. }) => {
                        settings_send(SettingsEvent::Lost);
                        registry = Some(Box::pin(registry_read(Arc::clone(&client), Duration::ZERO)));
                        registry_retries = 0;
                        tracing::warn!("scene host: incoming queue overflow; recovering settings and registry");
                    }
                    None => break,
                }
            }
            Some(message) = outbound.recv(), if sends.is_empty() => {
                let (client, topics) = (Arc::clone(&client), topics.clone());
                // Preserve publish/reply order without blocking settings work.
                sends.spawn(async move { send(&client, &topics, message).await; });
            }
            Some(event) = events.recv() => {
                let client = Arc::clone(&client);
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
        }
    }
    settings_send(SettingsEvent::Wake);
    // Finish the earlier active send before later queued messages. A bounded
    // drain that expires cancels the remaining sequence, preserving its order.
    let _ = tokio::time::timeout(Duration::from_millis(50), async {
        while sends.join_next().await.is_some() {}
        while let Ok(message) = outbound.try_recv() {
            send(&client, &topics, message).await;
        }
    })
    .await;
    flights.abort_all();
    replies.abort_all();
    sends.abort_all();
    match tokio::time::timeout(DEREGISTER_BUDGET, client.deregister()).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::debug!(%error, "scene host deregister did not complete cleanly"),
        Err(_) => tracing::debug!("scene host deregister timed out"),
    }
    client.close().await;
    settings_send(SettingsEvent::Wake);
}

async fn registry_read(
    client: Arc<SupervisedClient>,
    delay: Duration,
) -> Option<(u64, BTreeSet<String>)> {
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
    let generation = settings::native::live_generation(&client)?;
    let services = tokio::time::timeout(
        SEND_TIMEOUT,
        client.call(
            "noded",
            "noded.props.get",
            json!({"path":"services.registered"}),
        ),
    )
    .await
    .ok()?
    .ok()?;
    let services = services
        .as_array()?
        .iter()
        .map(|name| name.as_str().map(str::to_owned))
        .collect::<Option<BTreeSet<_>>>()?;
    Some((generation, services))
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
    let topic = match &message {
        Outbound::Publish { panel: true, .. } => panel_topic.as_str(),
        _ => scene_topic.as_str(),
    };
    let result = match &message {
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
    };
    if let Err(error) = result {
        match message {
            Outbound::Reply { to, command, .. } => {
                tracing::warn!(%error, to = %to, command = %command, "scene host reply not delivered")
            }
            Outbound::Publish { .. } => tracing::warn!(%error, topic, "scene host publish failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        session: &mut application::presentation::native::Session<Look>,
        panels: &mut crate::panels::Panels,
        revision: Option<u64>,
    ) -> usize {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let mut changes = 0;
        loop {
            for event in port.take_settings() {
                let (change, jobs) =
                    session.handle_with(event, port.settings_generation(), |presentation| {
                        panels.set_preferences(presentation.content().preferences.clone());
                    });
                changes += usize::from(change.is_some());
                port.settings_jobs(jobs);
            }
            let ready = match revision {
                Some(revision) => {
                    session.host().kind() == Some(settings::fallback::PresentationKind::Current)
                        && session.host().consumer().applied().is_some_and(|snapshot| {
                            snapshot.revision == settings::Revision(revision)
                        })
                }
                None => {
                    session.host().kind() == Some(settings::fallback::PresentationKind::Embedded)
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
    ) -> (
        Port,
        Receiver<()>,
        application::presentation::native::Session<Look>,
    ) {
        install_settings_fonts();
        let (notify, wake) = mpsc::channel();
        let port = Port::start(
            HostConfig {
                service_override: None,
                noded_url: url,
            },
            Arc::new(move || {
                let _ = notify.send(());
            }),
        )
        .unwrap();
        let mut session = application::presentation::native::Session::new(
            settings::consumer::Consumer::for_shell(port.settings_binding()).unwrap(),
        );
        let (_, jobs) = session.handle(SettingsEvent::Wake, port.settings_generation());
        port.settings_jobs(jobs);
        (port, wake, session)
    }

    #[test]
    #[ignore = "requires settings_test.mix isolated environment"]
    fn settings_offline_port_prepares_fallback_while_connecting() {
        // Own an unresponsive endpoint that never completes the WS upgrade,
        // without selecting a port
        // somebody else can claim between binding and the connection attempt.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let (port, wake, mut session) =
            settings_port(format!("ws://{}/ws", listener.local_addr().unwrap()));
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
                .host()
                .presentation()
                .unwrap()
                .content()
                .chrome(decor::ChromeStyle::Mac)
                .tokens,
            decor::TokenSource::Prepared
        );
        port.finish();
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
        let (port, wake, mut session) = settings_port(url.clone());
        let mut panels = crate::panels::Panels::default();
        panels.ensure("fixture", (1280.0, 800.0));
        assert_eq!(
            settings_drive(&port, &wake, &mut session, &mut panels, Some(1)),
            1
        );
        let before = session.host().presentation().unwrap().content().clone();
        let controller = SupervisedClient::connect_options("quoin-settings-controller", &url)
            .connect()
            .await
            .unwrap();
        let current = session.host().consumer().current().unwrap();
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
        let after = session.host().presentation().unwrap().content().clone();
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
        let current = session.host().consumer().current().unwrap();
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
        let current = session.host().consumer().current().unwrap();
        let invalid = controller.call("settingsd", "settings.apply", json!({
            "binding":current.binding, "expected_incarnation":current.incarnation,
            "expected_revision":"3", "operation_id":"quoin-duplicate-edge",
            "changes":{"shell.panels.duplicate":{"edge":"bottom","mode":"dock","thickness":100}}
        })).await.unwrap();
        assert_eq!(invalid["status"], "changed");
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while session.host().consumer().fault().is_none() {
            for event in port.take_settings() {
                let (change, jobs) =
                    session.handle_with(event, port.settings_generation(), |presentation| {
                        panels.set_preferences(presentation.content().preferences.clone());
                    });
                assert!(
                    change.is_none(),
                    "failed whole preparation must not activate either resource"
                );
                port.settings_jobs(jobs);
            }
            if session.host().consumer().fault().is_none() {
                wake.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                    .expect("Quoin invalid-policy diagnostic did not arrive before deadline");
            }
        }
        assert_eq!(
            session.host().consumer().applied().unwrap().revision,
            settings::Revision(3)
        );
        assert_eq!(
            panels.state("fixture", crate::seat::Edge::Bottom)["mode"],
            "docked"
        );
        assert_eq!(
            session
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
        let (change, jobs) = session.handle(SettingsEvent::Wake, port.settings_generation());
        port.settings_jobs(jobs);
        assert!(change.is_none());
        assert_eq!(
            session.host().kind(),
            Some(settings::fallback::PresentationKind::LastGood)
        );
        assert_eq!(
            session.host().consumer().applied().unwrap().revision,
            settings::Revision(3)
        );
        controller.close().await;
        port.finish();
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
    fn frames_route_to_verbs_registry_sets_or_nothing() {
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

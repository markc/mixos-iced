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
use std::sync::{Arc, OnceLock};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread;
use std::time::Duration;

use bus::{BoundedIncomingEvent, IncomingCommand, SupervisedClient, SupervisedError};
use application::presentation::native::{Event as SettingsEvent, Jobs, Mailbox, Worker as SettingsWorker};
use crate::appearance::Look;
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
    Reply { to: String, command: String, id: Option<String>, rc: u8, body: String, generation: u64 },
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
}

struct SettingsLane {
    binding: settings::Binding,
    jobs: watch::Receiver<Option<Jobs>>,
    mailbox: Mailbox<Look>,
}

/// The names to try, in order: `shell`, then the override when it differs.
pub fn candidate_names(service_override: Option<&str>) -> Vec<String> {
    let mut names = vec![DEFAULT_SERVICE.to_owned()];
    if let Some(name) = service_override.map(str::trim).filter(|name| !name.is_empty() && *name != DEFAULT_SERVICE) {
        names.push(name.to_owned());
    }
    names
}

impl Port {
    /// Start the worker. The broker connection is the worker's: a missing
    /// noded is retried with backoff and never blocks the compositor.
    pub fn start(config: HostConfig, waker: Waker) -> Result<Self, String> {
        let binding = settings::session::binding().map_err(|error| format!("settings session: {error:?}"))?;
        let (settings_jobs, jobs) = watch::channel(None);
        let settings_mailbox = Mailbox::default();
        let lane = SettingsLane { binding: binding.clone(), jobs, mailbox: settings_mailbox.clone() };
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
                let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        tracing::error!(%error, "scene host: failed to build its Bus runtime");
                        return;
                    }
                };
                let delivery = Delivery { sender: inbound_tx, waker };
                runtime.block_on(worker(config, delivery, outbound_rx, events_rx, pending, shutdown_rx, published, lane));
            })
            .map_err(|error| format!("failed to spawn the scene host's Bus worker: {error}"))?;
        let sink = EventSink { events, pending: pending_events };
        Ok(Self { inbound, outbound, sink, shutdown, completion, thread: Some(thread), client, binding, settings_jobs, settings_mailbox })
    }

    pub(crate) fn settings_binding(&self) -> settings::Binding { self.binding.clone() }
    pub(crate) fn settings_jobs(&self, jobs: Jobs) { self.settings_jobs.send_replace(Some(jobs)); }
    pub(crate) fn take_settings(&self) -> Vec<SettingsEvent<Look>> { self.settings_mailbox.take() }
    pub(crate) fn settings_generation(&self) -> Option<u64> {
        self.client.get().and_then(|client| settings::native::live_generation(client))
    }

    /// The live broker connection's generation; `None` before registering.
    pub fn connection_generation(&self) -> Option<u64> {
        self.client.get().map(|client| client.connection_generation())
    }

    /// The next message from the worker, without blocking.
    pub fn try_recv(&self) -> Option<Inbound> {
        self.inbound.try_recv().ok()
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
        let Some(thread) = self.thread.take() else { return };
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
            tracing::warn!(citizen, verb, "scene event dropped: {MAX_PENDING_EVENTS} events already pending");
            return false;
        }
        let event = Event { to: citizen.to_owned(), verb: verb.to_owned(), body };
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
        if let Some(next) = names.iter().skip_while(|candidate| *candidate != name).nth(1) {
            tracing::error!("SCENE HOST: falling back to the configured override name `{next}`");
        }
    }
    let reason = if names.len() == 1 {
        format!("{}; no --scene-service / scene_service override is configured, so the scene host is OFF", refusals.join("; "))
    } else {
        format!("{}; the scene host is OFF", refusals.join("; "))
    };
    tracing::error!("SCENE HOST: {reason}");
    Connected::Refused(reason)
}

fn registration_refusal(error: &SupervisedError) -> Option<String> {
    error.registration_rejection().map(|(rc, message)| format!("rc {rc}: {message}"))
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
        if lane.mailbox.publish(event) { (delivery.waker)(); }
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
    if let Some(jobs) = lane.jobs.borrow_and_update().clone() { settings_worker.replace(jobs); }
    settings_send(SettingsEvent::Wake);
    tracing::info!("scene host: registered as `{service}` via {}", config.noded_url);
    if !delivery.send(Inbound::Registered(service.clone())) {
        tracing::warn!("scene host: the engine's queue refused the registration notice");
    }
    let Some(mut incoming) = client.incoming_bounded() else {
        tracing::error!("scene host: the Bus client has no incoming lane; the host is OFF");
        let _ = delivery.send(Inbound::Refused("no incoming lane".into()));
        return;
    };
    let mut lifecycle = client.subscribe_state();
    let topics = (format!("{service}.scene.changed"), format!("{service}.panel.changed"));
    let mut flights = tokio::task::JoinSet::new();
    let registry_client = Arc::clone(&client);
    flights.spawn(async move {
        if let Err(error) = registry_client.subscribe_topic(REGISTRY_TOPIC).await {
            tracing::warn!(%error, "scene host: registry subscription failed; owner departures will not unload scenes");
        }
    });
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
            }
            event = settings_worker.next() => {
                if let Some(event) = event.take() { settings_send(event); }
            }
            command = incoming.recv() => {
                match command {
                    Some(BoundedIncomingEvent::Command(command)) => {
                        if let Some(decoded) = settings::native::Decoded::from_command(&lane.binding, &command) {
                            settings_send(SettingsEvent::Delivery(decoded));
                        } else { admit(&service, &client, &delivery, command).await; }
                    }
                    Some(BoundedIncomingEvent::Overflow { .. }) => {
                        settings_send(SettingsEvent::Lost);
                        tracing::warn!("scene host: incoming queue overflow; refreshing settings, registry reconciliation required");
                    }
                    None => break,
                }
            }
            Some(message) = outbound.recv() => send(&client, &topics, message).await,
            Some(event) = events.recv() => {
                let client = Arc::clone(&client);
                let pending = Arc::clone(&pending_events);
                flights.spawn(async move {
                    let call = client.call(&event.to, &event.verb, event.body);
                    match tokio::time::timeout(EVENT_TIMEOUT, call).await {
                        Ok(Ok(_)) => {}
                        Ok(Err(error)) => tracing::debug!(%error, to = %event.to, verb = %event.verb, "scene event failed"),
                        Err(_) => tracing::debug!(to = %event.to, verb = %event.verb, "scene event timed out"),
                    }
                    pending.fetch_sub(1, Ordering::AcqRel);
                });
            }
            Some(_) = flights.join_next(), if !flights.is_empty() => {}
        }
    }
    // Answer what the engine already queued, then leave the name.
    while let Ok(message) = outbound.try_recv() {
        send(&client, &topics, message).await;
    }
    flights.abort_all();
    match tokio::time::timeout(DEREGISTER_BUDGET, client.deregister()).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::debug!(%error, "scene host deregister did not complete cleanly"),
        Err(_) => tracing::debug!("scene host deregister timed out"),
    }
    client.close().await;
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

async fn admit(service: &str, client: &SupervisedClient, delivery: &Delivery, command: IncomingCommand) {
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
                refuse(client, &command, 11, body).await;
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
            refuse(client, &command, 10, json!({"error_code":"UNKNOWN_VERB","message":message})).await;
        }
        Route::Ignore => {}
    }
}

async fn send(client: &SupervisedClient, (scene_topic, panel_topic): &(String, String), message: Outbound) {
    let topic = match &message {
        Outbound::Publish { panel: true, .. } => panel_topic.as_str(),
        _ => scene_topic.as_str(),
    };
    let result = match &message {
        Outbound::Reply { generation, command, .. } if *generation != client.connection_generation() => {
            // Its caller's connection is gone; a reply on the new one would
            // reach whoever holds that name now, under a stale id.
            tracing::debug!(command = %command, "scene host reply dropped: admitted on an earlier connection");
            Ok(())
        }
        Outbound::Reply { generation, to, command, id, rc, body, .. } => {
            tokio::time::timeout(SEND_TIMEOUT, client.respond_parts(*generation, to, command, id.as_deref(), *rc, body))
                .await
                .map_err(|_| "timed out".to_string())
                .and_then(|result| result.map_err(|error| error.to_string()))
        }
        Outbound::Publish { panel, wire } => {
            let mut headers = BTreeMap::new();
            headers.insert("name".to_string(), topic.to_string());
            headers.insert("retain".to_string(), panel.to_string());
            match tokio::time::timeout(SEND_TIMEOUT, client.call_with_headers_raw("noded", "topic.publish", &headers, wire)).await {
                Err(_) => Err("timed out".to_string()),
                Ok(Err(error)) => Err(error.to_string()),
                Ok(Ok((0, _, _))) => Ok(()),
                Ok(Ok((rc, body, _))) => Err(format!("topic.publish rejected with rc {rc}: {body}")),
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

    #[test]
    fn page_request_from_worker_wakes_idle_loop_and_schedules_frame() {
        use dispatcher::state::state::{Dispatch, RedrawReason};
        use smithay::reexports::calloop::EventLoop;
        use smithay::reexports::calloop::ping::make_ping;
        use smithay::reexports::wayland_server::Display;

        struct Data { dispatch: Dispatch, host: crate::host::Host, frames: u32 }
        let mut event_loop: EventLoop<'static, Data> = EventLoop::try_new().unwrap();
        let display: Display<Dispatch> = Display::new().unwrap();
        let mut dispatch = dispatcher::wire::wire::new_dispatch(&display.handle(), None, event_loop.handle());
        let (redraw, source) = make_ping().unwrap();
        dispatch.redraw.set_ping(redraw);
        dispatch.redraw.rendering("kms");
        dispatch.redraw.frame("kms", false);
        event_loop.handle().insert_source(source, |_, _, data: &mut Data| {
            assert!(data.dispatch.redraw.pending());
            data.dispatch.redraw.rendering("kms");
            assert!(data.dispatch.redraw.frame("kms", false).contains(RedrawReason::Publish));
            data.frames += 1;
        }).unwrap();

        let mut host = crate::host::Host::default();
        host.panels.ensure("DP-1", (1280.0, 800.0));
        let request = |verb, body: Value| Request {
            verb, from: "scenes".into(), command: "shell.panel.page.set".into(), id: Some("1".into()),
            body: body.to_string(), headers: BTreeMap::from([("broker_origin".into(), "local".into())]), generation: 1,
        };
        let mut no_layout = |_: &crate::store::SceneStore, _: &str, _: Option<&str>| Err(Value::Null);
        for name in ["calendar", "notifications"] {
            let source = format!("---\nscene: 1\nname: {name}\ncitizen: scenes\nwindow: {{\"kind\":\"edge\",\"edge\":\"right\"}}\n---\n```mix\nroot: {{widget: \"column\", children: []}}\n```\n");
            let answer = host.answer(&request(SceneVerb::Load, json!({"source":source})), "DP-1", Some(1), &mut no_layout);
            assert_eq!(answer.rc, 0);
        }
        host.panels.sync(&host.store);
        host.panels.page_set("DP-1", crate::seat::Edge::Right, "scene-calendar").unwrap();

        // This is the production worker Delivery, including its after-enqueue
        // Ping. There are no input, frame clock, or watchdog sources to help it.
        let (sender, inbound) = mpsc::sync_channel(INBOUND_CAPACITY);
        let (ping, source) = make_ping().unwrap();
        let delivery = Arc::new(Delivery { sender, waker: Arc::new(move || ping.ping()) });
        event_loop.handle().insert_source(source, move |_, _, data: &mut Data| {
            let Inbound::Request(request) = inbound.try_recv().unwrap() else { panic!("expected page request") };
            let answer = data.host.answer(&request, "DP-1", Some(1), &mut no_layout);
            assert_eq!(answer.rc, 0);
            // lib::service uses exactly this changed verdict to request pixels.
            assert!(answer.changed);
            data.dispatch.schedule_redraw(RedrawReason::Publish);
        }).unwrap();
        let mut data = Data { dispatch, host, frames: 0 };
        for (index, page) in ["scene-notifications", "scene-calendar"].into_iter().enumerate() {
            let delivery = Arc::clone(&delivery);
            let request = request(SceneVerb::PanelPageSet, json!({"edge":"right", "id":page}));
            let worker = std::thread::spawn(move || assert!(delivery.send(Inbound::Request(request))));
            event_loop.dispatch(Duration::from_secs(1), &mut data).unwrap();
            assert_eq!(data.host.panels.state("DP-1", crate::seat::Edge::Right)["page"], page);
            assert!(data.dispatch.redraw.needs("kms"));
            event_loop.dispatch(Duration::from_secs(1), &mut data).unwrap();
            worker.join().unwrap();
            assert_eq!(data.frames, index as u32 + 1);
        }
    }

    fn incoming(from: &str, command: &str, id: Option<&str>, headers: &[(&str, &str)], body: &str) -> IncomingCommand {
        IncomingCommand {
            generation: 0,
            from: from.into(),
            command: command.into(),
            id: id.map(str::to_owned),
            args: Value::Null,
            body: body.into(),
            headers: headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        }
    }

    #[test]
    fn shell_first_then_only_a_distinct_override() {
        assert_eq!(candidate_names(None), ["shell"]);
        assert_eq!(candidate_names(Some("shell")), ["shell"]);
        assert_eq!(candidate_names(Some("  ")), ["shell"]);
        assert_eq!(candidate_names(Some("shell-nested")), ["shell", "shell-nested"]);
    }

    #[test]
    fn frames_route_to_verbs_registry_sets_or_nothing() {
        let load = incoming("loader", "shell.scene.load", Some("1"), &[("broker_origin", "local")], "{}");
        assert_eq!(route("shell", &load), Route::Scene(SceneVerb::Load));
        let unknown = incoming("loader", "shell.no.such.verb", Some("2"), &[], "{}");
        assert_eq!(route("shell", &unknown), Route::Unknown);
        let fire_and_forget = incoming("loader", "shell.no.such.verb", None, &[], "{}");
        assert_eq!(route("shell", &fire_and_forget), Route::Ignore);
        let registry = r#"{"path":"services.registered","new":["comp","scenes"]}"#;
        let live = incoming("noded", "", None, &[("topic", REGISTRY_TOPIC), ("broker_origin", "local")], registry);
        assert_eq!(route("shell", &live), Route::Live(BTreeSet::from(["comp".to_string(), "scenes".to_string()])));
        let forged = incoming("noded", "", None, &[("topic", REGISTRY_TOPIC), ("broker_origin", "mesh")], registry);
        assert_eq!(route("shell", &forged), Route::Forged);
        let other_path = incoming("noded", "", None, &[("topic", REGISTRY_TOPIC)], r#"{"path":"x","new":[]}"#);
        assert_eq!(route("shell", &other_path), Route::Ignore);
        let delivery = incoming("x", "shell.scene.load", None, &[("topic", "x.changed")], "{}");
        assert_eq!(route("shell", &delivery), Route::Ignore, "a topic delivery is never a request");
    }
}

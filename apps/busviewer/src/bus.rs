// SPDX-License-Identifier: MIT OR Apache-2.0
//! One supervised native Bus connection plus the shared settings Lane on the
//! same worker, runtime, client and receiver. Topics drive refreshes; no
//! poller and no second transport exists here.
use ::bus::native_client::{
    BoundedIncomingEvent, ConnState, IncomingCommand, NodedClient, SupervisedClient,
    SupervisedError,
};
use application::iced::futures::channel::{mpsc, oneshot};
use application::presentation::native::{
    Event as SettingsEvent, Progress, Session, Ui, Worker, bridge,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};

#[derive(Debug, Clone, PartialEq)]
pub enum Delivery {
    Command { id: u64, verb: String, body: String },
    Changed,
    Settings,
    Refused { name_taken: bool, message: String },
    Forwarded(Result<(), String>),
    Connected,
    Disconnected,
}
#[derive(Debug, Clone)]
pub struct Reply {
    pub rc: u8,
    pub body: String,
}
enum Effect {
    Call(
        String,
        String,
        String,
        oneshot::Sender<Result<Reply, CallError>>,
    ),
}
#[derive(Debug, Clone)]
pub struct CallError {
    pub message: String,
    pub outcome_unknown: bool,
}
impl CallError {
    fn not_sent(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            outcome_unknown: false,
        }
    }
    fn transport(error: SupervisedError) -> Self {
        let outcome_unknown = !matches!(
            error,
            SupervisedError::Disconnected | SupervisedError::ShuttingDown
        );
        Self {
            message: error.to_string(),
            outcome_unknown,
        }
    }
}
impl From<String> for CallError {
    fn from(message: String) -> Self {
        Self {
            message,
            outcome_unknown: true,
        }
    }
}
impl From<&str> for CallError {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}
impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(f)
    }
}
impl std::error::Error for CallError {}
enum Control {
    Reply(u64, u8, Value),
    Forward,
    Quit,
}
#[derive(Clone)]
pub struct Handle {
    tx: tokio::sync::mpsc::Sender<Effect>,
    control: tokio::sync::mpsc::UnboundedSender<Control>,
    done: Arc<(Mutex<bool>, Condvar)>,
    client: Option<Arc<SupervisedClient>>,
    #[cfg(test)]
    records: Arc<Mutex<Vec<(u64, u8, Value)>>>,
    #[cfg(test)]
    stopped: Arc<std::sync::atomic::AtomicBool>,
    #[cfg(test)]
    forwards: Arc<std::sync::atomic::AtomicUsize>,
}
impl Handle {
    pub async fn raw(&self, service: &str, verb: &str, body: String) -> Result<Reply, CallError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Effect::Call(service.into(), verb.into(), body, tx))
            .await
            .map_err(|_| CallError::not_sent("Bus stopped; no call sent"))?;
        rx.await
            .map_err(|_| CallError::from("Bus request abandoned"))?
    }
    pub async fn call(&self, service: &str, verb: &str, args: Value) -> Result<Reply, String> {
        self.raw(service, verb, args.to_string())
            .await
            .map_err(|e| e.to_string())
    }
    pub fn reply(&self, id: u64, rc: u8, body: Value) {
        #[cfg(test)]
        self.records.lock().unwrap().push((id, rc, body.clone()));
        // Only the GUI sends replies, once per accepted command (at most 32).
        // This separate queue cannot lose a reply to outgoing call backpressure.
        // A closed receiver means the native connection has already ended.
        let _ = self.control.send(Control::Reply(id, rc, body));
    }
    pub fn forward(&self) {
        #[cfg(test)]
        self.forwards
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        // The handoff travels the owned unbounded control FIFO, so a full
        // effect or GUI channel can never lose it or leave the UI pending.
        let _ = self.control.send(Control::Forward);
    }
    pub fn quit(&self) {
        #[cfg(test)]
        self.stopped
            .store(true, std::sync::atomic::Ordering::SeqCst);
        // FIFO with replies: accepted replies are flushed before close.
        let _ = self.control.send(Control::Quit);
    }
    pub fn wait_done(&self) -> Result<(), String> {
        let (lock, changed) = &*self.done;
        let state = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // One shared shutdown budget covers the worker's reply drain, settings
        // cache flush and connection close (2s) plus thread teardown.
        let (state, _) = changed
            .wait_timeout_while(state, Duration::from_secs(5), |done| !*done)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *state {
            Ok(())
        } else {
            Err("Bus shutdown did not complete".into())
        }
    }
    /// The connection's live state, sampled on the UI loop. Queued lifecycle
    /// notices cannot authorise an activation after real loss.
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
    #[cfg(test)]
    pub fn sink() -> Self {
        let (tx, _rx) = tokio::sync::mpsc::channel(64);
        let (control, _rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            tx,
            control,
            done: Arc::new((Mutex::new(true), Condvar::new())),
            client: None,
            records: Arc::new(Mutex::new(Vec::new())),
            stopped: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            forwards: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }
    #[cfg(test)]
    pub fn responses(&self) -> Vec<(u64, u8, Value)> {
        self.records.lock().unwrap().clone()
    }
    #[cfg(test)]
    pub fn has_quit(&self) -> bool {
        self.stopped.load(std::sync::atomic::Ordering::SeqCst)
    }
    #[cfg(test)]
    pub fn forward_count(&self) -> usize {
        self.forwards.load(std::sync::atomic::Ordering::SeqCst)
    }
}

pub fn start(
    service: &str,
    url: &str,
) -> Result<
    (
        Handle,
        Ui<()>,
        appearance::settings::Prepared,
        mpsc::Receiver<Delivery>,
    ),
    String,
> {
    let (send, receive) = mpsc::channel(64);
    let (tx, rx) = tokio::sync::mpsc::channel(64);
    let (control, controls) = tokio::sync::mpsc::unbounded_channel();
    let (ready_send, ready_receive) = std::sync::mpsc::channel();
    let done = Arc::new((Mutex::new(false), Condvar::new()));
    let finished = done.clone();
    let service = service.to_owned();
    let url = url.to_owned();
    std::thread::Builder::new()
        .name("busviewer-bus".into())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            match runtime {
                Ok(runtime) => {
                    runtime.block_on(worker(service, url, send, rx, controls, ready_send));
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
        Handle {
            tx,
            control,
            done,
            client: Some(client),
            #[cfg(test)]
            records: Arc::new(Mutex::new(Vec::new())),
            #[cfg(test)]
            stopped: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            #[cfg(test)]
            forwards: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
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

/// Retained bounded GUI deliveries: at most one refusal and one handoff
/// completion per lifetime (the worker gates the single forward), newest
/// lifecycle edges coalesce, and everything flushes through the one retained
/// sender without ever awaiting GUI capacity on the Lane or shutdown.
/// Nothing here is ever silently dropped.
#[derive(Default)]
struct Outbox {
    refused: Option<(bool, String)>,
    forwarded: Option<Result<(), String>>,
    connection: Option<Delivery>,
    changed: bool,
    settings: bool,
}
impl Outbox {
    fn is_empty(&self) -> bool {
        self.refused.is_none()
            && self.forwarded.is_none()
            && self.connection.is_none()
            && !self.changed
            && !self.settings
    }
    fn push(&mut self, delivery: Delivery) {
        match delivery {
            // A later terminal edge must not overwrite the typed reason.
            Delivery::Refused { name_taken, message } => {
                if self.refused.is_none() {
                    self.refused = Some((name_taken, message));
                }
            }
            // The first accepted handoff completion is the only one kept.
            Delivery::Forwarded(result) => {
                if self.forwarded.is_none() {
                    self.forwarded = Some(result);
                }
            }
            Delivery::Connected | Delivery::Disconnected => self.connection = Some(delivery),
            Delivery::Changed => self.changed = true,
            Delivery::Settings => self.settings = true,
            Delivery::Command { .. } => unreachable!("commands are bounded at the source"),
        }
    }
    /// Put a delivery that could not be sent back where it was taken from.
    fn restore(&mut self, delivery: Delivery) {
        match delivery {
            Delivery::Refused { name_taken, message } => {
                self.refused = Some((name_taken, message))
            }
            Delivery::Forwarded(result) => self.forwarded = Some(result),
            Delivery::Connected | Delivery::Disconnected => self.connection = Some(delivery),
            Delivery::Changed => self.changed = true,
            Delivery::Settings => self.settings = true,
            Delivery::Command { .. } => unreachable!("commands are bounded at the source"),
        }
    }
    /// Deterministic order: the refusal precedes its handoff completion, then
    /// the newest connection edge, then the coalesced refreshes.
    fn next(&mut self) -> Option<Delivery> {
        if let Some((name_taken, message)) = self.refused.take() {
            return Some(Delivery::Refused { name_taken, message });
        }
        if let Some(result) = self.forwarded.take() {
            return Some(Delivery::Forwarded(result));
        }
        self.connection.take().or_else(|| {
            self.changed.then(|| {
                self.changed = false;
                Delivery::Changed
            })
        }).or_else(|| {
            self.settings.then(|| {
                self.settings = false;
                Delivery::Settings
            })
        })
    }
    /// Flush everything current capacity allows; false when deliveries remain
    /// (full or closed — the capacity branch observes closure and exits the
    /// normal owned shutdown).
    fn flush(&mut self, send: &mut mpsc::Sender<Delivery>) -> bool {
        loop {
            let Some(delivery) = self.next() else {
                return true;
            };
            match send.try_send(delivery) {
                Ok(()) => {}
                Err(error) => {
                    self.restore(error.into_inner());
                    return false;
                }
            }
        }
    }
}

/// A bounded BUSY/ARGUMENT refusal, tracked like every other reply so one
/// shared shutdown budget drains them all.
fn reply_refused(
    client: &Arc<SupervisedClient>,
    replies: &mut tokio::task::JoinSet<Result<(), String>>,
    command: IncomingCommand,
    body: String,
) {
    let client = Arc::clone(client);
    replies.spawn(async move {
        tokio::time::timeout(
            Duration::from_secs(2),
            client.respond(&command, 10, &body),
        )
        .await
        .map_err(|_| "Bus reply timed out".to_owned())?
        .map_err(|error| format!("Bus reply: {error}"))
    });
}

/// Drain an aborted owned operation set under the shared shutdown budget.
async fn drain_aborted<T: Send + 'static>(
    tasks: &mut tokio::task::JoinSet<T>,
    deadline: std::time::Instant,
) -> Option<String> {
    while !tasks.is_empty() {
        match tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            tasks.join_next(),
        )
        .await
        {
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => {
                tasks.abort_all();
                return Some("operation drain timed out".into());
            }
        }
    }
    None
}

async fn worker(
    service: String,
    url: String,
    mut send: mpsc::Sender<Delivery>,
    mut effects: tokio::sync::mpsc::Receiver<Effect>,
    mut controls: tokio::sync::mpsc::UnboundedReceiver<Control>,
    ready: Ready,
) {
    let consumer = match settings::session::binding()
        .and_then(|binding| settings::consumer::Consumer::for_app(binding, "busviewer"))
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
    // Nonblocking startup: registration and subscription replay run in the
    // supervisor. Failures surface as lifecycle deliveries, never here.
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
    let settings_worker = match config::AppDirs::resolve("busviewer") {
        Some(dirs) => Worker::offline_with_cache(dirs.cache().join("settings"), build),
        // The bridge reports the missing cache root as a configuration
        // diagnostic, distinct from any later write fault.
        None => Worker::offline(build),
    };
    let (ui, mut lane) = bridge(Session::new(consumer), settings_worker);
    let mut outbox = Outbox::default();
    if lane.connect(Arc::clone(&client)) {
        outbox.push(Delivery::Settings);
    }
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
        eprintln!("busviewer: static assets: {error}");
    }
    let mut pending: HashMap<u64, IncomingCommand> = HashMap::new();
    let mut next_id = 0;
    let permits = Arc::new(tokio::sync::Semaphore::new(16));
    let mut incoming_open = true;
    let mut connection_open = true;
    let mut forward_gate = false;
    let mut calls = tokio::task::JoinSet::new();
    let mut forwards = tokio::task::JoinSet::new();
    let mut replies = tokio::task::JoinSet::new();
    let mut topics = tokio::task::JoinSet::new();
    let mut lifecycle = None;
    loop {
        // Fast path: flush retained deliveries without blocking the Lane.
        outbox.flush(&mut send);
        // Sample once before waiting: fast registration may already have
        // completed, and every later edge wakes this loop again.
        let now = *connection.borrow_and_update();
        if lifecycle != Some(now) {
            lifecycle = Some(now);
            match now {
                ConnState::Connected => {
                    arm_topics(&client, &mut topics);
                    if lane.publish(SettingsEvent::Wake) {
                        outbox.push(Delivery::Settings);
                    }
                    outbox.push(Delivery::Connected);
                }
                ConnState::Fatal | ConnState::ShuttingDown => {
                    // Keep the Lane alive: the settings cache still drains on
                    // quit, and the UI decides whether to hand off or stay.
                    let reason = client.registration_rejection();
                    if lane.publish(SettingsEvent::Wake) {
                        outbox.push(Delivery::Settings);
                    }
                    outbox.push(Delivery::Refused {
                        name_taken: reason
                            .as_ref()
                            .is_some_and(|reason| reason.message.contains("already registered")),
                        message: reason
                            .map_or_else(|| "connection stopped".into(), |reason| reason.message),
                    });
                }
                ConnState::Disconnected => {
                    pending.clear();
                    if lane.publish(SettingsEvent::Wake) {
                        outbox.push(Delivery::Settings);
                    }
                    outbox.push(Delivery::Disconnected);
                }
                ConnState::Connecting => {
                    pending.clear();
                }
            }
        }
        tokio::select! {
            biased;
            control = controls.recv() => {
                match control {
                    Some(Control::Reply(id, rc, value)) => {
                        if let Some(command) = pending.remove(&id) {
                            let client = client.clone();
                            let body = value.to_string();
                            replies.spawn(async move {
                                tokio::time::timeout(Duration::from_secs(2), client.respond(&command, rc, &body))
                                    .await
                                    .map_err(|_| "Bus reply timed out".to_owned())?
                                    .map_err(|error| format!("Bus reply: {error}"))
                            });
                        }
                    }
                    Some(Control::Forward) => {
                        // One handoff per lifetime: a repeat while queued, in
                        // flight or already attempted is ignored; the accepted
                        // completion is never lost.
                        if !forward_gate {
                            forward_gate = true;
                            let url = url.clone();
                            let service = service.clone();
                            forwards.spawn(async move {
                                forward_async(&url, &service).await
                            });
                        }
                    }
                    Some(Control::Quit) | None => break,
                }
            }
            progress = lane.drive() => {
                match progress {
                    Progress::Wake => outbox.push(Delivery::Settings),
                    Progress::UiClosed => break,
                    Progress::Updated => {}
                }
            }
            // Capacity-aware retained delivery: the polled future borrows
            // only the sender and holds no outbox state, so select
            // cancellation leaves every pending delivery intact; a closed
            // channel exits through the normal owned shutdown below.
            ready = std::future::poll_fn(|cx| send.poll_ready(cx)), if !outbox.is_empty() => {
                match ready {
                    Ok(()) => {
                        let _ = outbox.flush(&mut send);
                    }
                    Err(_) => break,
                }
            }
            result = calls.join_next(), if !calls.is_empty() => {
                if let Some(Err(error)) = result {
                    eprintln!("busviewer: Bus call: {error}");
                }
            }
            result = forwards.join_next(), if !forwards.is_empty() => {
                match result {
                    Some(Ok(result)) => outbox.push(Delivery::Forwarded(result)),
                    Some(Err(error)) => eprintln!("busviewer: Bus work: {error}"),
                    None => {}
                }
            }
            result = replies.join_next(), if !replies.is_empty() => {
                if let Some(Ok(Err(error))) = result {
                    eprintln!("busviewer: Bus reply: {error}");
                }
            }
            result = topics.join_next(), if !topics.is_empty() => {
                if let Some(Ok(Err(error))) = result {
                    eprintln!("busviewer: topic: {error}");
                }
            }
            command = incoming.recv(), if incoming_open => {
                let command = match command {
                    Some(BoundedIncomingEvent::Command(command)) => command,
                    Some(BoundedIncomingEvent::Overflow { .. }) => {
                        // Settings keep their Lost accounting; the app keeps
                        // its existing recovery: a conservative refetch.
                        if lane.publish(SettingsEvent::Lost) {
                            outbox.push(Delivery::Settings);
                        }
                        outbox.push(Delivery::Changed);
                        continue;
                    }
                    None => {
                        incoming_open = false;
                        if lane.publish(SettingsEvent::Wake) {
                            outbox.push(Delivery::Settings);
                        }
                        continue;
                    }
                };
                // Settings frames are swallowed before any other topic.
                if let Some(wake) = lane.delivery(&command) {
                    if wake {
                        outbox.push(Delivery::Settings);
                    }
                    continue;
                }
                if let Some(topic) = command.topic() {
                    if topic == "noded.props.changed"
                        && command.headers.get("gap").is_none_or(|value| value != "true")
                        && serde_json::from_str::<Value>(&command.body).ok()
                            .is_some_and(|body| body["path"] != "services.registered")
                    {
                        continue;
                    }
                    outbox.push(Delivery::Changed);
                    continue;
                }
                if command.command.is_empty() {
                    reply_refused(&client, &mut replies, command,
                        "{\"error_code\":\"ARGUMENT\",\"message\":\"command verb is empty\"}".into());
                    continue;
                }
                if pending.len() >= 32 {
                    reply_refused(&client, &mut replies, command,
                        "{\"error_code\":\"BUSY\",\"message\":\"too many pending commands\"}".into());
                    continue;
                }
                next_id += 1;
                let delivery = Delivery::Command {
                    id: next_id,
                    verb: command.command.clone(),
                    body: if command.body.trim().is_empty() { "{}".into() } else { command.body.clone() },
                };
                pending.insert(next_id, command);
                // Never await GUI capacity: a full bounded channel refuses the
                // command instead of blocking the Lane or shutdown.
                if send.try_send(delivery).is_err() {
                    let command = pending.remove(&next_id).expect("just inserted");
                    reply_refused(&client, &mut replies, command,
                        "{\"error_code\":\"BUSY\",\"message\":\"BusViewer window is busy; command not accepted\"}".into());
                }
            }
            effect = effects.recv() => {
                let Some(effect) = effect else { break; };
                match effect {
                    Effect::Call(service, verb, body, reply) => {
                        let Ok(permit) = permits.clone().try_acquire_owned() else {
                            let _ = reply.send(Err(CallError::not_sent("Bus call capacity exhausted; no call sent")));
                            continue;
                        };
                        let client = client.clone();
                        // Owned and drained like every other operation; the
                        // permit and outcome semantics are unchanged.
                        calls.spawn(async move {
                            let _permit = permit;
                            let result = tokio::time::timeout(Duration::from_secs(30), client.call_with_headers_raw(&service, &verb, &BTreeMap::new(), &body)).await
                                .map_err(|_| CallError::from("Bus request timed out"))
                                .and_then(|v| v.map_err(CallError::transport))
                                .map(|(rc, body, _)| Reply { rc, body });
                            let _ = reply.send(result);
                        });
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
    // One shared shutdown budget for every tracked reply, every owned
    // operation, the settings cache and the connection close — never a 2s
    // wait per pending reply.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let mut faults = Vec::new();
    topics.abort_all();
    forwards.abort_all();
    calls.abort_all();
    while !replies.is_empty() {
        match tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), replies.join_next())
            .await
        {
            Ok(Some(Ok(Ok(())))) => {}
            Ok(Some(Ok(Err(error)))) => faults.push(error),
            Ok(Some(Err(error))) => faults.push(format!("Bus reply: {error}")),
            Ok(None) => break,
            Err(_) => {
                faults.push("Bus reply drain timed out".into());
                replies.abort_all();
                break;
            }
        }
    }
    if let Some(fault) = drain_aborted(&mut calls, deadline).await {
        faults.push(format!("Bus call: {fault}"));
    }
    if let Some(fault) = drain_aborted(&mut forwards, deadline).await {
        faults.push(format!("Bus work: {fault}"));
    }
    if let Some(fault) = drain_aborted(&mut topics, deadline).await {
        faults.push(format!("topic: {fault}"));
    }
    if let Err(error) = lane.flush_cache(deadline).await {
        faults.push(format!("settings cache: {}: {}", error.code, error.message));
    }
    if tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), client.close())
        .await
        .is_err()
    {
        faults.push("Bus close timed out".into());
    }
    let _ = send.try_send(Delivery::Disconnected);
    eprintln!("BUSVIEWER_SHUTDOWN {}", json!({"faults": faults}));
}

fn arm_topics(
    client: &Arc<SupervisedClient>,
    tasks: &mut tokio::task::JoinSet<Result<(), String>>,
) {
    tasks.abort_all();
    // No theme.changed subscription: the settings pipeline is the only
    // appearance authority and the frontend never installs fonts itself.
    for topic in ["noded.props.changed".to_owned()] {
        let client = Arc::clone(client);
        tasks.spawn(async move {
            tokio::time::timeout(Duration::from_secs(2), client.subscribe_topic(&topic))
                .await
                .map_err(|_| format!("{topic}: subscription timed out"))?
                .map_err(|error| format!("{topic}: {error}"))
        });
    }
}

async fn forward_async(url: &str, service: &str) -> Result<(), String> {
    // Activation may arrive after registration but before the first map.
    tokio::time::timeout(Duration::from_secs(20), async {
        let client = NodedClient::connect_anonymous(url)
            .await
            .map_err(|e| e.to_string())?;
        let result = client
            .call_with_headers_raw(service, "busviewer.show", &BTreeMap::new(), "{}")
            .await;
        client.close().await;
        let (rc, body, _) = result.map_err(|e| e.to_string())?;
        if rc == 0 {
            Ok(())
        } else {
            Err(format!("activation refused: {body}"))
        }
    })
    .await
    .map_err(|_| "activation timed out".to_owned())?
}
fn anonymous(url: &str, service: &str, verb: &str, args: Value) -> Result<Reply, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(async {
        // Activation may arrive after registration but before the first map.
        let budget = if verb == "busviewer.show" { 20 } else { 5 };
        tokio::time::timeout(Duration::from_secs(budget), async {
            let client = NodedClient::connect_anonymous(url)
                .await
                .map_err(|e| e.to_string())?;
            let result = client
                .call_with_headers_raw(service, verb, &BTreeMap::new(), &args.to_string())
                .await;
            client.close().await;
            let (rc, body, _) = result.map_err(|e| e.to_string())?;
            Ok(Reply { rc, body })
        })
        .await
        .map_err(|_| "activation timed out".to_owned())?
    })
}
pub fn probe(url: &str, service: &str) -> bool {
    anonymous(url, service, "busviewer.ping", json!({})).is_ok_and(|r| r.rc == 0)
}
pub fn forward(url: &str, service: &str) -> Result<(), String> {
    let body = json!({});
    let reply = anonymous(url, service, "busviewer.show", body)?;
    if reply.rc != 0 {
        Err(format!("activation refused: {}", reply.body))
    } else {
        Ok(())
    }
}

async fn json_call(handle: &Handle, service: &str, verb: &str) -> Result<Value, String> {
    let reply = handle
        .raw(service, verb, String::new())
        .await
        .map_err(|e| e.to_string())?;
    if reply.rc >= 10 {
        return Err(format!("rc = {}: {}", reply.rc, reply.body));
    }
    serde_json::from_str(&reply.body).map_err(|e| e.to_string())
}
async fn describe(handle: &Handle, service: &str) -> Result<Vec<crate::model::Verb>, String> {
    let help = match json_call(handle, service, "HELP").await {
        Ok(value) => crate::model::parse_verbs(&value),
        Err(error) => Err(error),
    };
    match help {
        Ok(verbs) => Ok(verbs),
        Err(help_error) => match json_call(handle, service, "app.describe").await {
            Ok(value) => crate::model::parse_verbs(&value),
            Err(error) => Err(format!("HELP: {help_error}\napp.describe: {error}")),
        },
    }
}
/// Eight descriptions at a time; failed citizens do not stop later probes.
pub async fn discover(handle: Handle) -> crate::model::Snapshot {
    use application::iced::futures::{StreamExt, stream};
    let mut snapshot = crate::model::Snapshot::default();
    let names = match json_call(&handle, "noded", "noded.list")
        .await
        .and_then(|v| crate::model::services(&v))
    {
        Ok(names) => names,
        Err(error) => {
            snapshot.error = Some(error);
            return snapshot;
        }
    };
    match json_call(&handle, "noded", "noded.peers").await {
        Ok(value) => snapshot.peers = crate::model::peers(&value),
        Err(error) => snapshot.peer_error = Some(error),
    }
    let results = stream::iter(names.into_iter().map(|name| {
        let handle = handle.clone();
        async move {
            let result = describe(&handle, &name).await;
            (name, result)
        }
    }))
    .buffer_unordered(8)
    .collect::<Vec<_>>()
    .await;
    snapshot.services.extend(results);
    snapshot
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replies_and_quit_survive_full_call_queue_in_order() {
        let (tx, _rx) = tokio::sync::mpsc::channel(64);
        let (control, mut controls) = tokio::sync::mpsc::unbounded_channel();
        let handle = Handle {
            tx,
            control,
            ..Handle::sink()
        };
        for _ in 0..64 {
            let (reply, _rx) = oneshot::channel();
            assert!(
                handle
                    .tx
                    .try_send(Effect::Call(
                        "example".into(),
                        "echo".into(),
                        String::new(),
                        reply
                    ))
                    .is_ok()
            );
        }
        handle.reply(42, 0, json!({"ok":true}));
        handle.quit();
        assert!(matches!(controls.try_recv(), Ok(Control::Reply(42, 0, _))));
        assert!(matches!(controls.try_recv(), Ok(Control::Quit)));
        assert!(handle.wait_done().is_ok());
    }
    #[test]
    fn unsent_calls_and_lost_replies_have_distinct_outcomes() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let error = runtime
            .block_on(Handle::sink().raw("example", "echo", String::new()))
            .unwrap_err();
        assert!(!error.outcome_unknown);
        assert!(!CallError::transport(SupervisedError::Disconnected).outcome_unknown);
        assert!(!CallError::transport(SupervisedError::ShuttingDown).outcome_unknown);
        assert!(CallError::from("lost response").outcome_unknown);
    }
    #[test]
    fn sink_handle_reads_connected_without_a_client() {
        let handle = Handle::sink();
        assert!(handle.connected(), "a sink without a client reads as connected");
        assert!(!handle.ever_registered());
        assert_eq!(handle.settings_generation(), None);
        assert_eq!(handle.forward_count(), 0);
    }
    /// A genuine stalled-mailbox regression: with the bounded GUI channel
    /// actually Full (futures capacity includes a sender reserved slot, so
    /// fill until `try_send` fails), the settings wake must stay retained
    /// (never discarded after the mailbox coalesced its notification), the
    /// parked events must survive until the retained wake is delivered, and
    /// quit must flow through the separate control FIFO without waiting for
    /// GUI capacity.
    #[test]
    fn stalled_full_channel_keeps_settings_wake_and_quit_fifo() {
        use application::iced::futures::StreamExt;
        let (mut send, mut receive) = mpsc::channel(64);
        let mut filled = 0;
        while let Ok(()) = send.try_send(Delivery::Changed) {
            filled += 1;
        }
        assert!(filled > 0, "the channel fills to its real capacity");
        let mut outbox = Outbox::default();
        outbox.push(Delivery::Settings);
        assert!(
            !outbox.flush(&mut send),
            "a full GUI channel must not discard the settings wake"
        );
        let mailbox = application::presentation::native::Mailbox::<()>::default();
        assert!(mailbox.publish(SettingsEvent::Wake));
        assert!(!mailbox.publish(SettingsEvent::Wake), "notification coalesced");
        assert!(receive.try_next().unwrap().is_some());
        assert!(
            outbox.flush(&mut send),
            "the retained wake is delivered once capacity returns"
        );
        let parked = mailbox.take();
        assert!(!parked.is_empty(), "parked settings events survive the wake");
        assert!(mailbox.take().is_empty(), "the mailbox drains exactly once");
        let mut settings = 0;
        for _ in 0..=filled {
            match receive.try_next() {
                Ok(Some(Delivery::Settings)) => settings += 1,
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => break,
            }
        }
        assert_eq!(
            settings, 1,
            "exactly one retained wake, delivered behind the queued deliveries"
        );
        let (control, mut controls) = tokio::sync::mpsc::unbounded_channel();
        let handle = Handle {
            control,
            ..Handle::sink()
        };
        handle.forward();
        handle.quit();
        assert!(
            matches!(controls.try_recv(), Ok(Control::Forward)),
            "the handoff travels the reliable control FIFO"
        );
        assert!(
            matches!(controls.try_recv(), Ok(Control::Quit)),
            "quit FIFO never waits on GUI capacity"
        );
    }
    /// The capacity-aware branch itself, using the same actual ready path as
    /// the worker (`poll_ready` behind `poll_fn`): the polled future stays
    /// parked while the channel is full, completes when the receiver alone
    /// frees capacity (no other worker input), and the pinned borrow is
    /// dropped before the sender is reused. Exactly one retained wake is
    /// delivered, behind the queued deliveries.
    #[test]
    fn parked_worker_wake_waits_for_capacity_without_other_input() {
        use application::iced::futures::StreamExt;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (mut send, mut receive) = mpsc::channel(8);
        while let Ok(()) = send.try_send(Delivery::Changed) {}
        let mut outbox = Outbox::default();
        outbox.push(Delivery::Settings);
        assert!(!outbox.flush(&mut send), "the retained wake is parked");
        runtime.block_on(async {
            let ready = std::future::poll_fn(|cx| send.poll_ready(cx));
            tokio::pin!(ready);
            tokio::select! {
                biased;
                _ = tokio::time::sleep(Duration::from_millis(10)) => {
                    // The receiver frees capacity with no other worker input.
                    assert!(receive.try_next().unwrap().is_some());
                    // The same parked future completes after the drain.
                    assert!(ready.as_mut().await.is_ok());
                }
                result = ready.as_mut() => {
                    let _ = result;
                    panic!("a full channel must keep the retained wake parked");
                }
            }
        });
        // The pinned future is dropped before the sender is borrowed again.
        assert!(
            outbox.flush(&mut send),
            "the retained wake flushes once capacity returns"
        );
        let mut settings = 0;
        while let Ok(Some(delivery)) = receive.try_next() {
            if matches!(delivery, Delivery::Settings) {
                settings += 1;
            }
        }
        assert_eq!(
            settings, 1,
            "exactly one retained wake, delivered behind the queue"
        );
    }
    /// Lifecycle edges coalesce to their newest value while one-shot
    /// completions are retained in order: a full channel must not lose a
    /// refusal or a handoff completion.
    #[test]
    fn lifecycle_edges_coalesce_and_handoff_completions_are_retained() {
        use application::iced::futures::StreamExt;
        let (mut send, mut receive) = mpsc::channel(64);
        while let Ok(()) = send.try_send(Delivery::Changed) {}
        let mut outbox = Outbox::default();
        outbox.push(Delivery::Disconnected);
        outbox.push(Delivery::Connected);
        outbox.push(Delivery::Refused {
            name_taken: true,
            message: "already registered".into(),
        });
        // A later terminal edge must not overwrite the typed reason.
        outbox.push(Delivery::Refused {
            name_taken: false,
            message: "connection stopped".into(),
        });
        outbox.push(Delivery::Forwarded(Ok(())));
        assert!(
            !outbox.flush(&mut send),
            "a full channel retains lifecycle and completions"
        );
        assert!(receive.try_next().unwrap().is_some(), "one slot frees");
        assert!(
            !outbox.flush(&mut send),
            "one slot delivers one retained message"
        );
        let mut drained = Vec::new();
        while let Ok(Some(delivery)) = receive.try_next() {
            drained.push(delivery);
            if drained.len() > 256 {
                break;
            }
        }
        assert!(
            matches!(
                drained.last(),
                Some(Delivery::Refused {
                    name_taken: true,
                    message
                }) if message == "already registered"
            ),
            "the typed refusal reaches the GUI after the stall"
        );
        assert!(outbox.flush(&mut send), "remaining retained deliveries flush");
        let mut tail = Vec::new();
        while let Ok(Some(delivery)) = receive.try_next() {
            tail.push(delivery);
        }
        assert_eq!(
            tail,
            vec![Delivery::Forwarded(Ok(())), Delivery::Connected],
            "the completion precedes the newest coalesced edge"
        );
    }
    #[test]
    fn overflow_accounts_settings_loss_before_other_events() {
        let mailbox = application::presentation::native::Mailbox::<()>::default();
        assert!(mailbox.publish(SettingsEvent::Wake));
        assert!(mailbox.publish(SettingsEvent::Lost));
        let events = mailbox.take();
        assert!(
            matches!(events.first(), Some(SettingsEvent::Lost)),
            "Lost is accounted before other settings frames"
        );
    }
    /// One refusal and one handoff completion per lifetime: duplicates keep
    /// the first accepted value and never queue beyond the two slots.
    #[test]
    fn refusal_and_handoff_keep_their_first_value() {
        let mut outbox = Outbox::default();
        outbox.push(Delivery::Refused {
            name_taken: true,
            message: "already registered".into(),
        });
        outbox.push(Delivery::Refused {
            name_taken: false,
            message: "connection stopped".into(),
        });
        outbox.push(Delivery::Forwarded(Ok(())));
        outbox.push(Delivery::Forwarded(Err("late duplicate".into())));
        assert_eq!(
            outbox.next(),
            Some(Delivery::Refused {
                name_taken: true,
                message: "already registered".into(),
            })
        );
        assert_eq!(outbox.next(), Some(Delivery::Forwarded(Ok(()))));
        assert_eq!(outbox.next(), None, "no duplicate slots were queued");
    }
}

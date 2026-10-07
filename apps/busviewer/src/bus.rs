// SPDX-License-Identifier: MIT OR Apache-2.0
//! One supervised native Bus connection plus the shared settings Lane on the
//! same worker, runtime, client and receiver. Topics drive refreshes; no
//! poller and no second transport exists here.
use ::bus::native_client::{
    BoundedIncomingEvent, ConnState, IncomingCommand, NodedClient, RegistrationRejectionKind,
    SupervisedClient, SupervisedError,
};
use application::iced::futures::channel::{mpsc, oneshot};
use application::message::Once;
use application::native_actor::{
    Accepted, Completed, Faults, Reply as NativeReply, TaskSet, cancel, reap, submit_replies,
};
use application::native_queue::{Admission, Flush, Outbox as Queue, Permit, SendError};
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
    Command {
        id: Request,
        verb: String,
        body: String,
    },
    Changed,
    Settings,
    Notice(String),
    Refused {
        name_taken: bool,
        message: String,
    },
    Forwarded(Result<(), String>),
    Connected,
    Disconnected,
}
#[derive(Debug, Clone)]
pub struct Reply {
    pub rc: u8,
    pub body: String,
}

/// Clones identify one accepted delivery and can enqueue its answer only once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub id: u64,
    ticket: Once<u64>,
    generation: u64,
}
impl Request {
    fn new(id: u64, generation: u64) -> Self {
        Self {
            id,
            ticket: Once::new(id),
            generation,
        }
    }
}
#[cfg(test)]
impl From<u64> for Request {
    fn from(id: u64) -> Self {
        Self::new(id, 0)
    }
}
#[cfg(test)]
impl From<i32> for Request {
    fn from(id: i32) -> Self {
        Self::new(u64::try_from(id).expect("nonnegative fixture id"), 0)
    }
}
enum Effect {
    Call(
        String,
        String,
        String,
        oneshot::Sender<Result<Reply, CallError>>,
        Permit,
        std::time::Instant,
        Option<u64>,
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
    forward_once: Once<()>,
    quit_once: Once<()>,
    outgoing: Admission,
    #[cfg(test)]
    records: Arc<Mutex<Vec<(u64, u8, Value)>>>,
    #[cfg(test)]
    stopped: Arc<std::sync::atomic::AtomicBool>,
    #[cfg(test)]
    forwards: Arc<std::sync::atomic::AtomicUsize>,
    #[cfg(test)]
    test_generation: Option<u64>,
}
impl Handle {
    pub fn service_name<'a>(&'a self, fallback: &'a str) -> &'a str {
        self.client.as_ref().map_or(fallback, |client| client.service_name())
    }
    pub async fn raw(&self, service: &str, verb: &str, body: String) -> Result<Reply, CallError> {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let permit = self
            .outgoing
            .try_acquire()
            .ok_or_else(|| CallError::not_sent("Bus call admission exhausted; no call sent"))?;
        let generation = self.settings_generation();
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Effect::Call(
                service.into(),
                verb.into(),
                body,
                tx,
                permit,
                deadline,
                generation,
            ))
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
    pub fn reply(&self, request: impl Into<Request>, rc: u8, body: Value) {
        let Some(id) = request.into().ticket.take() else {
            return;
        };
        #[cfg(test)]
        self.records.lock().unwrap().push((id, rc, body.clone()));
        // Only the GUI sends replies, once per accepted command (at most 32).
        // This separate queue cannot lose a reply to outgoing call backpressure.
        // A closed receiver means the native connection has already ended.
        let _ = self.control.send(Control::Reply(id, rc, body));
    }
    pub fn forward(&self) {
        if self.forward_once.take().is_none() {
            return;
        }
        #[cfg(test)]
        self.forwards
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        // The handoff travels the owned unbounded control FIFO, so a full
        // effect or GUI channel can never lose it or leave the UI pending.
        let _ = self.control.send(Control::Forward);
    }
    pub fn quit(&self) {
        if self.quit_once.take().is_none() {
            return;
        }
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
    pub fn is_current(&self, request: &Request) -> bool {
        self.client.as_ref().is_none_or(|client| {
            settings::native::live_generation(client) == Some(request.generation)
        })
    }
    pub fn settings_generation(&self) -> Option<u64> {
        #[cfg(test)]
        if self.client.is_none() {
            return self.test_generation;
        }
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
            forward_once: Once::new(()),
            quit_once: Once::new(()),
            outgoing: Admission::new(64),
            records: Arc::new(Mutex::new(Vec::new())),
            stopped: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            forwards: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            test_generation: None,
        }
    }
    /// A sink whose sampled generation is explicit, for settings drains that
    /// fence on the live connection generation in tests.
    #[cfg(test)]
    pub fn sink_with_generation(generation: Option<u64>) -> Self {
        let mut sink = Self::sink();
        sink.test_generation = generation;
        sink
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
    start_inner(
        service,
        url,
        64,
        #[cfg(test)]
        None,
    )
}

#[cfg(test)]
#[derive(Clone, Debug, Default)]
struct ActorProbe {
    generation: u64,
    connected: bool,
    pending: usize,
    active: usize,
    reliable: usize,
    replies: usize,
    reply_tasks: usize,
    invariant_faults: usize,
}

fn start_inner(
    service: &str,
    url: &str,
    gui_capacity: usize,
    #[cfg(test)] probe: Option<tokio::sync::watch::Sender<ActorProbe>>,
) -> Result<
    (
        Handle,
        Ui<()>,
        appearance::settings::Prepared,
        mpsc::Receiver<Delivery>,
    ),
    String,
> {
    let (send, receive) = mpsc::channel(gui_capacity);
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
                    runtime.block_on(worker(
                        service,
                        url,
                        send,
                        rx,
                        controls,
                        ready_send,
                        #[cfg(test)]
                        probe,
                    ));
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
            forward_once: Once::new(()),
            quit_once: Once::new(()),
            outgoing: Admission::new(64),
            #[cfg(test)]
            records: Arc::new(Mutex::new(Vec::new())),
            #[cfg(test)]
            stopped: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            #[cfg(test)]
            forwards: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            #[cfg(test)]
            test_generation: None,
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

/// Product classification only; shared FIFO owns all retained delivery.
struct Outbox {
    queue: Queue<Delivery, 4>,
    refused: bool,
    forwarded: bool,
}
impl Default for Outbox {
    fn default() -> Self {
        Self {
            queue: Queue::new(34),
            refused: false,
            forwarded: false,
        }
    }
}
impl Outbox {
    fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
    fn retire_commands(&mut self, keep: impl Fn(&Request) -> bool) {
        self.queue.retain(|delivery| match delivery {
            Delivery::Command { id, .. } => keep(id),
            _ => true,
        });
    }
    fn push(&mut self, delivery: Delivery) -> bool {
        let slot = match &delivery {
            Delivery::Connected | Delivery::Disconnected => Some(0),
            Delivery::Notice(_) => Some(1),
            Delivery::Changed => Some(2),
            Delivery::Settings => Some(3),
            Delivery::Refused { .. } => {
                if self.refused {
                    return true;
                }
                self.refused = true;
                None
            }
            Delivery::Forwarded(_) => {
                if self.forwarded {
                    return true;
                }
                self.forwarded = true;
                None
            }
            Delivery::Command { .. } => None,
        };
        if let Some(slot) = slot {
            self.queue.replace(slot, delivery).is_ok()
        } else {
            self.queue.push(delivery).is_ok()
        }
    }
    fn flush(&mut self, send: &mut mpsc::Sender<Delivery>) -> bool {
        self.queue.flush_with(|delivery| {
            send.try_send(delivery).map_err(|error| {
                if error.is_full() {
                    SendError::Full(error.into_inner())
                } else {
                    SendError::Closed(error.into_inner())
                }
            })
        }) == Flush::Empty
    }
    #[cfg(test)]
    fn next(&mut self) -> Option<Delivery> {
        let mut first = None;
        self.queue.flush_with(|delivery| {
            if first.is_some() {
                Err(SendError::Full(delivery))
            } else {
                first = Some(delivery);
                Ok(())
            }
        });
        first
    }
}

fn reply_refused(
    client: &Arc<SupervisedClient>,
    replies: &mut TaskSet<Result<(), String>>,
    admission: &Admission,
    command: IncomingCommand,
    body: String,
) {
    if replies.is_full() {
        eprintln!("busviewer: refusal shed under load; task cap occupied");
        return;
    }
    let Some(permit) = admission.try_acquire() else {
        eprintln!("busviewer: refusal shed under load; admission occupied");
        return;
    };
    let now = std::time::Instant::now();
    let reply = Accepted::new(client.clone(), command, permit, now).reply(
        10,
        body,
        now + Duration::from_secs(2),
    );
    if let Err(reply) = replies.try_spawn_with(reply, NativeReply::into_task) {
        reply.retire().finish();
        eprintln!("busviewer: refusal task capacity invariant failed");
    }
}

async fn worker(
    service: String,
    url: String,
    mut send: mpsc::Sender<Delivery>,
    mut effects: tokio::sync::mpsc::Receiver<Effect>,
    mut controls: tokio::sync::mpsc::UnboundedReceiver<Control>,
    ready: Ready,
    #[cfg(test)] probe: Option<tokio::sync::watch::Sender<ActorProbe>>,
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
            .with_initial_topics(vec!["noded.props.changed".into()])
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
        if tokio::time::timeout(Duration::from_secs(2), client.close())
            .await
            .is_err()
        {
            eprintln!("busviewer: startup-abandon close exceeded deadline");
        }
        return;
    }
    // Font files are installed on the existing worker, after UI readiness and
    // before the Lane may prepare a checked presentation.
    if let Err(error) = appearance::fonts::register_installed() {
        eprintln!("busviewer: static assets: {error}");
    }
    let mut pending: HashMap<u64, Accepted> = HashMap::new();
    let mut accepted = Queue::<NativeReply, 0>::new(32);
    let mut next_id = 0u64;
    let admission = Admission::new(32);
    let refusal_admission = Admission::new(4);
    let forward_admission = Admission::new(1);
    let mut faults = Faults::default();
    let mut incoming_open = true;
    let mut connection_open = true;
    let mut forward_gate = false;
    let mut calls = TaskSet::<()>::new(16);
    let mut forwards = TaskSet::<Result<(), String>>::new(1);
    let mut replies = TaskSet::<Result<(), String>>::new(8);
    let mut refusals = TaskSet::<Result<(), String>>::new(4);
    let mut lifecycle = None;
    loop {
        // Fast path: flush retained deliveries without blocking the Lane.
        outbox.flush(&mut send);
        // Admit retained accepted replies as running slots free up; the
        // queue stays bounded because admission below pauses while it is
        // non-empty.
        submit_replies(&mut accepted, &mut replies);
        // Sample once before waiting: fast registration may already have
        // completed, and every later edge wakes this loop again.
        let now = *connection.borrow_and_update();
        let generation = client.connection_generation();
        if lifecycle != Some((now, generation)) {
            lifecycle = Some((now, generation));
            let stale: Vec<_> = pending
                .iter()
                .filter_map(|(id, entry)| (!entry.is_current(&client)).then_some(*id))
                .collect();
            for id in stale {
                if let Some(entry) = pending.remove(&id) {
                    faults.push(format!("accepted command {id} retired on lifecycle change"));
                    entry.retire().finish();
                }
            }
            // Lifecycle retirement must release retained queue space before
            // new-generation admission. Deliveries already handed to the
            // bounded GUI channel are fenced again by App::command.
            outbox.retire_commands(|request| pending.contains_key(&request.id));
            match now {
                ConnState::Connected => {
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
                    // Only a typed name-taken collision may ever hand off;
                    // unknown or admission refusals never forward or exit.
                    outbox.push(Delivery::Refused {
                        name_taken: client.subscription_declaration_error().is_none()
                            && reason.as_ref().is_some_and(|reason| {
                                reason.kind() == RegistrationRejectionKind::NameTaken
                            }),
                        message: client.subscription_declaration_error().map_or_else(
                            || {
                                reason.map_or_else(
                                    || "connection stopped".into(),
                                    |reason| reason.message,
                                )
                            },
                            |error| format!("Subscription failed: {error}"),
                        ),
                    });
                }
                ConnState::Disconnected => {
                    if lane.publish(SettingsEvent::Wake) {
                        outbox.push(Delivery::Settings);
                    }
                    outbox.push(Delivery::Disconnected);
                }
                ConnState::Connecting => {}
            }
        }
        #[cfg(test)]
        if let Some(probe) = &probe {
            probe.send_replace(ActorProbe {
                generation,
                connected: settings::native::live_generation(&client).is_some(),
                pending: pending.len(),
                active: admission.counts().active,
                reliable: outbox.queue.reliable_len(),
                replies: accepted.len(),
                reply_tasks: replies.len(),
                invariant_faults: faults
                    .recent()
                    .iter()
                    .filter(|error| error.contains("invariant"))
                    .count(),
            });
        }
        tokio::select! {
            control = controls.recv() => {
                match control {
                    Some(Control::Reply(id, rc, value)) => {
                        if let Some(entry) = pending.remove(&id) {
                            // Accepted replies are retained FIFO, never
                            // dropped; spawning waits for a running-task
                            // slot so a stalled broker cannot grow tasks
                            // without bound.
                            let reply = entry.reply(rc,value.to_string(),std::time::Instant::now()+Duration::from_secs(2));
                            if let Err(reply) = accepted.push(reply) {
                                faults.push("accepted reply retention invariant failed".into());
                                reply.retire().finish();
                            }
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
                            let permit = forward_admission.try_acquire().expect("one lifetime handoff");
                            if let Err(permit) = forwards.try_spawn_with(permit, |permit| (permit,async move {
                                forward_async(&url, &service).await
                            })) {
                                permit.finish();
                                faults.push("forward task capacity invariant failed".into());
                                outbox.push(Delivery::Forwarded(Err("handoff capacity exhausted".into())));
                            }
                        }
                    }
                    Some(Control::Quit) | None => break,
                }
            }
            // Random select fairness gives ready GUI calls, settings and
            // completions opportunities during sustained inbound traffic.
            // Each call keeps the absolute budget captured by its producer.
            effect = effects.recv() => {
                let Some(effect) = effect else { break; };
                match effect {
                    Effect::Call(service, verb, body, reply, permit, deadline, generation) => {
                        if calls.is_full() {
                            let _ = reply.send(Err(CallError::not_sent("Bus call capacity exhausted; no call sent")));
                            permit.finish();
                            continue;
                        }
                        let client = client.clone();
                        // Owned and drained like every other operation; the
                        // permit and outcome semantics are unchanged.
                        let result = calls.try_spawn_with(permit, |permit| (permit, async move {
                            if reply.is_canceled() || std::time::Instant::now() >= deadline || generation.is_none()
                                || settings::native::live_generation(&client) != generation {
                                let _ = reply.send(Err(CallError::not_sent("queued Bus call retired or expired; no call sent")));
                                return;
                            }
                            let result = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), client.call_with_headers_raw_at_generation(generation.expect("live generation checked"), &service, &verb, &BTreeMap::new(), &body)).await
                                .map_err(|_| CallError::from("Bus request timed out"))
                                .and_then(|v| v.map_err(CallError::transport))
                                .map(|(rc, body, _)| Reply { rc, body });
                            let _ = reply.send(result);
                        }));
                        if let Err(permit) = result {
                            permit.finish();
                            faults.push("outgoing task capacity invariant failed".into());
                        }
                    }
                }
            }
            progress = lane.drive() => {
                match progress {
                    Progress::Wake => { outbox.push(Delivery::Settings); },
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
                if let Some(result) = result {
                    reap("Bus call",result,&mut faults,|(),_|{});
                }
            }
            result = forwards.join_next(), if !forwards.is_empty() => {
                match result {
                    Some(Ok(Completed {permit,value})) => {
                        outbox.push(Delivery::Forwarded(value));
                        permit.finish();
                    },
                    Some(Err(error)) => {
                        eprintln!("busviewer: Bus work: {error}");
                        faults.push(format!("Bus work: {error}"));
                    }
                    None => {}
                }
            }
            result = replies.join_next(), if !replies.is_empty() => {
                if let Some(result) = result {
                    reap("Bus reply",result,&mut faults,|value,faults| {if let Err(error)=value { faults.push(error); }});
                }
            }
            result = refusals.join_next(), if !refusals.is_empty() => {
                if let Some(result) = result {
                    reap("Bus refusal",result,&mut faults,|value,faults| {if let Err(error)=value { faults.push(error); }});
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
                    // Settings frames the Lane did not swallow belong to
                    // another profile; they are not bus inventory changes.
                    if command.header("broker_service") == Some("settingsd") {
                        continue;
                    }
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
                    reply_refused(&client, &mut refusals, &refusal_admission, command,
                        "{\"error_code\":\"ARGUMENT\",\"message\":\"command verb is empty\"}".into());
                    continue;
                }
                if settings::native::live_generation(&client) != Some(command.generation) {
                    faults.push("stale queued command retired before frontend admission".into());
                    continue;
                }
                if command.command == application::describe::VERB
                    && let Err(violation) = application::describe::validate_request(&command.body) {
                    reply_refused(&client, &mut refusals, &refusal_admission, command,
                        crate::model::describe_refusal(&violation).to_string());
                    continue;
                }
                let Some(permit) = admission.try_acquire() else {
                    reply_refused(&client, &mut refusals, &refusal_admission, command,
                        "{\"error_code\":\"BUSY\",\"message\":\"too many pending commands or accepted replies still flushing\"}".into());
                    continue;
                };
                let Some(id) = next_id.checked_add(1) else {
                    permit.finish();
                    reply_refused(&client,&mut refusals,&refusal_admission,command,"{\"error_code\":\"EXHAUSTED\"}".into());
                    continue;
                };
                next_id = id;
                let delivery = Delivery::Command {
                    id: Request::new(next_id, command.generation),
                    verb: command.command.clone(),
                    body: if command.body.trim().is_empty() { "{}".into() } else { command.body.clone() },
                };
                pending.insert(next_id, Accepted::new(client.clone(),command,permit,std::time::Instant::now()));
                if !outbox.push(delivery) {
                    pending.remove(&next_id).expect("just inserted").retire().finish();
                    faults.push("frontend reliable retention invariant failed".into());
                    break;
                }
            }
            changed = connection.changed(), if connection_open => {
                if changed.is_err() {
                    connection_open = false;
                }
            }
        }
    }
    // Freeze producers and close queued unsent calls without replay. All
    // drains share one deadline; runtime teardown has a separate 100 ms cap.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    effects.close();
    while let Ok(Effect::Call(_, _, _, reply, permit, _, _)) = effects.try_recv() {
        let _ = reply.send(Err(CallError::not_sent(
            "Bus shutdown; queued call not sent",
        )));
        permit.finish();
    }
    for (_, entry) in pending.drain() {
        faults.push("accepted command retired without frontend answer".into());
        entry.retire().finish();
    }
    calls.abort_all();
    forwards.abort_all();
    while !replies.is_empty() || !accepted.is_empty() {
        if std::time::Instant::now() >= deadline {
            faults.push("Bus reply drain timed out; delivery unconfirmed".into());
            break;
        }
        submit_replies(&mut accepted, &mut replies);
        match tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            replies.join_next(),
        )
        .await
        {
            Ok(Some(result)) => reap("Bus reply", result, &mut faults, |value, faults| {
                if let Err(error) = value {
                    faults.push(error);
                }
            }),
            Ok(None) => break,
            Err(_) => {
                faults.push("Bus reply drain timed out; delivery unconfirmed".into());
                break;
            }
        }
    }
    let unsent = accepted.len();
    for reply in accepted.drain() {
        reply.retire().finish();
    }
    if unsent > 0 {
        faults.push(format!("{unsent} retained replies retired unsent"));
    }
    cancel("Bus reply", replies, &mut faults, |value, faults| {
        if let Err(error) = value {
            faults.push(error);
        }
    });
    cancel("Bus call", calls, &mut faults, |(), _| {});
    cancel("Bus forward", forwards, &mut faults, |value, faults| {
        if let Err(error) = value {
            faults.push(error);
        }
    });
    while !refusals.is_empty() && std::time::Instant::now() < deadline {
        match tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            refusals.join_next(),
        )
        .await
        {
            Ok(Some(result)) => reap("Bus refusal", result, &mut faults, |value, faults| {
                if let Err(error) = value {
                    faults.push(error);
                }
            }),
            Ok(None) | Err(_) => break,
        }
    }
    cancel("Bus refusal", refusals, &mut faults, |value, faults| {
        if let Err(error) = value {
            faults.push(error);
        }
    });
    if let Err(error) = lane.flush_cache(deadline).await {
        faults.push(format!("settings cache: {}: {}", error.code, error.message));
    }
    // Even at expiry, the first poll requests supervisor stop. Completion
    // receives no fresh drain budget and is reported separately if uncertain.
    if tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), client.close())
        .await
        .is_err()
    {
        faults.push("Bus close timed out; stop requested, completion unconfirmed".into());
    }
    let _ = send.try_send(Delivery::Disconnected);
    let accepted = admission.counts();
    let refused = refusal_admission.counts();
    eprintln!(
        "BUSVIEWER_SHUTDOWN {}",
        json!({"faults":faults,"retained_unsent":unsent,
        "accepted":{"active":accepted.active,"finished":accepted.finished,"abandoned":accepted.abandoned},
        "refusals":{"active":refused.active,"finished":refused.finished,"abandoned":refused.abandoned}})
    );
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

/// A canonical authority snapshot fixture for settings pipeline tests: the
/// consumer's own binding, the embedded design source, and an explicit
/// appearance/text-scale selection.
#[cfg(test)]
pub(crate) fn settings_snapshot(
    binding: &settings::Binding,
    dark: bool,
    text_scale: f64,
) -> settings::Snapshot {
    let mut desktop = settings::Desktop::default();
    if dark {
        desktop.appearance.mode = "dark".into();
    }
    desktop.ui.text_scale = text_scale;
    settings::Snapshot {
        schema: 1,
        binding: binding.clone(),
        incarnation: "fixture".into(),
        revision: settings::Revision(2),
        design_revision: settings::Revision(2),
        source_digest: settings::source_digest(settings::EMBEDDED_DEFAULT_SOURCE),
        effective: settings::resolve(&desktop).expect("fixture desktop resolves"),
        desktop,
    }
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
                        reply,
                        handle.outgoing.try_acquire().unwrap(),
                        std::time::Instant::now() + Duration::from_secs(30),
                        Some(1)
                    ))
                    .is_ok()
            );
        }
        let request = Request::new(42, 1);
        handle.reply(request.clone(), 0, json!({"ok":true}));
        handle.reply(request, 0, json!({"duplicate":true}));
        handle.quit();
        handle.quit();
        assert!(matches!(controls.try_recv(), Ok(Control::Reply(42, 0, _))));
        assert!(matches!(controls.try_recv(), Ok(Control::Quit)));
        assert!(
            controls.try_recv().is_err(),
            "cloned replies and repeated quit do not enqueue twice"
        );
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
        assert!(
            handle.connected(),
            "a sink without a client reads as connected"
        );
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
        assert!(
            !mailbox.publish(SettingsEvent::Wake),
            "notification coalesced"
        );
        assert_eq!(
            receive.try_recv(),
            Ok(Delivery::Changed),
            "the queued delivery is the first real dequeue"
        );
        assert!(
            outbox.flush(&mut send),
            "the retained wake is delivered once capacity returns"
        );
        let parked = mailbox.take();
        assert!(
            !parked.is_empty(),
            "parked settings events survive the wake"
        );
        assert!(mailbox.take().is_empty(), "the mailbox drains exactly once");
        let mut settings = 0;
        for _ in 0..=filled {
            match receive.try_recv() {
                Ok(Delivery::Settings) => settings += 1,
                Ok(_) => {}
                Err(_) => break,
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
                    assert_eq!(receive.try_recv(), Ok(Delivery::Changed));
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
        while let Ok(delivery) = receive.try_recv() {
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
        assert!(receive.try_recv().is_ok(), "one slot frees");
        assert!(
            !outbox.flush(&mut send),
            "one slot delivers one retained message"
        );
        let mut drained = Vec::new();
        while let Ok(delivery) = receive.try_recv() {
            drained.push(delivery);
            if drained.len() > 256 {
                break;
            }
        }
        assert!(
            matches!(drained.last(), Some(Delivery::Connected)),
            "the newest coalesced edge retains its first FIFO position"
        );
        assert!(
            outbox.flush(&mut send),
            "remaining retained deliveries flush"
        );
        let mut tail = Vec::new();
        while let Ok(delivery) = receive.try_recv() {
            tail.push(delivery);
        }
        assert_eq!(
            tail,
            vec![
                Delivery::Refused {
                    name_taken: true,
                    message: "already registered".into()
                },
                Delivery::Forwarded(Ok(()))
            ],
            "the typed refusal precedes its reliable handoff completion"
        );
    }
    /// Overflow loss through the real session: a queued Lost coalesces with
    /// the outstanding notification, and the drain fences the confirmed
    /// authority readback and restages the bound read.
    #[test]
    fn overflow_loss_fences_confirmed_readback_through_a_real_drain() {
        let binding = settings::Binding {
            instance: "fixture".into(),
            profile: "default".into(),
        };
        let consumer = settings::consumer::Consumer::for_app(binding.clone(), "busviewer").unwrap();
        let (mut ui, lane) = bridge(Session::new(consumer), Worker::offline(|_, _| Ok(())));
        // Establish a confirmed authority read through the real consumer.
        let _ = ui.handle_with(SettingsEvent::Wake, Some(1), |_| {});
        let subscribe = ui
            .session()
            .host()
            .consumer()
            .current_work()
            .expect("subscribe work")
            .clone();
        let _ = ui.handle_with(SettingsEvent::Rpc(subscribe, Ok(None)), Some(1), |_| {});
        let _ = ui.handle_with(SettingsEvent::Wake, Some(1), |_| {});
        let read = ui
            .session()
            .host()
            .consumer()
            .current_work()
            .expect("read work")
            .clone();
        let _ = ui.handle_with(
            SettingsEvent::Rpc(read, Ok(Some(settings_snapshot(&binding, false, 1.0)))),
            Some(1),
            |_| {},
        );
        assert!(
            ui.session().host().consumer().is_confirmed(),
            "authority read confirmed"
        );
        // The wake is the outstanding notification; the loss coalesces into
        // it and is applied by the same drain.
        assert!(lane.publish(SettingsEvent::Wake));
        assert!(
            !lane.publish(SettingsEvent::Lost),
            "loss coalesces with the outstanding notification"
        );
        ui.drain_with(|| Some(1), |_| {});
        assert!(
            !ui.session().host().consumer().is_confirmed(),
            "Lost fences the confirmed readback"
        );
        assert!(
            ui.session().host().consumer().current_work().is_some(),
            "Lost restages a bound read"
        );
    }
    #[test]
    fn cutoff_reports_unconfirmed_operations_without_waiting() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let admission = Admission::new(1);
            let mut calls = TaskSet::new(1);
            let (entered, started) = tokio::sync::oneshot::channel();
            let (mut release, held) = tokio::sync::oneshot::channel::<()>();
            assert!(
                calls
                    .try_spawn_with(admission.try_acquire().unwrap(), |permit| (
                        permit,
                        async move {
                            entered.send(()).unwrap();
                            let _ = held.await;
                        }
                    ))
                    .is_ok()
            );
            started.await.unwrap();
            let mut faults = Faults::default();
            cancel("Bus call", calls, &mut faults, |(), _| {});
            assert_eq!(faults.count(), 1);
            assert!(faults.recent()[0].contains("unconfirmed"));
            assert_eq!(admission.counts().active, 1);
            release.closed().await;
            assert_eq!(admission.counts().abandoned, 1);
            assert_eq!(admission.counts().finished, 0);
        });
    }

    #[test]
    fn inventory_flood_cannot_move_an_existing_settings_wake_backwards() {
        let mut outbox = Outbox::default();
        outbox.push(Delivery::Changed);
        outbox.push(Delivery::Settings);
        for _ in 0..1024 {
            outbox.push(Delivery::Changed);
            outbox.push(Delivery::Settings);
        }
        assert_eq!(outbox.queue.len(), 2);
        assert_eq!(outbox.next(), Some(Delivery::Changed));
        outbox.push(Delivery::Changed);
        assert_eq!(outbox.next(), Some(Delivery::Settings));
        assert_eq!(outbox.next(), Some(Delivery::Changed));
        assert!(outbox.is_empty());
    }

    #[test]
    fn reconnect_retires_queued_commands_before_reusing_reliable_capacity() {
        let mut outbox = Outbox::default();
        outbox.push(Delivery::Settings);
        for id in 0..32 {
            assert!(outbox.push(Delivery::Command {
                id: Request::new(id, 1),
                verb: "old".into(),
                body: String::new()
            }));
        }
        outbox.push(Delivery::Changed);
        outbox.retire_commands(|request| request.generation == 2);
        assert_eq!(outbox.queue.reliable_len(), 0);
        for id in 32..64 {
            assert!(outbox.push(Delivery::Command {
                id: Request::new(id, 2),
                verb: "new".into(),
                body: String::new()
            }));
        }
        // Coalesced wakes keep their positions ahead of replacement work.
        assert_eq!(outbox.next(), Some(Delivery::Settings));
        assert_eq!(outbox.next(), Some(Delivery::Changed));
        for id in 32..64 {
            assert!(
                matches!(outbox.next(),Some(Delivery::Command {id: request,..}) if request.id==id && request.generation==2)
            );
        }
        assert!(outbox.is_empty());
    }

    async fn observed(
        probe: &mut tokio::sync::watch::Receiver<ActorProbe>,
        predicate: impl Fn(&ActorProbe) -> bool,
    ) -> ActorProbe {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let state = probe.borrow_and_update().clone();
                assert_eq!(state.invariant_faults, 0, "actor invariant: {state:?}");
                if predicate(&state) {
                    return state;
                }
                probe
                    .changed()
                    .await
                    .expect("actual worker exited before checkpoint");
            }
        })
        .await
        .expect("real actor checkpoint deadline")
    }

    /// The broker and all calls are real production noded/ABP. Only GUI
    /// capacity and read-only watch snapshots are test configuration.
    fn native_reconnect_with_stalled_gui(gui_capacity: usize) {
        use application::iced::futures::StreamExt;
        let mut broker = term_test_broker::Broker::start_stable();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (probe, mut observation) = tokio::sync::watch::channel(ActorProbe::default());
            let (handle, _ui, _bootstrap, mut events) =
                start_inner("actor-viewer", &broker.url, gui_capacity, Some(probe)).unwrap();
            struct Stop(Handle);
            impl Drop for Stop {
                fn drop(&mut self) {
                    self.0.quit();
                    let _ = self.0.wait_done();
                }
            }
            let _stop = Stop(handle.clone());
            let mut state = observed(&mut observation, |state| state.connected).await;
            // The GUI remains paused. Invalid raw descriptions must be
            // answered by the actual actor before ordinary admission.
            let caller = NodedClient::connect_anonymous(&broker.url).await.unwrap();
            let oversized = " ".repeat(application::describe::MAX_REQUEST_BYTES + 1);
            for body in ["{", "[]", "null", r#"{"x":1}"#, oversized.as_str()] {
                let (rc, reply) = tokio::time::timeout(Duration::from_secs(5), caller.call_with_headers_raw("actor-viewer", "app.describe", &BTreeMap::new(), body)).await.unwrap().unwrap();
                assert_eq!(rc, 10);
                let refusal: Value = serde_json::from_str(&reply).unwrap();
                assert_eq!(refusal["error_code"], "ARGUMENT");
                assert!(refusal["describe_code"].is_string());
            }
            caller.close().await;
            assert_eq!(observation.borrow().pending, 0);
            let mut final_calls = None;
            let mut final_caller = None;
            // With production64, several stale generations first occupy the
            // GUI channel. Admission32 alone cannot fill it in one generation.
            for batch in 0..5 {
                let caller = Arc::new(NodedClient::connect_anonymous(&broker.url).await.unwrap());
                let mut calls = tokio::task::JoinSet::new();
                for sequence in 0..32 {
                    let caller = caller.clone();
                    calls.spawn(async move {
                        let body = json!({"batch":batch,"sequence":sequence}).to_string();
                        tokio::time::timeout(
                            Duration::from_secs(20),
                            caller.call_with_headers_raw(
                                "actor-viewer",
                                "busviewer.ping",
                                &BTreeMap::new(),
                                &body,
                            ),
                        )
                        .await
                        .unwrap()
                    });
                }
                let generation = state.generation;
                state = observed(&mut observation, |state| {
                    state.generation == generation && state.pending == 32
                })
                .await;
                assert_eq!(state.active, 32);
                if final_calls.is_some() {
                    unreachable!("only one final batch");
                }
                // Once a retained old generation really exists, bounce and
                // admit the decisive replacement32 with the GUI still stalled.
                if state.reliable > 2 {
                    broker.bounce();
                    state = observed(&mut observation, |state| {
                        state.connected
                            && state.generation > generation
                            && state.pending == 0
                            && state.reliable == 0
                    })
                    .await;
                    while let Some(result) = calls.join_next().await {
                        assert!(
                            result.unwrap().is_err(),
                            "lost old call must not be acknowledged"
                        );
                    }
                    caller.close().await;
                    let caller =
                        Arc::new(NodedClient::connect_anonymous(&broker.url).await.unwrap());
                    let mut calls = tokio::task::JoinSet::new();
                    for sequence in 0..32 {
                        let caller = caller.clone();
                        calls.spawn(async move {
                            let body = json!({"batch":"new","sequence":sequence}).to_string();
                            let response = tokio::time::timeout(
                                Duration::from_secs(20),
                                caller.call_with_headers_raw(
                                    "actor-viewer",
                                    "busviewer.ping",
                                    &BTreeMap::new(),
                                    &body,
                                ),
                            )
                            .await
                            .unwrap()
                            .unwrap();
                            assert_eq!(response.0, 0);
                            assert_eq!(response.1, body);
                        });
                    }
                    let generation = state.generation;
                    state = observed(&mut observation, |state| {
                        state.generation == generation && state.pending == 32 && state.reliable > 2
                    })
                    .await;
                    assert_eq!(state.active, 32);
                    final_calls = Some(calls);
                    final_caller = Some(caller);
                    break;
                }
                broker.bounce();
                state = observed(&mut observation, |state| {
                    state.connected
                        && state.generation > generation
                        && state.pending == 0
                        && state.reliable == 0
                })
                .await;
                while let Some(result) = calls.join_next().await {
                    assert!(result.unwrap().is_err());
                }
                caller.close().await;
            }
            let mut calls =
                final_calls.expect("default GUI channel must actually reach backpressure");
            let mut sequences = std::collections::BTreeSet::new();
            tokio::time::timeout(Duration::from_secs(15), async {
                while sequences.len() < 32 {
                    if let Delivery::Command { id, body, .. } =
                        events.next().await.expect("worker closed during GUI drain")
                    {
                        if !handle.is_current(&id) {
                            continue;
                        }
                        let body: Value = serde_json::from_str(&body).unwrap();
                        assert_eq!(body["batch"], "new");
                        assert!(sequences.insert(body["sequence"].as_u64().unwrap()));
                        handle.reply(id.clone(), 0, body);
                        handle.reply(id, 10, json!({"duplicate":true}));
                    }
                }
            })
            .await
            .unwrap();
            while let Some(result) = calls.join_next().await {
                result.unwrap();
            }
            observed(&mut observation, |state| {
                state.pending == 0
                    && state.active == 0
                    && state.replies == 0
                    && state.reply_tasks == 0
            })
            .await;
            let caller = final_caller.unwrap();
            let calling = caller.clone();
            let quit = tokio::spawn(async move {
                calling
                    .call_with_headers_raw("actor-viewer", "busviewer.quit", &BTreeMap::new(), "{}")
                    .await
            });
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if let Delivery::Command { id, verb, .. } = events
                        .next()
                        .await
                        .expect("worker closed before quit command")
                    {
                        assert_eq!(verb, "busviewer.quit");
                        assert!(handle.is_current(&id));
                        handle.reply(id, 0, json!({"quitting":true}));
                        handle.quit();
                        break;
                    }
                }
            })
            .await
            .unwrap();
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(5), quit)
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap()
                    .0,
                0
            );
            handle.wait_done().unwrap();
            caller.close().await;
        });
    }

    #[test]
    fn real_broker_reconnect_retires_commands_with_tight_gui_capacity() {
        native_reconnect_with_stalled_gui(1);
    }

    #[test]
    fn real_broker_reconnect_retires_commands_with_production_gui_capacity() {
        native_reconnect_with_stalled_gui(64);
    }

    #[test]
    fn repeated_failures_bound_diagnostic_count_storage_and_utf8_bytes() {
        let mut faults = Faults::default();
        for _ in 0..1024 {
            faults.push("é".repeat(5000));
        }
        assert_eq!(faults.count(), 1024);
        assert_eq!(faults.recent().len(), 32);
        assert!(faults.recent().iter().all(|message| message.len() <= 4096));
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

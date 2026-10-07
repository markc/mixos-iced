// SPDX-License-Identifier: MIT OR Apache-2.0
//! One supervised native Bus connection on its own bounded runtime. Startup is
//! nonblocking (`options.start`): the settings lane and the app stream are
//! ready before registration settles. Settings frames decode before ordinary
//! traffic; channel overflow and lifecycle loss are explicit. An explicit
//! typed name collision hands the launch paths to the running instance on this
//! same worker — never from reply text, and only when no connection was ever
//! established.
//!
//! The GUI outbox retains accepted commands without blocking the worker;
//! settings wakes, lifecycle edges and refresh nudges coalesce in place. Accepted
//! replies travel through one serial tracked task with a fixed admission sum
//! (pending + queued + running + completed but unreaped), so replies are retained;
//! refusals use a separate capped pool with diagnosed shedding. Quit drains
//! the accepted writer, refusals, capture restoration calls, the settings
//! cache and the client close against one shared deadline.
use ::bus::native_client::{
    BoundedIncomingEvent, ConnState, IncomingCommand, NodedClient, RegistrationRejectionKind,
    SupervisedClient,
};
use application::iced::futures::channel::{mpsc, oneshot};
use application::message::Once;
use application::native_actor::{
    Accepted, Completed, Faults, Reply as NativeReply, TaskSet, cancel, reap, submit_replies,
};
use application::native_queue::{Admission, Flush, Outbox, Permit, SendError};
use application::presentation::native::{
    Event as SettingsEvent, Progress, Session, Ui, Worker, bridge,
};

mod actor;
use actor::worker;
#[cfg(test)]
mod actor_tests;
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
    pub id: Request,
    pub verb: String,
    pub body: String,
    /// `local:<from>` / `mesh:<service>@<peer>` / `anon` (editd E0 §4.3).
    pub caller_key: String,
}

/// Clones share one reply token and the receiving connection generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub id: u64,
    generation: u64,
    ticket: Once<u64>,
}
impl Request {
    fn new(id: u64, generation: u64) -> Self {
        Self {
            id,
            generation,
            ticket: Once::new(id),
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

/// Effects the app sends back to the bus thread.
pub enum Effect {
    /// Reply to command `id` with `(rc, body)`.
    Respond { id: u64, rc: u8, body: String },
    Call {
        service: String,
        verb: String,
        args: Value,
        deadline: Instant,
        generation: Option<u64>,
        permit: Permit,
        reply: oneshot::Sender<Result<Value, String>>,
    },
    Delay {
        when: Instant,
        permit: Permit,
        reply: oneshot::Sender<()>,
    },
    /// Stop the bus thread (the app is quitting).
    Quit,
}

impl std::fmt::Debug for Effect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Respond { id, rc, .. } => f
                .debug_struct("Respond")
                .field("id", id)
                .field("rc", rc)
                .finish(),
            Self::Call { service, verb, .. } => f
                .debug_struct("Call")
                .field("service", service)
                .field("verb", verb)
                .finish(),
            Self::Delay { .. } => f.write_str("Delay(..)"),
            Self::Quit => f.write_str("Quit"),
        }
    }
}

/// The handle the app uses to call, reply and quit.
#[derive(Clone)]
pub struct BusHandle {
    tx: tokio::sync::mpsc::UnboundedSender<Effect>,
    /// Set and notified after the bounded worker drain. The structured shutdown
    /// receipt distinguishes completed replies from unsent/unconfirmed work.
    done: Arc<(Mutex<bool>, Condvar)>,
    client: Option<Arc<SupervisedClient>>,
    outgoing: Admission,
    cleanup: Admission,
    quitting: Arc<std::sync::atomic::AtomicBool>,
}

impl BusHandle {
    pub fn is_current(&self, request: &Request) -> bool {
        self.client.as_ref().is_none_or(|client| {
            settings::native::live_generation(client) == Some(request.generation)
        })
    }
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
        if self.quitting.load(std::sync::atomic::Ordering::Acquire) {
            return Err("Bus worker stopped".into());
        }
        let admission = if matches!(verb, "comp.region.cancel" | "comp.window.restore") {
            &self.cleanup
        } else {
            &self.outgoing
        };
        let deadline = Instant::now()
            .checked_add(limit)
            .ok_or("Bus deadline exhausted")?;
        let permit = admission.try_acquire().ok_or("Bus worker busy")?;
        let (tx, rx) = oneshot::channel();
        if let Err(error) = self.tx.send(Effect::Call {
            service: service.into(),
            verb: verb.into(),
            args,
            deadline,
            generation: self.settings_generation(),
            permit,
            reply: tx,
        }) {
            actor::retire_unsent(error.0);
            return Err("Bus worker stopped".into());
        }
        rx.await.map_err(|_| "Bus request abandoned")?
    }
    pub async fn delay(&self, duration: Duration) -> Result<(), String> {
        if self.quitting.load(std::sync::atomic::Ordering::Acquire) {
            return Err("Bus worker stopped".into());
        }
        let when = Instant::now()
            .checked_add(duration)
            .ok_or("Bus delay exhausted")?;
        let permit = self.outgoing.try_acquire().ok_or("Bus worker busy")?;
        let (tx, rx) = oneshot::channel();
        if let Err(error) = self.tx.send(Effect::Delay {
            when,
            permit,
            reply: tx,
        }) {
            actor::retire_unsent(error.0);
            return Err("Bus worker stopped".into());
        }
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
                outgoing: Admission::new(OPERATION_CAP),
                cleanup: Admission::new(CLEANUP_CAP),
                quitting: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            },
            rx,
        )
    }
    pub fn respond(&self, request: impl Into<Request>, rc: u8, body: String) {
        let request = request.into();
        let Some(id) = request.ticket.take() else {
            return;
        };
        if !self.is_current(&request) {
            return;
        }
        let _ = self.tx.send(Effect::Respond { id, rc, body });
    }
    pub fn quit(&self) {
        if self
            .quitting
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return;
        }
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

const ACCEPTED_CAP: usize = 32;
const REFUSAL_CAP: usize = 16;
const OPERATION_CAP: usize = 32;
const CLEANUP_CAP: usize = 4;
const OUTBOX_CAP: usize = 64;
const BUSY_BODY: &str = "{\"error\":\"too many pending Cap commands\"}";
const SHUTDOWN_BUDGET: Duration = Duration::from_secs(2);

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
    start_inner(
        service,
        url,
        handoff,
        OUTBOX_CAP,
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
    operations: usize,
    invariant_faults: usize,
}

fn start_inner(
    service: &str,
    url: &str,
    handoff: Option<Vec<String>>,
    gui_capacity: usize,
    #[cfg(test)] probe: Option<tokio::sync::watch::Sender<ActorProbe>>,
) -> Result<
    (
        BusHandle,
        Ui<()>,
        appearance::settings::Prepared,
        mpsc::Receiver<Delivery>,
    ),
    String,
> {
    let (send, receive) = mpsc::channel(gui_capacity);
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
                    runtime.block_on(worker(
                        service,
                        url,
                        handoff,
                        send,
                        rx,
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
        BusHandle {
            tx,
            done,
            client: Some(client),
            outgoing: Admission::new(OPERATION_CAP),
            cleanup: Admission::new(CLEANUP_CAP),
            quitting: Arc::new(std::sync::atomic::AtomicBool::new(false)),
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
}

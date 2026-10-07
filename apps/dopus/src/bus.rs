// SPDX-License-Identifier: MIT OR Apache-2.0
//! One supervised Bus actor with bounded owned work and offline GUI startup.
use std::{collections::{BTreeMap, HashMap}, sync::Arc, time::{Duration, Instant}};
use ::bus::native_client::{BoundedIncomingEvent, ConnState, IncomingCommand, NodedClient, RegistrationRejectionKind, SupervisedClient};
use application::iced::futures::channel::mpsc::{Receiver, Sender, channel};
use application::message::Once;
use application::native_actor::{Accepted, Faults, Reply as NativeReply, TaskSet, cancel, reap, submit_replies};
use application::native_queue::{Admission, Flush, Outbox, Permit, SendError};
use application::presentation::native::{Event as SettingsEvent, Progress, Session, Ui as SettingsUi, Worker as SettingsWorker, bridge};
mod actor;
#[cfg(test)] mod actor_tests;

/// Clones retain one response/mutation token and the receiving generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request { pub id: u64, generation: u64, ticket: Once<u64> }
impl Request { fn new(id: u64, generation: u64) -> Self { Self { id, generation, ticket: Once::new(id) } } }
#[cfg(test)] impl From<u64> for Request { fn from(id: u64) -> Self { Self::new(id, 0) } }
#[cfg(test)] impl From<i32> for Request { fn from(id: i32) -> Self { Self::new(u64::try_from(id).expect("nonnegative fixture id"), 0) } }

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
    Stopped {
        faults: Vec<String>,
    },
}

/// One request to dopus. `id` indexes a pending reply; `None`-reply verbs
/// still get one (an error reply at least).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub id: Request,
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
    pub reply_id: Option<Request>,
    pub binding: settings::Binding,
    pub expected_incarnation: String,
    pub expected_revision: settings::Revision,
    pub operation_id: String,
    pub changes: BTreeMap<String, serde_json::Value>,
    /// The requested selection, echoed on a successful apply.
    pub scheme: String,
    pub mode: String,
}



pub enum Effect {
    Respond { id: u64, rc: u8, body: String },
    ForwardOpen { paths: Vec<String>, permit: Permit },
    ThemeApply { request: ThemeRequest, generation: Option<u64>, deadline: Instant, permit: Permit },
    Quit,
}
impl std::fmt::Debug for Effect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Respond { id, rc, .. } => f.debug_struct("Respond").field("id",id).field("rc",rc).finish(),
            Self::ForwardOpen { .. } => f.write_str("ForwardOpen(..)"), Self::ThemeApply { .. } => f.write_str("ThemeApply(..)"), Self::Quit => f.write_str("Quit"),
        }
    }
}
#[derive(Default)]
struct Done { finished: bool, faults: Vec<String>, fault_count: u64 }
#[derive(Clone)]
pub struct BusHandle {
    tx: tokio::sync::mpsc::UnboundedSender<Effect>,
    done: Arc<(std::sync::Mutex<Done>, std::sync::Condvar)>,
    client: Option<Arc<SupervisedClient>>,
    settings: Option<SettingsUi<crate::app::Content>>,
    bootstrap: Option<appearance::settings::Prepared>,
    themes: Admission,
    quitting: Arc<std::sync::atomic::AtomicBool>,
    forwarded: Arc<std::sync::atomic::AtomicBool>,
}
impl BusHandle {
    #[cfg(test)]
    pub fn response_sink() -> (Self, tokio::sync::mpsc::UnboundedReceiver<Effect>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (Self { tx, done: Arc::new((std::sync::Mutex::new(Done { finished:true, ..Default::default() }),std::sync::Condvar::new())), client:None, settings:None, bootstrap:None,
            themes:Admission::new(THEME_QUEUE_BOUND), quitting:Arc::new(std::sync::atomic::AtomicBool::new(false)), forwarded:Arc::new(std::sync::atomic::AtomicBool::new(false)) },rx)
    }
    pub fn take_settings_ui(&mut self) -> Option<SettingsUi<crate::app::Content>> { self.settings.take() }
    pub fn take_bootstrap(&mut self) -> Option<appearance::settings::Prepared> { self.bootstrap.take() }
    pub fn settings_generation(&self) -> Option<u64> { self.client.as_ref().and_then(|client| settings::native::live_generation(client)) }
    pub fn registration_generation(&self) -> u64 { self.client.as_ref().map_or(0, |client| client.connection_generation()) }
    pub fn connected(&self) -> bool { self.client.as_ref().is_none_or(|client| settings::native::live_generation(client).is_some()) }
    pub fn is_current(&self, request: &Request) -> bool { self.client.as_ref().is_none_or(|client| settings::native::live_generation(client) == Some(request.generation)) }
    pub fn respond(&self, request: impl Into<Request>, rc: u8, body: String) {
        let request = request.into();
        let Some(id) = request.ticket.take() else { return; };
        if self.is_current(&request) { let _ = self.tx.send(Effect::Respond { id, rc, body }); }
    }
    pub fn forward_open(&self, paths: Vec<String>) {
        if self.quitting.load(std::sync::atomic::Ordering::Acquire) || self.forwarded.swap(true,std::sync::atomic::Ordering::AcqRel) { return; }
        let permit = Admission::new(1).try_acquire().expect("single lifetime forward");
        if let Err(error) = self.tx.send(Effect::ForwardOpen { paths, permit }) { actor::retire_unsent(error.0); }
    }
    pub fn theme_apply(&self, request: ThemeRequest) -> Result<(), String> {
        if self.quitting.load(std::sync::atomic::Ordering::Acquire) { return Err("Bus worker stopped".into()); }
        let Some(permit) = self.themes.try_acquire() else {
            if let Some(id) = request.reply_id { self.respond(id, 10, "{\"error_code\":\"BUSY\",\"message\":\"appearance queue exhausted\"}".into()); }
            return Err("appearance queue exhausted".into());
        };
        if let Some(id) = &request.reply_id {
            if !self.is_current(id) || id.ticket.take().is_none() { permit.finish(); return Err("Bus theme request retired".into()); }
        }
        let effect = Effect::ThemeApply { request, generation:self.settings_generation(), deadline:Instant::now()+SHUTDOWN_BUDGET, permit };
        if let Err(error) = self.tx.send(effect) { actor::retire_unsent(error.0); return Err("Bus worker stopped".into()); }
        Ok(())
    }
    pub fn quit(&self) { if !self.quitting.swap(true,std::sync::atomic::Ordering::AcqRel) { let _ = self.tx.send(Effect::Quit); } }
    /// Actual worker completion, with bounded retained fault text. A completed
    /// thread does not establish delivery of unconfirmed native replies.
    pub fn wait_done(&self, timeout:Duration) -> Result<Vec<String>,String> {
        let (lock,notified)=&*self.done;
        let state=lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (mut state,_)=notified.wait_timeout_while(state,timeout,|state|!state.finished).unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.finished { Ok(std::mem::take(&mut state.faults)) } else { Err("Bus shutdown did not complete in time".into()) }
    }
    pub fn shutdown_fault_count(&self) -> u64 { self.done.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).fault_count }
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



const PENDING_BOUND: usize = 32;
const JOB_BOUND: usize = 16;
const THEME_QUEUE_BOUND: usize = 4;
pub fn spawn(service:&str,url:&str)->Result<(BusHandle,Receiver<Delivery>),StartError> { actor::start(service,url,false,DELIVERY_BOUND,#[cfg(test)] None) }
pub fn spawn_settings(service:&str,url:&str)->Result<(BusHandle,Receiver<Delivery>),StartError> { actor::start(service,url,true,DELIVERY_BOUND,#[cfg(test)] None) }

#[cfg(test)]
#[derive(Clone, Debug, Default)]
struct ActorProbe { generation:u64, connected:bool, pending:usize, active:usize, reliable:usize, reply_tasks:usize, replies:usize, themes:usize, invariant_faults:usize }

async fn call_settings(
    client: &SupervisedClient,
    generation: u64,
    verb: &str,
    body: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let (rc, body, _) = client
        .call_with_headers_raw_at_generation(generation, "settingsd", verb, &BTreeMap::new(), &body.to_string())
        .await
        .map_err(|error| error.to_string())?;
    if rc == 0 { serde_json::from_str(&body).map_err(|error|error.to_string()) } else { Err(body) }
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
    generation: Option<u64>,
    deadline: Instant,
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
    let Some(generation) = generation.filter(|generation| settings::native::live_generation(client) == Some(*generation)) else {
        return refused("UNAVAILABLE", "queued appearance mutation retired; no call sent".into());
    };
    if Instant::now() >= deadline { return refused("UNAVAILABLE", "queued appearance mutation expired; no call sent".into()); }
    let deadline = tokio::time::Instant::from_std(deadline);
    let outcome: Result<(), (String, String)> = match tokio::time::timeout_at(deadline, async {
        let validated = call_settings(client, generation, "settings.validate", body.clone())
            .await
            .map_err(|message| authority_refusal(&message))?;
        let status = validated.get("status").and_then(|s| s.as_str());
        if status != Some("valid") {
            return Err(settings_refusal(
                status.unwrap_or("validation_failed"),
                &validated,
            ));
        }
        let applied = call_settings(client, generation, "settings.apply", body)
            .await
            .map_err(|message| authority_refusal(&message))?;
        let status = applied.get("status").and_then(|s| s.as_str());
        match status {
            Some("changed" | "unchanged") => Ok(()),
            _ => Err(settings_refusal(
                status.unwrap_or("apply_refused"),
                &applied,
            )),
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

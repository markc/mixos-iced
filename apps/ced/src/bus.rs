// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Bus thread (ced E1 plan §2): a current-thread tokio runtime on its own
//! OS thread (the `mixos-term-core/src/bus.rs:68-107` shape) holding a
//! `SupervisedClient` registered as `ced` (or `--service NAME`) with
//! `fatal_on_registration_rejection(true)`. It correlates requests, arms
//! one-shot timers and deadlines, subscribes topics (`edit.changed`,
//! `theme.changed`, `noded.props.changed`), forwards connection edges, and
//! hands `ced.*` commands to the Controller. Deliveries reach iced through an
//! unbounded futures channel exposed as a `Subscription` (no poll thread).
//!
//! Every wait here is an event: a Bus frame, an effect from the host, a
//! connection-state edge, or a one-shot timer the Controller armed. Requests
//! go out with `call_with_headers_raw` so a refusal's whole `{error_code,
//! message, reason, …}` body reaches the mirror (`call_typed` keeps only the
//! rendered message). A request that fails on the transport (disconnected) is
//! reported as its `Deadline` only once its deadline has passed, so a mirror
//! reconciling while the broker is down retries at most once per deadline —
//! never in a hot loop.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use ::bus::native_client::{
    BoundedIncomingEvent, ConnState, IncomingCommand, NodedClient, SupervisedClient,
};
use application::iced::futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use editor_model::types::{Incoming, ParsedBody};

use crate::controller::{BusCommand, Effect};
use application::presentation::native::{
    Event as SettingsEvent, Progress, Session, Ui as SettingsUi, Worker as SettingsWorker, bridge,
};

/// Everything the bus thread delivers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    Incoming(Incoming),
    Command(BusCommand),
    Settings,
    Registered,
    RegistrationFailed(StartError),
    HandoffFinished(Result<(), String>),
    Stopped { faults: Vec<String> },
}

enum WorkerCommand {
    Effect(Effect),
    ForwardOpen(Vec<String>),
    Shutdown(Option<crate::session::SessionWriter>),
}

/// Why the Bus could not be started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartError {
    /// Another instance owns the service name (single-instance forward).
    NameTaken,
    /// noded refused registration for another reason (message).
    Rejected(String),
    /// No broker reachable.
    Unreachable(String),
    /// Local desktop settings binding could not be resolved.
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

/// The service every mirror request goes to.
const EDIT: &str = "edit";
/// Success replies bigger than this are parsed on the bus thread, not the
/// UI thread (snapshot pages are up to 4 MiB).
const PARSE_OFF_UI_BYTES: usize = 64 * 1024;
/// Initial connect + register budget.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Single-instance probe deadline (plan §4.8).
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// The handle the Controller's host uses to perform [`Effect`]s that need the
/// Bus (sends, replies, timers, subscriptions). Other effects are the host's
/// own (chrome, clipboard, session) and are ignored here.
pub struct BusHandle {
    tx: tokio::sync::mpsc::UnboundedSender<WorkerCommand>,
    settings: Option<SettingsUi<crate::theme::Theme>>,
    client: Arc<SupervisedClient>,
}

impl BusHandle {
    pub fn service_name(&self) -> &str {
        self.client.service_name()
    }
    pub fn take_settings_ui(&mut self) -> Option<SettingsUi<crate::theme::Theme>> {
        self.settings.take()
    }
    pub fn settings_generation(&self) -> Option<u64> {
        settings::native::live_generation(&self.client)
    }
    pub fn registration_generation(&self) -> u64 {
        self.client.connection_generation()
    }
    pub fn connected(&self) -> bool {
        self.client.is_connected()
    }
    pub fn forward_bootstrap(&self, paths: Vec<String>) {
        let _ = self.tx.send(WorkerCommand::ForwardOpen(paths));
    }
    pub fn shutdown(&self, session: Option<crate::session::SessionWriter>) {
        let _ = self.tx.send(WorkerCommand::Shutdown(session));
    }
    pub fn perform(&self, effect: &Effect) {
        match effect {
            Effect::Send { .. }
            | Effect::Respond { .. }
            | Effect::Timer { .. }
            | Effect::Subscribe { .. }
            | Effect::Quit => {
                let _ = self.tx.send(WorkerCommand::Effect(effect.clone()));
            }
            _ => {}
        }
    }
}

/// The attested caller key editd would derive (E0 §4.3): `local:<from>` for a
/// registered local caller, `mesh:<service>@<peer>` for a mesh caller, else
/// `anon`. noded strips client-supplied `broker_*` headers, so these are the
/// broker's stamps.
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

/// Start the bus thread registered as `service`.
pub fn spawn(service: &str) -> Result<(BusHandle, UnboundedReceiver<Delivery>), StartError> {
    spawn_inner(service, false)
}
/// GUI bootstrap opts into settings; the headless controller keeps its existing
/// subscriptions and does not require a desktop session binding.
pub fn spawn_settings(
    service: &str,
) -> Result<(BusHandle, UnboundedReceiver<Delivery>), StartError> {
    spawn_inner(service, true)
}
fn spawn_inner(
    service: &str,
    desktop_settings: bool,
) -> Result<(BusHandle, UnboundedReceiver<Delivery>), StartError> {
    let (dtx, drx) = unbounded();
    let (etx, erx) = tokio::sync::mpsc::unbounded_channel();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let service = service.to_string();
    let url = ::bus::client_helpers::resolve_noded_url();
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
            runtime.block_on(run(service, url, dtx, erx, ready_tx, desktop_settings));
            // A timed-out spawn_blocking cache operation cannot be aborted.
            // Do not turn bounded worker shutdown into unbounded runtime Drop.
            runtime.shutdown_timeout(Duration::from_millis(100));
        })
        .map_err(|e| StartError::Unreachable(format!("Bus thread: {e}")))?;
    match ready_rx.recv() {
        Ok(Ok((client, settings))) => Ok((
            BusHandle {
                tx: etx,
                settings,
                client,
            },
            drx,
        )),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(StartError::Unreachable("the Bus thread exited".into())),
    }
}

type Ready = Result<
    (
        Arc<SupervisedClient>,
        Option<SettingsUi<crate::theme::Theme>>,
    ),
    StartError,
>;

fn settings_wake(delivery: &UnboundedSender<Delivery>, needed: bool) {
    if needed {
        let _ = delivery.unbounded_send(Delivery::Settings);
    }
}

async fn run(
    service: String,
    url: String,
    dtx: UnboundedSender<Delivery>,
    mut erx: tokio::sync::mpsc::UnboundedReceiver<WorkerCommand>,
    ready: std::sync::mpsc::Sender<Ready>,
    desktop_settings: bool,
) {
    let binding = match desktop_settings
        .then(settings::session::binding)
        .transpose()
    {
        Ok(binding) => binding,
        Err(error) => {
            let _ = ready.send(Err(StartError::SettingsBinding(error.message)));
            return;
        }
    };
    let options = match ::bus::client_helpers::local_supervised_options(&service, &url) {
        Ok(options) => options,
        Err(error) => {
            let _ = ready.send(Err(StartError::Unreachable(error.to_string())));
            return;
        }
    }
        .fatal_on_registration_rejection(true)
        .bounded_incoming(64);
    let client = if desktop_settings {
        Arc::new(options.start())
    } else {
        match tokio::time::timeout(CONNECT_TIMEOUT, options.connect()).await {
            Ok(Ok(c)) => Arc::new(c),
            Ok(Err(e)) => {
                let err = match e.registration_rejection() {
                    Some((_, msg)) if msg.contains("already registered") => StartError::NameTaken,
                    Some((rc, msg)) => StartError::Rejected(format!("rc {rc}: {msg}")),
                    None => StartError::Unreachable(e.to_string()),
                };
                let _ = ready.send(Err(err));
                return;
            }
            Err(_) => {
                let _ = ready.send(Err(StartError::Unreachable("connect timed out".into())));
                return;
            }
        }
    };
    let Some(mut incoming) = client.incoming_bounded() else {
        let _ = ready.send(Err(StartError::Unreachable("no incoming channel".into())));
        return;
    };
    let mut state = client.subscribe_state();
    let (ui, mut lane) = if let Some(binding) = binding {
        let consumer = match settings::consumer::Consumer::for_app(binding, "ced") {
            Ok(consumer) => consumer,
            Err(error) => {
                let _ = ready.send(Err(StartError::SettingsBinding(error.message)));
                return;
            }
        };
        let worker = match crate::dirs::AppDirs::resolve(crate::dirs::COMPONENT) {
            Some(dirs) => SettingsWorker::offline_with_cache(
                dirs.cache().join("settings"),
                crate::theme::from_settings,
            ),
            None => SettingsWorker::offline(crate::theme::from_settings),
        };
        let (ui, mut lane) = bridge(Session::new(consumer), worker);
        settings_wake(&dtx, lane.connect(Arc::clone(&client)));
        (Some(ui), Some(lane))
    } else {
        (None, None)
    };
    let _ = ready.send(Ok((Arc::clone(&client), ui)));

    let mut commands: HashMap<u64, IncomingCommand> = HashMap::new();
    let mut replies = tokio::task::JoinSet::new();
    let mut refusals = application::native_actor::Refusals::new(8);
    let mut refusal_faults = application::native_actor::Faults::default();
    let mut next_command = 0u64;
    let mut incoming_open = true;
    let mut registered = false;
    let mut shutdown_session = None;
    if desktop_settings && client.connection_generation() > 0 {
        registered = true;
        let _ = dtx.unbounded_send(Delivery::Registered);
    }
    if desktop_settings && matches!(client.state(), ConnState::Fatal | ConnState::ShuttingDown) {
        let _ = dtx.unbounded_send(Delivery::RegistrationFailed(registration_error(&client)));
    }
    loop {
        tokio::select! {
            result = refusals.join_next(), if !refusals.is_empty() => {
                if let Some(result) = result { application::native_actor::Refusals::record(result, &mut refusal_faults); }
            }
            result = replies.join_next(), if !replies.is_empty() => {
                if let Some(Ok(Err(error))) = result {
                    tracing::warn!(%error, "Ced Bus reply or handoff failed");
                }
            },
            progress = async { lane.as_mut().expect("guarded settings lane").drive().await }, if lane.is_some() => {
                match progress {
                    Progress::Wake => settings_wake(&dtx, true),
                    Progress::UiClosed => break,
                    Progress::Updated => {}
                }
            }
            cmd = incoming.recv(), if incoming_open => {
                let cmd = match cmd {
                    Some(BoundedIncomingEvent::Command(cmd)) => cmd,
                    Some(BoundedIncomingEvent::Overflow { .. }) => {
                        if let Some(lane) = &lane { settings_wake(&dtx, lane.publish(SettingsEvent::Lost)); }
                        // Mirrors conservatively reconcile any lost editor topics.
                        let _ = dtx.unbounded_send(Delivery::Incoming(Incoming::Connection { up: true }));
                        continue;
                    }
                    None => {
                        if !desktop_settings { break; }
                        incoming_open = false;
                        if let Some(lane) = &lane { settings_wake(&dtx, lane.publish(SettingsEvent::Wake)); }
                        continue;
                    },
                };
                if let Some(wake) = lane.as_ref().and_then(|lane| lane.delivery(&cmd)) {
                    settings_wake(&dtx, wake);
                    continue;
                }
                if let Some(topic) = cmd.topic() {
                    let _ = dtx.unbounded_send(Delivery::Incoming(Incoming::Topic { topic: topic.to_string(), body: cmd.body.clone() }));
                    continue;
                }
                if cmd.command.is_empty() {
                    continue;
                }
                if cmd.command == "app.describe" {
                    if settings::native::live_generation(&client) != Some(cmd.generation) { continue; }
                    if let Err(error) = application::describe::validate_request(&cmd.body) {
                        if refusals.try_reply(client.clone(), cmd, 10, crate::verbs::describe_refusal(&error),
                            std::time::Instant::now() + Duration::from_secs(2)).is_err() {
                            refusal_faults.push("description refusal capacity exhausted; request shed".into());
                        }
                        continue;
                    }
                }
                next_command += 1;
                let delivery = Delivery::Command(BusCommand {
                    id: next_command,
                    verb: cmd.command.clone(),
                    body: if cmd.body.trim().is_empty() { "{}".to_string() } else { cmd.body.clone() },
                    caller_key: caller_key(&cmd),
                });
                if cmd.id.is_some() {
                    commands.insert(next_command, cmd);
                }
                let _ = dtx.unbounded_send(delivery);
            }
            effect = erx.recv() => {
                let Some(command) = effect else { break };
                let effect = match command {
                    WorkerCommand::Shutdown(session) => { shutdown_session = session; break; }
                    WorkerCommand::ForwardOpen(paths) => {
                        let (url, service, d) = (url.clone(), service.clone(), dtx.clone());
                        if client.connection_generation() == 0 {
                            replies.spawn(async move {
                                let result = forward_open_async(&url, &service, &paths).await;
                                let _ = d.unbounded_send(Delivery::HandoffFinished(result.clone()));
                                result
                            });
                        }
                        continue;
                    }
                    WorkerCommand::Effect(effect) => effect,
                };
                match effect {
                    Effect::Send { req, out } => {
                        let (c, d) = (client.clone(), dtx.clone());
                        tokio::spawn(async move {
                            let deadline = tokio::time::Instant::now() + Duration::from_millis(out.deadline_ms);
                            let headers = BTreeMap::new();
                            let call = c.call_with_headers_raw(EDIT, &out.verb, &headers, &out.body);
                            let incoming = match tokio::time::timeout_at(deadline, call).await {
                                // A large success body (a snapshot page) is
                                // parsed here, off the UI thread.
                                Ok(Ok((rc, body, _))) if rc < 10 && body.len() > PARSE_OFF_UI_BYTES => {
                                    match serde_json::from_str::<serde_json::Value>(&body) {
                                        Ok(v) => Incoming::Parsed { req, rc, body: ParsedBody(v) },
                                        Err(_) => Incoming::Reply { req, rc, body },
                                    }
                                }
                                Ok(Ok((rc, body, _))) => Incoming::Reply { req, rc, body },
                                Ok(Err(_)) => {
                                    tokio::time::sleep_until(deadline).await;
                                    Incoming::Deadline { req }
                                }
                                Err(_) => Incoming::Deadline { req },
                            };
                            let _ = d.unbounded_send(Delivery::Incoming(incoming));
                        });
                    }
                    Effect::Respond { id, rc, body } => {
                        if let Some(cmd) = commands.remove(&id) {
                            let c = client.clone();
                            replies.spawn(async move {
                                tokio::time::timeout(Duration::from_secs(2), c.respond(&cmd, rc, &body)).await
                                    .map_err(|_| "Bus reply timed out".to_owned())?
                                    .map_err(|error| format!("Bus reply: {error}"))
                            });
                        }
                    }
                    Effect::Timer { id, ms } => {
                        let d = dtx.clone();
                        tokio::spawn(async move {
                            tokio::time::sleep(Duration::from_millis(ms)).await;
                            let _ = d.unbounded_send(Delivery::Incoming(Incoming::Timer { id }));
                        });
                    }
                    Effect::Subscribe { topic } => {
                        // The migrated GUI consumes compiled settings snapshots.
                        if lane.is_some() && topic == "theme.changed" { continue; }
                        let (c, d) = (client.clone(), dtx.clone());
                        tokio::spawn(async move {
                            // The client replays only topics that once
                            // subscribed, so a failed first subscribe would
                            // leave ced deaf for good (Opus m7): retry with
                            // backoff, and once it lands treat it as the
                            // reconnect edge — every mirror recovers what it
                            // missed.
                            let mut delay = Duration::from_millis(250);
                            let mut failed = false;
                            loop {
                                if matches!(c.state(), ConnState::ShuttingDown | ConnState::Fatal) { return; }
                                match c.subscribe_topic(&topic).await {
                                    Ok(_) => break,
                                    Err(e) => {
                                        if !failed {
                                            tracing::warn!("subscribe {topic}: {e}; retrying");
                                        }
                                        failed = true;
                                        tokio::time::sleep(delay).await;
                                        delay = (delay * 2).min(Duration::from_secs(5));
                                    }
                                }
                            }
                            if failed {
                                tracing::info!("subscribed {topic} after retrying");
                                let _ = d.unbounded_send(Delivery::Incoming(Incoming::Connection { up: true }));
                            }
                        });
                    }
                    Effect::Quit => break,
                    _ => {}
                }
            }
            changed = state.changed() => {
                if changed.is_err() {
                    break;
                }
                let now = *state.borrow_and_update();
                if desktop_settings && !registered && client.connection_generation() > 0 {
                    registered = true;
                    let _ = dtx.unbounded_send(Delivery::Registered);
                }
                match now {
                    ConnState::Connected => {
                        if let Some(lane) = &lane { settings_wake(&dtx, lane.publish(SettingsEvent::Wake)); }
                        let _ = dtx.unbounded_send(Delivery::Incoming(Incoming::Connection { up: true }));
                    }
                    ConnState::Disconnected => {
                        if let Some(lane) = &lane { settings_wake(&dtx, lane.publish(SettingsEvent::Wake)); }
                        let _ = dtx.unbounded_send(Delivery::Incoming(Incoming::Connection { up: false }));
                    }
                    ConnState::ShuttingDown | ConnState::Fatal => {
                        if !desktop_settings { break; }
                        if let Some(lane) = &lane { settings_wake(&dtx, lane.publish(SettingsEvent::Wake)); }
                        let error = registration_error(&client);
                        let _ = dtx.unbounded_send(Delivery::RegistrationFailed(error));
                        let _ = dtx.unbounded_send(Delivery::Incoming(Incoming::Connection { up: false }));
                    }
                    ConnState::Connecting => {}
                }
            }
        }
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let mut faults = Vec::new();
    refusals.drain(deadline, &mut refusal_faults).await;
    faults.extend(refusal_faults.recent().iter().cloned());
    // A quit response is queued before Shutdown. Let the existing response
    // tasks send it before closing its generation's socket.
    while !replies.is_empty() {
        match tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            replies.join_next(),
        )
        .await
        {
            Ok(Some(Ok(Ok(())))) => {}
            Ok(Some(Ok(Err(error)))) => faults.push(error),
            Ok(Some(Err(error))) => faults.push(format!("Bus reply: {error}")),
            Ok(None) => break,
            Err(_) => {
                faults.push("Bus reply or handoff drain timed out".into());
                replies.abort_all();
                break;
            }
        }
    }
    if desktop_settings {
        if let Err(error) = lane
            .as_mut()
            .expect("GUI settings lane")
            .flush_cache(deadline)
            .await
        {
            faults.push(format!("settings cache: {}: {}", error.code, error.message));
        }
        if let Some(mut writer) = shutdown_session {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let drained = tokio::task::spawn_blocking(move || writer.flush_for(remaining));
            match tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), drained).await {
                Ok(Ok(true)) => {}
                _ => faults.push("session drain timed out or failed".into()),
            }
        }
    }
    if tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), client.close())
        .await
        .is_err()
    {
        faults.push("Bus close timed out".into());
    }
    let _ = dtx.unbounded_send(Delivery::Stopped { faults });
}

fn registration_error(client: &SupervisedClient) -> StartError {
    match client.registration_rejection() {
        Some(reason) if reason.message.contains("already registered") => StartError::NameTaken,
        Some(reason) => StartError::Rejected(format!("rc {}: {}", reason.rc, reason.message)),
        None => StartError::Unreachable("connection stopped".into()),
    }
}

async fn forward_open_async(url: &str, service: &str, paths: &[String]) -> Result<(), String> {
    let client = tokio::time::timeout(Duration::from_secs(5), NodedClient::connect_anonymous(url))
        .await
        .map_err(|_| "forward connection timed out".to_string())?
        .map_err(|error| error.to_string())?;
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        let ping = client
            .call_with_headers_raw(service, "ced.ping", &BTreeMap::new(), "{}")
            .await
            .map_err(|error| error.to_string())?;
        if ping.0 != 0 {
            return Err(format!("ced.ping refused: rc {}", ping.0));
        }
        if paths.is_empty() {
            return Ok(());
        }
        let reply = client
            .call_with_headers_raw(
                service,
                "ced.open",
                &BTreeMap::new(),
                &serde_json::json!({"paths": paths}).to_string(),
            )
            .await
            .map_err(|error| error.to_string())?;
        if reply.0 == 0 {
            Ok(())
        } else {
            Err(format!("ced.open refused: rc {}: {}", reply.0, reply.1))
        }
    })
    .await
    .unwrap_or_else(|_| Err("forward request timed out".into()));
    let _ = tokio::time::timeout(Duration::from_millis(500), client.close()).await;
    result
}

/// One anonymous request to `service`, bounded by `limit`.
fn anonymous_call(
    service: &str,
    verb: &str,
    body: &serde_json::Value,
    limit: Duration,
) -> Option<(u8, String)> {
    let url = ::bus::client_helpers::resolve_noded_url();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    runtime.block_on(async {
        let call = async {
            let client = NodedClient::connect_anonymous(&url).await.ok()?;
            let reply = client
                .call_with_headers_raw(service, verb, &BTreeMap::new(), &body.to_string())
                .await
                .ok();
            client.close().await;
            reply.map(|(rc, body, _)| (rc, body))
        };
        tokio::time::timeout(limit, call).await.ok().flatten()
    })
}

/// Single-instance probe (plan §4.8): an anonymous `ced.ping` with a 500 ms
/// deadline; `true` when an instance answered. Then `forward_open` sends the
/// argv paths as `ced.open`.
pub fn probe_running(service: &str) -> bool {
    matches!(
        anonymous_call(service, "ced.ping", &serde_json::json!({}), PROBE_TIMEOUT),
        Some((0, _))
    )
}

pub fn forward_open(service: &str, paths: &[String]) -> Result<(), String> {
    match anonymous_call(
        service,
        "ced.open",
        &serde_json::json!({ "paths": paths }),
        Duration::from_secs(5),
    ) {
        Some((0, _)) => Ok(()),
        Some((rc, body)) => Err(format!("ced.open refused (rc {rc}): {body}")),
        None => Err(format!("no answer from {service}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn shared_refusal_credit_survives_completion_until_reap_and_overflow_preserves_origin() {
        use application::native_actor::{Faults, Refusals};
        let broker = term_test_broker::Broker::start_stable();
        let client = Arc::new(
            SupervisedClient::connect_options("refusal-owner", &broker.url)
                .bounded_incoming(2)
                .connect()
                .await
                .unwrap(),
        );
        let mut incoming = client.incoming_bounded().unwrap();
        let caller = Arc::new(NodedClient::connect_anonymous(&broker.url).await.unwrap());
        let mut refusals = Refusals::new(1);
        let mut faults = Faults::default();
        let calling = caller.clone();
        let first = tokio::spawn(async move {
            calling
                .call_with_headers_raw(
                    "refusal-owner",
                    "app.describe",
                    &BTreeMap::new(),
                    "{\"first\":true}",
                )
                .await
        });
        let Some(BoundedIncomingEvent::Command(command)) =
            tokio::time::timeout(Duration::from_secs(5), incoming.recv())
                .await
                .unwrap()
        else {
            panic!("actual first command missing");
        };
        assert!(
            refusals
                .try_reply(
                    client.clone(),
                    command,
                    10,
                    "{}".into(),
                    std::time::Instant::now() - Duration::from_millis(1)
                )
                .is_ok()
        );
        let completed = refusals.join_next().await.unwrap();
        assert!(refusals.is_empty());
        assert_eq!(refusals.counts().active, 1);
        let calling = caller.clone();
        let second = tokio::spawn(async move {
            calling
                .call_with_headers_raw(
                    "refusal-owner",
                    "app.describe",
                    &BTreeMap::new(),
                    "{\"second\":true}",
                )
                .await
        });
        let Some(BoundedIncomingEvent::Command(command)) =
            tokio::time::timeout(Duration::from_secs(5), incoming.recv())
                .await
                .unwrap()
        else {
            panic!("actual second command missing");
        };
        let origin = (
            command.from.clone(),
            command.id.clone(),
            command.generation,
            command.body.clone(),
            command.headers.clone(),
        );
        let command = refusals
            .try_reply(
                client.clone(),
                command,
                10,
                "{}".into(),
                std::time::Instant::now() + Duration::from_secs(2),
            )
            .unwrap_err();
        assert_eq!(
            (
                command.from.clone(),
                command.id.clone(),
                command.generation,
                command.body.clone(),
                command.headers.clone()
            ),
            origin
        );
        assert!(refusals.is_empty());
        Refusals::record(completed, &mut faults);
        assert_eq!(
            faults.count(),
            1,
            "expired absolute deadline is a recorded failure"
        );
        assert_eq!(refusals.counts().active, 0);
        assert_eq!(refusals.counts().finished, 1);
        assert!(
            refusals
                .try_reply(
                    client.clone(),
                    *command,
                    10,
                    "{\"refused\":true}".into(),
                    std::time::Instant::now() + Duration::from_secs(2)
                )
                .is_ok()
        );
        let (rc, body, _) = tokio::time::timeout(Duration::from_secs(5), second)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(rc, 10);
        assert_eq!(body, "{\"refused\":true}");
        Refusals::record(refusals.join_next().await.unwrap(), &mut faults);
        assert_eq!(refusals.counts().finished, 2);
        assert_eq!(faults.count(), 1);
        first.abort();
        let _ = first.await;
        refusals
            .drain(
                std::time::Instant::now() + Duration::from_secs(2),
                &mut faults,
            )
            .await;
        caller.close().await;
        client.close().await;
    }

    #[tokio::test]
    async fn invalid_descriptions_refuse_on_the_actual_actor_without_frontend_dispatch() {
        use application::iced::futures::StreamExt;
        let broker = term_test_broker::Broker::start_stable();
        for settings in [false, true] {
            let service = if settings {
                "ced-description-gui"
            } else {
                "ced-description-headless"
            };
            let (send, mut gui) = unbounded();
            let (effects, receive) = tokio::sync::mpsc::unbounded_channel();
            let (ready, observed) = std::sync::mpsc::channel();
            let actor = tokio::spawn(run(
                service.into(),
                broker.url.clone(),
                send,
                receive,
                ready,
                settings,
            ));
            let (client, _ui) =
                tokio::task::spawn_blocking(move || observed.recv_timeout(Duration::from_secs(5)))
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
            let mut connection = client.subscribe_state();
            tokio::time::timeout(Duration::from_secs(5), async {
                while settings::native::live_generation(&client).is_none() {
                    connection.changed().await.unwrap();
                }
            })
            .await
            .unwrap();
            assert_eq!(client.service_name(), service);
            let caller = NodedClient::connect_anonymous(&broker.url).await.unwrap();
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
                    caller.call_with_headers_raw(service, "app.describe", &BTreeMap::new(), &body),
                )
                .await
                .unwrap()
                .unwrap();
                let value: serde_json::Value = serde_json::from_str(&body)
                    .unwrap_or_else(|error| panic!("{service}: rc={rc} body={body:?}: {error}"));
                assert_eq!(rc, 10);
                assert_eq!(value["error_code"], "INVALID_ARGUMENT");
                assert!(value["describe_code"].is_string());
            }
            assert!(
                tokio::time::timeout(Duration::from_millis(20), async {
                    while let Some(delivery) = gui.next().await {
                        if matches!(delivery, Delivery::Command(_)) {
                            return;
                        }
                    }
                    panic!("actor unexpectedly closed");
                })
                .await
                .is_err(),
                "invalid request reached the stalled frontend"
            );
            effects.send(WorkerCommand::Shutdown(None)).unwrap();
            tokio::time::timeout(Duration::from_secs(4), actor)
                .await
                .unwrap()
                .unwrap();
            caller.close().await;
        }
    }

    fn cmd(from: &str, headers: &[(&str, &str)]) -> IncomingCommand {
        IncomingCommand {
            generation: 0,
            from: from.to_string(),
            command: "ced.ping".into(),
            id: Some("1".into()),
            args: serde_json::Value::Null,
            body: String::new(),
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
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
}

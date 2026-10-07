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
    Event as SettingsEvent, Jobs, Mailbox, Worker as SettingsWorker,
};

/// Everything the bus thread delivers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    Incoming(Incoming),
    Command(BusCommand),
    Settings(Mailbox<crate::theme::Theme>),
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
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StartError::NameTaken => f.write_str("the service name is already registered"),
            StartError::Rejected(m) => write!(f, "registration refused: {m}"),
            StartError::Unreachable(m) => write!(f, "Bus unreachable: {m}"),
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
    settings: tokio::sync::watch::Sender<Option<Jobs>>,
    binding: Option<settings::Binding>,
    client: Arc<SupervisedClient>,
}

impl BusHandle {
    pub fn settings_binding(&self) -> Option<settings::Binding> {
        self.binding.clone()
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
    pub fn settings_jobs(&self, jobs: Jobs) {
        self.settings.send_replace(Some(jobs));
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
    let (settings, settings_rx) = tokio::sync::watch::channel(None);
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
            runtime.block_on(run(
                service,
                url,
                dtx,
                erx,
                ready_tx,
                settings_rx,
                desktop_settings,
            ));
            // A timed-out spawn_blocking cache operation cannot be aborted.
            // Do not turn bounded worker shutdown into unbounded runtime Drop.
            runtime.shutdown_timeout(Duration::from_millis(100));
        })
        .map_err(|e| StartError::Unreachable(format!("Bus thread: {e}")))?;
    match ready_rx.recv() {
        Ok(Ok((binding, client))) => Ok((
            BusHandle {
                tx: etx,
                settings,
                binding,
                client,
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
    dtx: UnboundedSender<Delivery>,
    mut erx: tokio::sync::mpsc::UnboundedReceiver<WorkerCommand>,
    ready: std::sync::mpsc::Sender<
        Result<(Option<settings::Binding>, Arc<SupervisedClient>), StartError>,
    >,
    mut settings_rx: tokio::sync::watch::Receiver<Option<Jobs>>,
    desktop_settings: bool,
) {
    let binding = match desktop_settings
        .then(settings::session::binding)
        .transpose()
    {
        Ok(binding) => binding,
        Err(error) => {
            let _ = ready.send(Err(StartError::Rejected(error.message)));
            return;
        }
    };
    let options = SupervisedClient::connect_options(&service, &url)
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
    let _ = ready.send(Ok((binding.clone(), Arc::clone(&client))));
    let mut settings_worker = match desktop_settings
        .then(|| crate::dirs::AppDirs::resolve(crate::dirs::COMPONENT))
        .flatten()
    {
        Some(dirs) => SettingsWorker::offline_with_cache(
            dirs.cache().join("settings"),
            crate::theme::from_settings,
        ),
        None => SettingsWorker::offline(crate::theme::from_settings),
    };
    settings_worker.connect(Arc::clone(&client));
    let settings_mailbox = Mailbox::default();
    let settings_send = |event| {
        if settings_mailbox.publish(event) {
            let _ = dtx.unbounded_send(Delivery::Settings(settings_mailbox.clone()));
        }
    };

    let mut commands: HashMap<u64, IncomingCommand> = HashMap::new();
    let mut replies = tokio::task::JoinSet::new();
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
            _ = replies.join_next(), if !replies.is_empty() => {},
            changed = settings_rx.changed() => {
                if changed.is_err() { break; }
                let jobs = settings_rx.borrow_and_update().clone();
                if let Some(jobs) = jobs { settings_worker.replace(jobs); }
            }
            event = settings_worker.next() => { if let Some(event) = event.take() { settings_send(event); } }
            cmd = incoming.recv(), if incoming_open => {
                let cmd = match cmd {
                    Some(BoundedIncomingEvent::Command(cmd)) => cmd,
                    Some(BoundedIncomingEvent::Overflow { .. }) => {
                        if binding.is_some() { settings_send(SettingsEvent::Lost); }
                        // Mirrors conservatively reconcile any lost editor topics.
                        let _ = dtx.unbounded_send(Delivery::Incoming(Incoming::Connection { up: true }));
                        continue;
                    }
                    None => {
                        if !desktop_settings { break; }
                        incoming_open = false;
                        settings_send(SettingsEvent::Wake);
                        continue;
                    },
                };
                if let Some(decoded) = binding.as_ref().and_then(|binding| settings::native::Decoded::from_command(binding, &cmd)) {
                    settings_send(SettingsEvent::Delivery(decoded));
                    continue;
                }
                if let Some(topic) = cmd.topic() {
                    let _ = dtx.unbounded_send(Delivery::Incoming(Incoming::Topic { topic: topic.to_string(), body: cmd.body.clone() }));
                    continue;
                }
                if cmd.command.is_empty() {
                    continue;
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
                            tokio::spawn(async move {
                                let result = forward_open_async(&url, &service, &paths).await;
                                let _ = d.unbounded_send(Delivery::HandoffFinished(result));
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
                                let _ = tokio::time::timeout(Duration::from_secs(2), c.respond(&cmd, rc, &body)).await;
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
                        if binding.is_some() && topic == "theme.changed" { continue; }
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
                        if binding.is_some() { settings_send(SettingsEvent::Wake); }
                        let _ = dtx.unbounded_send(Delivery::Incoming(Incoming::Connection { up: true }));
                    }
                    ConnState::Disconnected => {
                        if binding.is_some() { settings_send(SettingsEvent::Wake); }
                        let _ = dtx.unbounded_send(Delivery::Incoming(Incoming::Connection { up: false }));
                    }
                    ConnState::ShuttingDown | ConnState::Fatal => {
                        if !desktop_settings { break; }
                        settings_send(SettingsEvent::Wake);
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
    // Quit may win select before the last activated save is received.
    if let Some(jobs) = settings_rx.borrow_and_update().clone() {
        settings_worker.replace(jobs);
    }
    // A quit response is queued before Shutdown. Let the existing response
    // tasks send it before closing its generation's socket.
    while !replies.is_empty() {
        match tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            replies.join_next(),
        )
        .await
        {
            Ok(Some(Ok(()))) => {}
            Ok(Some(Err(error))) => faults.push(format!("Bus reply: {error}")),
            Ok(None) => break,
            Err(_) => {
                faults.push("Bus reply drain timed out".into());
                replies.abort_all();
                break;
            }
        }
    }
    if desktop_settings {
        if let Err(error) = settings_worker.flush_cache(deadline).await {
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

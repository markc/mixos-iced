// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Bus thread — ced's `bus.rs` shape (itself the
//! `mixos-term-core/src/bus.rs` shape): a current-thread tokio runtime on its
//! own OS thread holding a [`SupervisedClient`] registered as `cap` (or
//! `--service NAME`) with `fatal_on_registration_rejection(true)`. It forwards
//! `cap.*` commands and `theme.changed` topic frames to the app through an
//! unbounded futures channel (exposed as a `Subscription`, no poll thread),
//! reports connection edges, and carries the app's replies back.
//!
//! Capture needs the native compositor service. Registration failure is an
//! explicit startup error; this app does not substitute a portal or shell tool.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use ::bus::native_client::{ConnState, IncomingCommand, NodedClient, SupervisedClient};
use iced::futures::channel::mpsc::{UnboundedReceiver, unbounded};

/// Everything the bus thread delivers to the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// A `cap.*` command (topics are separated out below).
    Command(Command),
    /// The `theme.changed` topic fired (body is the theme selection; the app
    /// re-resolves from the files, the same as ctk).
    ThemeChanged,
    Connected,
    Disconnected,
}

/// One request to cap. `id` indexes a pending reply; `None`-reply verbs
/// still get one (an error reply at least).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub id: u64,
    pub verb: String,
    pub body: String,
    /// `local:<from>` / `mesh:<service>@<peer>` / `anon` (editd E0 §4.3).
    pub caller_key: String,
}

/// Effects the app sends back to the bus thread.
#[derive(Debug)]
pub enum Effect {
    /// Reply to command `id` with `(rc, body)`.
    Respond { id: u64, rc: u8, body: String },
    Call {
        service: String,
        verb: String,
        args: serde_json::Value,
        limit: Duration,
        reply: iced::futures::channel::oneshot::Sender<Result<serde_json::Value, String>>,
    },
    Delay {
        duration: Duration,
        reply: iced::futures::channel::oneshot::Sender<()>,
    },
    /// Stop the bus thread (the app is quitting).
    Quit,
}

/// The handle the app uses to reply / quit.
#[derive(Clone)]
pub struct BusHandle {
    tx: tokio::sync::mpsc::UnboundedSender<Effect>,
    /// Set + notified when the bus thread has finished (replies flushed,
    /// client closed) — `wait_done` before process exit guarantees the
    /// last reply reached the wire instead of racing it. Arc-shared so
    /// the handle stays Clone (a raw Receiver is not).
    done: std::sync::Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
}

impl BusHandle {
    pub async fn call(
        &self,
        service: &str,
        verb: &str,
        args: serde_json::Value,
        limit: Duration,
    ) -> Result<serde_json::Value, String> {
        let (tx, rx) = iced::futures::channel::oneshot::channel();
        self.tx
            .send(Effect::Call {
                service: service.into(),
                verb: verb.into(),
                args,
                limit,
                reply: tx,
            })
            .map_err(|_| "Bus worker stopped")?;
        rx.await.map_err(|_| "Bus request abandoned")?
    }
    pub async fn delay(&self, duration: Duration) -> Result<(), String> {
        let (tx, rx) = iced::futures::channel::oneshot::channel();
        self.tx
            .send(Effect::Delay {
                duration,
                reply: tx,
            })
            .map_err(|_| "Bus worker stopped")?;
        rx.await.map_err(|_| "Bus delay abandoned".into())
    }
    /// Exercise the real window command performer without a broker connection.
    #[cfg(test)]
    pub fn response_sink() -> (Self, tokio::sync::mpsc::UnboundedReceiver<Effect>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (
            Self {
                tx,
                done: std::sync::Arc::new((std::sync::Mutex::new(true), std::sync::Condvar::new())),
            },
            rx,
        )
    }

    pub fn respond(&self, id: u64, rc: u8, body: String) {
        let _ = self.tx.send(Effect::Respond { id, rc, body });
    }

    pub fn quit(&self) {
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

/// Why the Bus could not be started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartError {
    /// Another instance owns the service name (single-instance forward).
    NameTaken,
    /// noded refused registration for another reason (message).
    Rejected(String),
    /// No broker reachable — run windowed without a Bus.
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
/// Single-instance probe deadline.
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);
/// The topic the shared theme selection announces on.
pub const THEME_TOPIC: &str = "theme.changed";

/// Start the bus thread registered as `service`, connecting to `url`
/// (`::bus::client_helpers::resolve_noded_url()` unless
/// `--noded-url` overrode it).
pub fn spawn(
    service: &str,
    url: &str,
) -> Result<(BusHandle, UnboundedReceiver<Delivery>), StartError> {
    let (dtx, drx) = unbounded();
    let (etx, erx) = tokio::sync::mpsc::unbounded_channel();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let done = std::sync::Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let done_thread = std::sync::Arc::clone(&done);
    let service = service.to_string();
    let url = url.to_owned();
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
            runtime.block_on(run(service, url, dtx, erx, ready_tx));
            let (lock, notified) = &*done_thread;
            *lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
            notified.notify_all();
        })
        .map_err(|e| StartError::Unreachable(format!("Bus thread: {e}")))?;
    match ready_rx.recv() {
        Ok(Ok(())) => Ok((BusHandle { tx: etx, done }, drx)),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(StartError::Unreachable("the Bus thread exited".into())),
    }
}

async fn run(
    service: String,
    url: String,
    dtx: iced::futures::channel::mpsc::UnboundedSender<Delivery>,
    mut erx: tokio::sync::mpsc::UnboundedReceiver<Effect>,
    ready: std::sync::mpsc::Sender<Result<(), StartError>>,
) {
    let connect = SupervisedClient::connect_options(&service, &url)
        .fatal_on_registration_rejection(true)
        .connect();
    let client = match tokio::time::timeout(CONNECT_TIMEOUT, connect).await {
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
    };
    let Some(mut incoming) = client.incoming() else {
        let _ = ready.send(Err(StartError::Unreachable("no incoming channel".into())));
        return;
    };
    let mut state = client.subscribe_state();
    match tokio::time::timeout(CONNECT_TIMEOUT, client.subscribe_topic(THEME_TOPIC)).await {
        Ok(Ok(())) => {}
        other => {
            let _ = ready.send(Err(StartError::Unreachable(format!(
                "theme subscription failed: {other:?}"
            ))));
            let _ = tokio::time::timeout(Duration::from_secs(2), client.close()).await;
            return;
        }
    }
    let _ = ready.send(Ok(()));

    // Commands awaiting a reply from the app.
    let mut commands: HashMap<u64, IncomingCommand> = HashMap::new();
    let mut next_command = 0u64;
    loop {
        tokio::select! {
            cmd = incoming.recv() => {
                let Some(cmd) = cmd else { break };
                if let Some(topic) = cmd.topic() {
                    if topic == THEME_TOPIC {
                        let _ = dtx.unbounded_send(Delivery::ThemeChanged);
                    }
                    continue;
                }
                if cmd.command.is_empty() {
                    continue;
                }
                if commands.len()>=32 {
                    let _=tokio::time::timeout(Duration::from_secs(2),client.respond(&cmd,10,"{\"error\":\"too many pending Cap commands\"}")).await;
                    continue;
                }
                next_command += 1;
                let delivery = Delivery::Command(Command {
                    id: next_command,
                    verb: cmd.command.clone(),
                    body: if cmd.body.trim().is_empty() { "{}".to_string() } else { cmd.body.clone() },
                    caller_key: caller_key(&cmd),
                });
                commands.insert(next_command, cmd);
                let _ = dtx.unbounded_send(delivery);
            }
            effect = erx.recv() => {
                let Some(effect) = effect else { break };
                match effect {
                    Effect::Call {service,verb,args,limit,reply} => {
                        let client=client.clone();
                        tokio::spawn(async move {
                            let result=tokio::time::timeout(limit,client.call(&service,&verb,args)).await
                                .map_err(|_|"Bus request timed out".to_string())
                                .and_then(|r|r.map_err(|e|e.to_string()));
                            let _=reply.send(result);
                        });
                    }
                    Effect::Delay {duration,reply} => {tokio::spawn(async move {tokio::time::sleep(duration).await;let _=reply.send(());});}
                    Effect::Respond { id, rc, body } => {
                        if let Some(cmd) = commands.remove(&id) {
                            // Awaited INLINE, not spawned: a reply —
                            // `cap.quit`'s above all — must be on the wire
                            // before this loop can break (Effect::Quit) and
                            // close the client under it. The 2 s cap keeps a
                            // wedged broker from hanging the thread.
                            let _ =
                                tokio::time::timeout(Duration::from_secs(2), client.respond(&cmd, rc, &body)).await;
                        }
                    }
                    Effect::Quit => {
                        // A quit racing its own reply must not swallow it:
                        // headless replies-then-quits in one breath, and
                        // select! may pick this arm while the Respond is
                        // still queued — drain every pending reply (each
                        // awaited inline, same 2 s cap) before breaking.
                        while let Ok(effect) = erx.try_recv() {
                            if let Effect::Respond { id, rc, body } = effect
                                && let Some(cmd) = commands.remove(&id)
                            {
                                let _ = tokio::time::timeout(
                                    Duration::from_secs(2),
                                    client.respond(&cmd, rc, &body),
                                )
                                .await;
                            }
                        }
                        break;
                    }
                }
            }
            changed = state.changed() => {
                if changed.is_err() {
                    break;
                }
                let edge = match *state.borrow_and_update() {
                    ConnState::Connected => Some(Delivery::Connected),
                    ConnState::Disconnected => Some(Delivery::Disconnected),
                    ConnState::ShuttingDown | ConnState::Fatal => {
                        break;
                    }
                    ConnState::Connecting => None,
                };
                if let Some(edge) = edge {
                    let _ = dtx.unbounded_send(edge);
                }
            }
        }
    }
    let _ = tokio::time::timeout(Duration::from_secs(2), client.close()).await;
}

/// One anonymous request to `service`, bounded by `limit`.
fn anonymous_call(
    url: &str,
    service: &str,
    verb: &str,
    body: &serde_json::Value,
    limit: Duration,
) -> Option<(u8, String)> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    runtime.block_on(async {
        let call = async {
            let client = NodedClient::connect_anonymous(url).await.ok()?;
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

/// Single-instance probe: an anonymous `cap.ping` with a 500 ms deadline;
/// `true` when an instance answered.
pub fn probe_running(url: &str, service: &str) -> bool {
    matches!(
        anonymous_call(
            url,
            service,
            "cap.ping",
            &serde_json::json!({}),
            PROBE_TIMEOUT
        ),
        Some((0, _))
    )
}

/// Single-instance forward: send the argv paths as `cap.open`. The running
/// instance opens one image, or raises its existing window when no path was
/// supplied. Dirty work is never discarded by a launcher invocation.
pub fn forward_open(url: &str, service: &str, paths: &[String]) -> Result<(), String> {
    forward_with(paths, |verb, args| {
        anonymous_call(url, service, verb, args, Duration::from_secs(5))
    })
}
fn forward_with(
    paths: &[String],
    mut call: impl FnMut(&str, &serde_json::Value) -> Option<(u8, String)>,
) -> Result<(), String> {
    let mut requests = Vec::new();
    if let Some(path) = paths.first() {
        requests.push(("cap.open", serde_json::json!({"path":path})));
    }
    requests.push(("cap.show", serde_json::json!({})));
    for (verb, args) in requests {
        match call(verb, &args) {
            Some((0, _)) => {}
            Some((rc, body)) => return Err(format!("{verb} refused (rc {rc}): {body}")),
            None => return Err(format!("no answer to {verb}")),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn launcher_open_activates_only_after_success() {
        let mut calls = Vec::new();
        forward_with(&["/tmp/image.png".into()], |verb, args| {
            calls.push((verb.to_string(), args.clone()));
            Some((0, String::new()))
        })
        .unwrap();
        assert_eq!(
            calls.iter().map(|c| c.0.as_str()).collect::<Vec<_>>(),
            ["cap.open", "cap.show"]
        );
        assert_eq!(calls[0].1["path"], "/tmp/image.png");
        let mut calls = Vec::new();
        assert!(
            forward_with(&["/tmp/image.png".into()], |verb, _| {
                calls.push(verb.to_string());
                Some((10, "dirty".into()))
            })
            .is_err()
        );
        assert_eq!(calls, ["cap.open"]);
    }

    fn cmd(from: &str, headers: &[(&str, &str)]) -> IncomingCommand {
        IncomingCommand {
            generation: 0,
            from: from.to_string(),
            command: "cap.ping".into(),
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

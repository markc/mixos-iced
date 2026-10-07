// SPDX-License-Identifier: MIT OR Apache-2.0
//! One supervised native Bus connection. Topics drive refreshes; no poller.
use ::bus::native_client::{
    BoundedIncomingEvent, ConnState, IncomingCommand, NodedClient, SupervisedClient,
    SupervisedError,
};
use application::iced::futures::SinkExt;
use application::iced::futures::channel::{mpsc, oneshot};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};

#[derive(Debug, Clone)]
pub enum Delivery {
    Command { id: u64, verb: String, body: String },
    Changed,
    Theme,
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
    Quit,
}
#[derive(Clone)]
pub struct Handle {
    tx: tokio::sync::mpsc::Sender<Effect>,
    control: tokio::sync::mpsc::UnboundedSender<Control>,
    done: Arc<(Mutex<bool>, Condvar)>,
    #[cfg(test)]
    records: Arc<Mutex<Vec<(u64, u8, Value)>>>,
    #[cfg(test)]
    stopped: Arc<std::sync::atomic::AtomicBool>,
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
        // 32 pending replies × their 2s budget, plus connection close.
        let (state, _) = changed
            .wait_timeout_while(state, Duration::from_secs(70), |done| !*done)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *state {
            Ok(())
        } else {
            Err("Bus shutdown did not complete".into())
        }
    }
    #[cfg(test)]
    pub fn sink() -> Self {
        let (tx, _rx) = tokio::sync::mpsc::channel(64);
        let (control, _rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            tx,
            control,
            done: Arc::new((Mutex::new(true), Condvar::new())),
            records: Arc::new(Mutex::new(Vec::new())),
            stopped: Arc::new(std::sync::atomic::AtomicBool::new(false)),
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
}

struct Finished(Arc<(Mutex<bool>, Condvar)>);
impl Drop for Finished {
    fn drop(&mut self) {
        let (lock, changed) = &*self.0;
        *lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        changed.notify_all();
    }
}

pub fn start(service: &str, url: &str) -> Result<(Handle, mpsc::Receiver<Delivery>), String> {
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
            let _finished = Finished(finished);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            match runtime {
                Ok(runtime) => {
                    runtime.block_on(worker(service, url, send, rx, controls, ready_send))
                }
                Err(error) => {
                    let _ = ready_send.send(Err(format!("Bus runtime: {error}")));
                }
            }
        })
        .map_err(|e| e.to_string())?;
    ready_receive
        .recv_timeout(Duration::from_secs(15))
        .map_err(|e| format!("Bus startup: {e}"))??;
    Ok((
        Handle {
            tx,
            control,
            done,
            #[cfg(test)]
            records: Arc::new(Mutex::new(Vec::new())),
            #[cfg(test)]
            stopped: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        },
        receive,
    ))
}
async fn worker(
    service: String,
    url: String,
    mut send: mpsc::Sender<Delivery>,
    mut effects: tokio::sync::mpsc::Receiver<Effect>,
    mut controls: tokio::sync::mpsc::UnboundedReceiver<Control>,
    ready: std::sync::mpsc::Sender<Result<(), String>>,
) {
    let connect = SupervisedClient::connect_options(&service, &url)
        .bounded_incoming(64)
        .fatal_on_registration_rejection(true)
        .with_initial_topics(vec![
            "theme.changed".to_owned(),
            "noded.props.changed".to_owned(),
        ])
        .connect();
    let client = match tokio::time::timeout(Duration::from_secs(5), connect).await {
        Ok(Ok(client)) => Arc::new(client),
        Ok(Err(error)) => {
            let _ = ready.send(Err(match error {
                SupervisedError::SubscriptionDeclaration(error) => {
                    format!("Bus subscription declaration: {error}")
                }
                error => format!("Bus registration: {error}"),
            }));
            return;
        }
        Err(_) => {
            let _ = ready.send(Err("Bus registration timed out".into()));
            return;
        }
    };
    let Some(mut incoming) = client.incoming_bounded() else {
        let _ = ready.send(Err("no incoming Bus channel".into()));
        return;
    };
    let mut connection = client.subscribe_state();
    let _ = ready.send(Ok(()));
    let mut pending: HashMap<u64, IncomingCommand> = HashMap::new();
    let mut next_id = 0;
    let permits = Arc::new(tokio::sync::Semaphore::new(16));
    loop {
        tokio::select! {
            biased;
            control = controls.recv() => {
                match control {
                    Some(Control::Reply(id,rc,value)) => {
                        if let Some(command) = pending.remove(&id) {
                            let _ = tokio::time::timeout(Duration::from_secs(2),client.respond(&command,rc,&value.to_string())).await;
                        }
                    }
                    Some(Control::Quit) | None => break,
                }
            }
            command = incoming.recv() => {
                let command = match command {
                    Some(BoundedIncomingEvent::Command(command)) => command,
                    Some(BoundedIncomingEvent::Overflow{..}) => {
                        let _ = send.send(Delivery::Changed).await;
                        let _ = send.send(Delivery::Theme).await;
                        continue;
                    },
                    None => break,
                };
                if let Some(topic) = command.topic() {
                    if topic == "noded.props.changed" && command.headers.get("gap").is_none_or(|value|value != "true")
                        && serde_json::from_str::<Value>(&command.body).ok().is_some_and(|body|body["path"] != "services.registered") {
                        continue;
                    }
                    let _ = send.send(if topic == "theme.changed" { Delivery::Theme } else { Delivery::Changed }).await;
                    continue;
                }
                if command.command.is_empty() {
                    let _ = tokio::time::timeout(Duration::from_secs(2),client.respond(&command,10,"{\"error_code\":\"ARGUMENT\",\"message\":\"command verb is empty\"}")).await;
                    continue;
                }
                if pending.len() >= 32 {
                    let _ = tokio::time::timeout(Duration::from_secs(2),client.respond(&command,10,"{\"error_code\":\"BUSY\",\"message\":\"too many pending commands\"}")).await;
                    continue;
                }
                next_id += 1;
                let delivery = Delivery::Command {id:next_id,verb:command.command.clone(),body:if command.body.trim().is_empty(){"{}".into()}else{command.body.clone()}};
                pending.insert(next_id,command);
                let _ = send.send(delivery).await;
            }
            effect = effects.recv() => {
                let Some(effect) = effect else { break; };
                match effect {
                    Effect::Call(service,verb,body,reply) => {
                        let Ok(permit) = permits.clone().try_acquire_owned() else {
                            let _ = reply.send(Err(CallError::not_sent("Bus call capacity exhausted; no call sent")));
                            continue;
                        };
                        let client = client.clone();
                        tokio::spawn(async move {
                            let _permit = permit;
                            let result = tokio::time::timeout(Duration::from_secs(30),client.call_with_headers_raw(&service,&verb,&BTreeMap::new(),&body)).await
                                .map_err(|_|CallError::from("Bus request timed out"))
                                .and_then(|v|v.map_err(CallError::transport))
                                .map(|(rc,body,_)|Reply{rc,body});
                            let _ = reply.send(result);
                        });
                    }
                }
            }
            changed = connection.changed() => {
                if changed.is_err() { break; }
                let event = match *connection.borrow_and_update() {
                    ConnState::Connected => Some(Delivery::Connected),
                    ConnState::Disconnected | ConnState::Connecting => { pending.clear(); Some(Delivery::Disconnected) },
                    ConnState::Fatal | ConnState::ShuttingDown => break,
                };
                if let Some(event) = event { let _ = send.send(event).await; }
            }
        }
    }
    let _ = send.try_send(Delivery::Disconnected);
    let _ = tokio::time::timeout(Duration::from_secs(2), client.close()).await;
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

    /// The initial ready signal follows the finite connect, which now
    /// includes the acknowledgement of both declared topics: `start` answers
    /// only once `theme.changed` and `noded.props.changed` are established.
    #[tokio::test]
    #[ignore = "requires isolated settings_test.mix broker"]
    async fn initial_ready_follows_finite_establishment_of_both_declared_topics() {
        use application::iced::futures::StreamExt;
        let url = std::env::var("MIXOS_NODED_URL").expect("isolated broker url");
        let (handle, mut deliveries) =
            start("busviewer-sampled-fixture", &url).expect("both declared topics acknowledged");
        handle.quit();
        while let Some(delivery) = deliveries.next().await {
            if matches!(delivery, Delivery::Disconnected) {
                break;
            }
        }
        handle.wait_done().expect("bounded Bus shutdown");
    }
}

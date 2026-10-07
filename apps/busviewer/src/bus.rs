// SPDX-License-Identifier: MIT OR Apache-2.0
//! One supervised native Bus connection. Topics drive refreshes; no poller.
use ::bus::native_client::{BoundedIncomingEvent, ConnState, IncomingCommand, NodedClient, SupervisedClient};
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
    Reply(u64, u8, Value),
    Call(
        String,
        String,
        String,
        oneshot::Sender<Result<Reply, String>>,
    ),
    Quit,
}
#[derive(Clone)]
pub struct Handle {
    tx: tokio::sync::mpsc::Sender<Effect>,
    done: Arc<(Mutex<bool>, Condvar)>,
}
impl Handle {
    pub async fn raw(&self, service: &str, verb: &str, body: String) -> Result<Reply, String> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Effect::Call(service.into(), verb.into(), body, tx))
            .await
            .map_err(|_| "Bus stopped")?;
        rx.await.map_err(|_| "Bus request abandoned")?
    }
    pub async fn call(&self, service: &str, verb: &str, args: Value) -> Result<Reply, String> {
        self.raw(service, verb, args.to_string()).await
    }
    pub fn reply(&self, id: u64, rc: u8, body: Value) {
        let _ = self.tx.try_send(Effect::Reply(id, rc, body));
    }
    pub fn quit(&self) {
        let _ = self.tx.try_send(Effect::Quit);
    }
    pub fn wait_done(&self) {
        let (lock, changed) = &*self.done;
        let state = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = changed
            .wait_timeout_while(state, Duration::from_secs(5), |done| !*done)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
    }
    #[cfg(test)]
    pub fn sink() -> Self {
        let (tx, _rx) = tokio::sync::mpsc::channel(64);
        Self {
            tx,
            done: Arc::new((Mutex::new(true), Condvar::new())),
        }
    }
}

pub fn start(
    service: &str,
    url: &str,
) -> Result<(Handle, mpsc::Receiver<Delivery>), String> {
    let (send, receive) = mpsc::channel(64);
    let (tx, rx) = tokio::sync::mpsc::channel(64);
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
                Ok(runtime) => runtime.block_on(worker(service, url, send, rx, ready_send)),
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
    ready_receive
        .recv_timeout(Duration::from_secs(15))
        .map_err(|e| format!("Bus startup: {e}"))??;
    Ok((Handle { tx, done }, receive))
}
async fn worker(
    service: String,
    url: String,
    mut send: mpsc::Sender<Delivery>,
    mut effects: tokio::sync::mpsc::Receiver<Effect>,
    ready: std::sync::mpsc::Sender<Result<(), String>>,
) {
    let connect = SupervisedClient::connect_options(&service, &url)
        .bounded_incoming(64)
        .fatal_on_registration_rejection(true)
        .connect();
    let client = match tokio::time::timeout(Duration::from_secs(5), connect).await {
        Ok(Ok(client)) => Arc::new(client),
        Ok(Err(error)) => {
            let _ = ready.send(Err(format!("Bus registration: {error}")));
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
    for topic in ["theme.changed".to_owned(), "noded.props.changed".to_owned()] {
        if !matches!(
            tokio::time::timeout(Duration::from_secs(2), client.subscribe_topic(&topic)).await,
            Ok(Ok(()))
        ) {
            let _ = ready.send(Err(format!("cannot subscribe to {topic}")));
            let _ = client.close().await;
            return;
        }
    }
    let _ = ready.send(Ok(()));
    let mut pending: HashMap<u64, IncomingCommand> = HashMap::new();
    let mut next_id = 0;
    let permits = Arc::new(tokio::sync::Semaphore::new(16));
    loop {
        tokio::select! {
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
                    Effect::Reply(id,rc,value) => {
                        if let Some(command) = pending.remove(&id) {
                            let _ = tokio::time::timeout(Duration::from_secs(2),client.respond(&command,rc,&value.to_string())).await;
                        }
                    }
                    Effect::Call(service,verb,body,reply) => {
                        let Ok(permit) = permits.clone().try_acquire_owned() else {
                            let _ = reply.send(Err("Bus call capacity exhausted; no call sent".into()));
                            continue;
                        };
                        let client = client.clone();
                        tokio::spawn(async move {
                            let _permit = permit;
                            let result = tokio::time::timeout(Duration::from_secs(30),client.call_with_headers_raw(&service,&verb,&BTreeMap::new(),&body)).await
                                .map_err(|_|"Bus request timed out".to_owned())
                                .and_then(|v|v.map_err(|e|e.to_string()))
                                .map(|(rc,body,_)|Reply{rc,body});
                            let _ = reply.send(result);
                        });
                    }
                    Effect::Quit => break,
                }
            }
            changed = connection.changed() => {
                if changed.is_err() { break; }
                let event = match *connection.borrow_and_update() {
                    ConnState::Connected => Some(Delivery::Connected),
                    ConnState::Disconnected => { pending.clear(); Some(Delivery::Disconnected) },
                    ConnState::Fatal | ConnState::ShuttingDown => break,
                    ConnState::Connecting => None,
                };
                if let Some(event) = event { let _ = send.send(event).await; }
            }
        }
    }
    let _ = send.send(Delivery::Disconnected).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), client.close()).await;
}
fn anonymous(url: &str, service: &str, verb: &str, args: Value) -> Result<Reply, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(5), async {
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
    let reply = handle.raw(service, verb, String::new()).await?;
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

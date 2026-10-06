// SPDX-License-Identifier: MIT OR Apache-2.0
//! One supervised native Bus connection. Topics drive refreshes; no poller.
use ::bus::native_client::{ConnState, IncomingCommand, NodedClient, SupervisedClient};
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
    pub value: Value,
}
enum Effect {
    Reply(u64, u8, Value),
    Call(
        String,
        String,
        Value,
        oneshot::Sender<Result<Reply, String>>,
    ),
    Quit,
}
#[derive(Clone)]
pub struct Handle {
    tx: tokio::sync::mpsc::UnboundedSender<Effect>,
    done: Arc<(Mutex<bool>, Condvar)>,
}
impl Handle {
    pub async fn call(&self, service: &str, verb: &str, args: Value) -> Result<Reply, String> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Effect::Call(service.into(), verb.into(), args, tx))
            .map_err(|_| "Bus stopped")?;
        rx.await.map_err(|_| "Bus request abandoned")?
    }
    pub fn reply(&self, id: u64, rc: u8, body: Value) {
        let _ = self.tx.send(Effect::Reply(id, rc, body));
    }
    pub fn quit(&self) {
        let _ = self.tx.send(Effect::Quit);
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
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            tx,
            done: Arc::new((Mutex::new(true), Condvar::new())),
        }
    }
}

pub fn start(
    service: &str,
    url: &str,
    host: &str,
) -> Result<(Handle, mpsc::UnboundedReceiver<Delivery>), String> {
    let (send, receive) = mpsc::unbounded();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let (ready_send, ready_receive) = std::sync::mpsc::channel();
    let done = Arc::new((Mutex::new(false), Condvar::new()));
    let finished = done.clone();
    let service = service.to_owned();
    let url = url.to_owned();
    let host = host.to_owned();
    std::thread::Builder::new()
        .name("scene-editor-bus".into())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            match runtime {
                Ok(runtime) => runtime.block_on(worker(service, url, host, send, rx, ready_send)),
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
    host: String,
    send: mpsc::UnboundedSender<Delivery>,
    mut effects: tokio::sync::mpsc::UnboundedReceiver<Effect>,
    ready: std::sync::mpsc::Sender<Result<(), String>>,
) {
    let connect = SupervisedClient::connect_options(&service, &url)
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
    let Some(mut incoming) = client.incoming() else {
        let _ = ready.send(Err("no incoming Bus channel".into()));
        return;
    };
    let mut connection = client.subscribe_state();
    for topic in [
        "theme.changed".to_owned(),
        "scenes.changed".to_owned(),
        "noded.props.changed".to_owned(),
        format!("{host}.panel.changed"),
    ] {
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
    loop {
        tokio::select! {
            command = incoming.recv() => {
                let Some(command) = command else { break; };
                if let Some(topic) = command.topic() {
                    if topic == "noded.props.changed" && command.headers.get("gap").is_none_or(|value|value != "true")
                        && serde_json::from_str::<Value>(&command.body).ok().is_some_and(|body|body["path"] != "services.registered") {
                        continue;
                    }
                    let _ = send.unbounded_send(if topic == "theme.changed" { Delivery::Theme } else { Delivery::Changed });
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
                let _ = send.unbounded_send(delivery);
            }
            effect = effects.recv() => {
                let Some(effect) = effect else { break; };
                match effect {
                    Effect::Reply(id,rc,value) => {
                        if let Some(command) = pending.remove(&id) {
                            let _ = tokio::time::timeout(Duration::from_secs(2),client.respond(&command,rc,&value.to_string())).await;
                        }
                    }
                    Effect::Call(service,verb,args,reply) => {
                        let client = client.clone();
                        tokio::spawn(async move {
                            let result = tokio::time::timeout(Duration::from_secs(30),client.call_with_headers_raw(&service,&verb,&BTreeMap::new(),&args.to_string())).await
                                .map_err(|_|"Bus request timed out".to_owned())
                                .and_then(|v|v.map_err(|e|e.to_string()))
                                .and_then(|(rc,body,_)|serde_json::from_str(&body).map(|value|Reply{rc,value}).map_err(|e|format!("invalid reply: {e}")));
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
                    ConnState::Disconnected => Some(Delivery::Disconnected),
                    ConnState::Fatal | ConnState::ShuttingDown => break,
                    ConnState::Connecting => None,
                };
                if let Some(event) = event { let _ = send.unbounded_send(event); }
            }
        }
    }
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
            let value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
            Ok(Reply { rc, value })
        })
        .await
        .map_err(|_| "activation timed out".to_owned())?
    })
}
pub fn probe(url: &str, service: &str) -> bool {
    anonymous(url, service, "scene-editor.ping", json!({})).is_ok_and(|r| r.rc == 0)
}
pub fn forward(
    url: &str,
    service: &str,
    selection: Option<&crate::model::Selection>,
) -> Result<(), String> {
    let body = match selection {
        Some(selection) => json!({"view":selection.view,"scene":selection.scene}),
        None => json!({}),
    };
    let reply = anonymous(url, service, "scene-editor.show", body)?;
    if reply.rc != 0 {
        Err(format!("activation refused: {}", reply.value))
    } else {
        Ok(())
    }
}

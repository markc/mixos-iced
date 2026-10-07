// SPDX-License-Identifier: MIT OR Apache-2.0
//! One supervised native Bus connection. Topics drive refreshes; no poller.
use ::bus::native_client::{
    BoundedIncomingEvent, ConnState, IncomingCommand, NodedClient, RegistrationRejectionKind,
    SupervisedClient,
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

#[derive(Debug, Clone)]
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
    Forward(crate::model::Selection),
}
#[derive(Clone)]
pub struct Handle {
    tx: tokio::sync::mpsc::UnboundedSender<Effect>,
    done: Arc<(Mutex<bool>, Condvar)>,
    client: Option<Arc<SupervisedClient>>,
}
impl Handle {
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
    pub fn forward_selection(&self, selection: crate::model::Selection) {
        let _ = self.tx.send(Effect::Forward(selection));
    }
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
            client: None,
        }
    }
}

pub fn start(
    service: &str,
    url: &str,
    host: &str,
) -> Result<
    (
        Handle,
        Ui<()>,
        appearance::settings::Prepared,
        mpsc::UnboundedReceiver<Delivery>,
    ),
    String,
> {
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
                Ok(runtime) => {
                    runtime.block_on(worker(service, url, host, send, rx, ready_send));
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
            done,
            client: Some(client),
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
fn settings_wake(send: &mpsc::UnboundedSender<Delivery>, needed: bool) {
    if needed {
        let _ = send.unbounded_send(Delivery::Settings);
    }
}

async fn worker(
    service: String,
    url: String,
    host: String,
    send: mpsc::UnboundedSender<Delivery>,
    mut effects: tokio::sync::mpsc::UnboundedReceiver<Effect>,
    ready: Ready,
) {
    let consumer = match settings::session::binding()
        .and_then(|binding| settings::consumer::Consumer::for_app(binding, "scene-editor"))
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
    let client = Arc::new(
        SupervisedClient::connect_options(&service, &url)
            .fatal_on_registration_rejection(true)
            .bounded_incoming(64)
            .with_initial_topics(vec![
                "scenes.changed".to_owned(),
                "noded.props.changed".to_owned(),
                format!("{host}.panel.changed"),
            ])
            .establishment_timeout(Duration::from_secs(5))
            .start(),
    );
    let Some(mut incoming) = client.incoming_bounded() else {
        let _ = ready.send(Err("no incoming Bus channel".into()));
        return;
    };
    let mut connection = client.subscribe_state();
    let build = |_: &appearance::settings::Prepared, _: &settings::Snapshot| Ok(());
    let settings_worker = match config::AppDirs::resolve("scene-editor") {
        Some(dirs) => Worker::offline_with_cache(dirs.cache().join("settings"), build),
        None => Worker::offline(build),
    };
    let (ui, mut lane) = bridge(Session::new(consumer), settings_worker);
    settings_wake(&send, lane.connect(Arc::clone(&client)));
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
        eprintln!("scene-editor: static assets: {error}");
    }
    let mut pending: HashMap<u64, IncomingCommand> = HashMap::new();
    let mut next_id = 0;
    let mut incoming_open = true;
    let mut connection_open = true;
    let mut operations = tokio::task::JoinSet::new();
    let mut lifecycle = None;
    // Sample once before waiting: fast registration may already have completed.
    loop {
        let now = *connection.borrow_and_update();
        if lifecycle != Some(now) {
            lifecycle = Some(now);
            settings_wake(&send, lane.publish(SettingsEvent::Wake));
            match now {
                ConnState::Connected => {
                    let _ = send.unbounded_send(Delivery::Connected);
                }
                ConnState::Fatal | ConnState::ShuttingDown => {
                    // A declared-topic refusal is terminal with its own
                    // diagnostic; it is never the registration NameTaken kind.
                    let (name_taken, message) = if let Some(error) =
                        client.subscription_declaration_error()
                    {
                        (false, error.to_string())
                    } else if let Some(reason) = client.registration_rejection() {
                        (
                            reason.kind() == RegistrationRejectionKind::NameTaken,
                            reason.message,
                        )
                    } else {
                        (false, "connection stopped".into())
                    };
                    let _ = send.unbounded_send(Delivery::Refused { name_taken, message });
                }
                ConnState::Disconnected => {
                    let _ = send.unbounded_send(Delivery::Disconnected);
                }
                ConnState::Connecting => {}
            }
        }
        tokio::select! {
            result = operations.join_next(), if !operations.is_empty() => {
                if let Some(Err(error)) = result { eprintln!("scene-editor: Bus work: {error}"); }
            }
            progress = lane.drive() => match progress {
                Progress::Wake => settings_wake(&send, true),
                Progress::UiClosed => break,
                Progress::Updated => {}
            },
            command = incoming.recv(), if incoming_open => {
                let command = match command {
                    Some(BoundedIncomingEvent::Command(command)) => command,
                    Some(BoundedIncomingEvent::Overflow { .. }) => {
                        settings_wake(&send, lane.publish(SettingsEvent::Lost));
                        let _ = send.unbounded_send(Delivery::Changed);
                        continue;
                    }
                    None => {
                        incoming_open = false;
                        settings_wake(&send, lane.publish(SettingsEvent::Wake));
                        continue;
                    }
                };
                if let Some(wake) = lane.delivery(&command) { settings_wake(&send, wake); continue; }
                if let Some(topic) = command.topic() {
                    if topic == "noded.props.changed" && command.headers.get("gap").is_none_or(|value|value != "true")
                        && serde_json::from_str::<Value>(&command.body).ok().is_some_and(|body|body["path"] != "services.registered") {
                        continue;
                    }
                    let _ = send.unbounded_send(Delivery::Changed);
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
                        operations.spawn(async move {
                            let result = tokio::time::timeout(Duration::from_secs(30),client.call_with_headers_raw(&service,&verb,&BTreeMap::new(),&args.to_string())).await
                                .map_err(|_|"Bus request timed out".to_owned())
                                .and_then(|v|v.map_err(|e|e.to_string()))
                                .and_then(|(rc,body,_)|serde_json::from_str(&body).map(|value|Reply{rc,value}).map_err(|e|format!("invalid reply: {e}")));
                            let _ = reply.send(result);
                        });
                    }
                    Effect::Forward(selection) => {
                        let url = url.clone();
                        let service = service.clone();
                        let send = send.clone();
                        operations.spawn(async move {
                            let result = forward_async(&url, &service, &selection).await;
                            let _ = send.unbounded_send(Delivery::Forwarded(result));
                        });
                    }
                    Effect::Quit => break,
                }
            }
            changed = connection.changed(), if connection_open => {
                if changed.is_err() { connection_open = false; }
            }
        }
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let mut faults = Vec::new();
    operations.abort_all();
    if let Err(error) = lane.flush_cache(deadline).await {
        faults.push(format!("settings cache: {}: {}", error.code, error.message));
    }
    if tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), client.close())
        .await
        .is_err()
    {
        faults.push("Bus close timed out".into());
    }
    eprintln!("SCENE_EDITOR_SHUTDOWN {}", json!({"faults":faults}));
}

async fn forward_async(
    url: &str,
    service: &str,
    selection: &crate::model::Selection,
) -> Result<(), String> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let client = NodedClient::connect_anonymous(url)
            .await
            .map_err(|error| error.to_string())?;
        let result = client
            .call_with_headers_raw(
                service,
                "scene-editor.show",
                &BTreeMap::new(),
                &json!({"view":selection.view,"scene":selection.scene}).to_string(),
            )
            .await;
        client.close().await;
        let (rc, body, _) = result.map_err(|error| error.to_string())?;
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

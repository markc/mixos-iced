// SPDX-License-Identifier: MIT OR Apache-2.0

//! The supervised client against an in-process stub broker: it survives a
//! broker bounce, re-registers, replays its subscriptions in recorded order,
//! keeps the incoming stream open across the drop, fails fast while
//! disconnected and never leaks a socket from a failed reconnect.

#![cfg(feature = "client")]

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use bus::native_client::BoundedIncomingEvent;
use bus::{BusMessage, ConnState, Connection, SupervisedClient, SupervisedError};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, Notify, oneshot};
use tokio_tungstenite::tungstenite::Message;

#[derive(Default)]
struct StubState {
    /// `from` of every `noded.register`, in order: one per connection.
    register_names: Vec<String>,
    register_bodies: Vec<String>,
    /// `name` of every `rc 0` `topic.subscribe`, in order.
    subscribed: Vec<String>,
    /// `name` of every `rc 0` `topic.unsubscribe`, in order.
    unsubscribed: Vec<String>,
    /// Every `topic.subscribe` seen, rejected ones included.
    subscribe_attempts: usize,
    deregistered: bool,
    responses: Vec<BusMessage>,
    /// Connections accepted so far.
    connections: usize,
    /// Connections currently open. A reconnect socket the client leaked
    /// never decrements this.
    open_connections: usize,
    /// Registered name to owning connection, as the real broker keeps it: a
    /// name belongs to one open connection, is freed when that socket
    /// closes, and a second register for a name another open connection
    /// holds is a collision.
    active_registrations: HashMap<String, usize>,
}

/// The shape of the register-rejection reply the stub emits, mirroring the
/// broker generations a client can face.
enum RejectionShape {
    /// Real noded: rc=10, the `error` header, and the structured v1 body.
    Structured,
    /// A legacy broker: rc=10 and an `error` header only.
    LegacyText,
    /// Structured-looking but with an unrecognised `error_code`.
    Malformed,
}

/// A per-topic `topic.subscribe` behaviour the stub enforces, the barrier
/// mechanism the declaration tests drive.
#[derive(Clone)]
enum SubscribeAction {
    /// Reply rc 0 (the default).
    Ack,
    /// Reply with this rc and an `error` header.
    Refuse(u8),
    /// Signal `entered`, wait for `release`, then reply `rc`.
    Hold {
        entered: Arc<Notify>,
        release: Arc<Notify>,
        rc: u8,
    },
    /// Close the socket without replying.
    Disconnect,
}

struct Stub {
    state: Mutex<StubState>,
    /// Fired by a test to drop connection #1 (the simulated bounce).
    drop_conn1: Notify,
    /// Reject `topic.subscribe` on reconnected sockets (connection >= 2).
    fail_replay_on_reconnect: bool,
    /// Reject `noded.register` on reconnected sockets but keep the socket
    /// open, as the real broker does on a collision.
    reject_register_on_reconnect: bool,
    /// Refuse this topic name on every connection.
    reject_topic: Option<String>,
    /// Reject `topic.unsubscribe` for this topic on every connection.
    reject_unsubscribe: Option<String>,
    /// The shape of a collision reply.
    rejection_shape: RejectionShape,
    flood_on_register: usize,
    /// Extra commands flooded right after the FIRST register ACK only.
    first_register_flood: usize,
    replay_delay: Duration,
    register_delay: Duration,
    /// Delay every subscribe reply (after receipt, before the record).
    subscribe_delay: Duration,
    /// Delay the FIRST connection's subscribe replies only.
    initial_subscribe_delay: Duration,
    /// Close the socket right after ACKing registration on connection 1.
    drop_after_register_first: bool,
    /// Send a HELP request on reconnected sockets (connection >= 2).
    help_on_reconnect: bool,
    /// Per-topic subscribe actions for the first connection.
    initial_actions: HashMap<String, SubscribeAction>,
    /// Per-topic subscribe actions for reconnected sockets (connection >= 2).
    replay_actions: HashMap<String, SubscribeAction>,
    /// Per-topic subscribe actions on every connection (checked first).
    all_actions: HashMap<String, SubscribeAction>,
}

impl Stub {
    fn flooding(commands: usize) -> Arc<Stub> {
        Arc::new(Stub {
            state: Mutex::new(StubState::default()),
            drop_conn1: Notify::new(),
            fail_replay_on_reconnect: false,
            reject_register_on_reconnect: false,
            reject_topic: None,
            reject_unsubscribe: None,
            rejection_shape: RejectionShape::Structured,
            flood_on_register: commands,
            first_register_flood: 0,
            replay_delay: Duration::ZERO,
            register_delay: Duration::ZERO,
            subscribe_delay: Duration::ZERO,
            initial_subscribe_delay: Duration::ZERO,
            drop_after_register_first: false,
            help_on_reconnect: false,
            initial_actions: HashMap::new(),
            replay_actions: HashMap::new(),
            all_actions: HashMap::new(),
        })
    }
    fn new(fail_replay_on_reconnect: bool, reject_register_on_reconnect: bool) -> Arc<Stub> {
        Self::with_rejection_shape(
            RejectionShape::Structured,
            fail_replay_on_reconnect,
            reject_register_on_reconnect,
        )
    }
    fn legacy_text(
        fail_replay_on_reconnect: bool,
        reject_register_on_reconnect: bool,
    ) -> Arc<Stub> {
        Self::with_rejection_shape(
            RejectionShape::LegacyText,
            fail_replay_on_reconnect,
            reject_register_on_reconnect,
        )
    }
    fn malformed_body(
        fail_replay_on_reconnect: bool,
        reject_register_on_reconnect: bool,
    ) -> Arc<Stub> {
        Self::with_rejection_shape(
            RejectionShape::Malformed,
            fail_replay_on_reconnect,
            reject_register_on_reconnect,
        )
    }
    fn with_rejection_shape(
        rejection_shape: RejectionShape,
        fail_replay_on_reconnect: bool,
        reject_register_on_reconnect: bool,
    ) -> Arc<Stub> {
        Arc::new(Stub {
            state: Mutex::new(StubState::default()),
            drop_conn1: Notify::new(),
            fail_replay_on_reconnect,
            reject_register_on_reconnect,
            reject_topic: None,
            reject_unsubscribe: None,
            rejection_shape,
            flood_on_register: 0,
            first_register_flood: 0,
            replay_delay: Duration::ZERO,
            register_delay: Duration::ZERO,
            subscribe_delay: Duration::ZERO,
            initial_subscribe_delay: Duration::ZERO,
            drop_after_register_first: false,
            help_on_reconnect: false,
            initial_actions: HashMap::new(),
            replay_actions: HashMap::new(),
            all_actions: HashMap::new(),
        })
    }

    fn rejecting(topic: &str) -> Arc<Stub> {
        Arc::new(Stub {
            state: Mutex::new(StubState::default()),
            drop_conn1: Notify::new(),
            fail_replay_on_reconnect: false,
            reject_register_on_reconnect: false,
            reject_topic: Some(topic.to_string()),
            reject_unsubscribe: None,
            rejection_shape: RejectionShape::Structured,
            flood_on_register: 0,
            first_register_flood: 0,
            replay_delay: Duration::ZERO,
            register_delay: Duration::ZERO,
            subscribe_delay: Duration::ZERO,
            initial_subscribe_delay: Duration::ZERO,
            drop_after_register_first: false,
            help_on_reconnect: false,
            initial_actions: HashMap::new(),
            replay_actions: HashMap::new(),
            all_actions: HashMap::new(),
        })
    }

    fn action_for(&self, name: &str, conn_index: usize) -> SubscribeAction {
        if let Some(action) = self.all_actions.get(name) {
            return action.clone();
        }
        if conn_index == 1 {
            if let Some(action) = self.initial_actions.get(name) {
                return action.clone();
            }
        } else if let Some(action) = self.replay_actions.get(name) {
            return action.clone();
        }
        if self.reject_topic.as_deref() == Some(name) {
            return SubscribeAction::Refuse(10);
        }
        if conn_index >= 2 && self.fail_replay_on_reconnect {
            return SubscribeAction::Refuse(10);
        }
        SubscribeAction::Ack
    }
}

/// A hold action plus its two barriers: `entered` fires when the stub
/// receives the subscribe, `release` lets the reply go.
fn hold_action() -> (SubscribeAction, Arc<Notify>, Arc<Notify>) {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    (
        SubscribeAction::Hold {
            entered: entered.clone(),
            release: release.clone(),
            rc: 0,
        },
        entered,
        release,
    )
}

/// A `type: response` reply correlated to `req`.
fn reply(req: &BusMessage, rc: &str) -> String {
    let mut m = BusMessage::new()
        .with_header("type", "response")
        .with_header("command", req.get("command").unwrap_or("?"))
        .with_header("from", "noded")
        .with_header("rc", rc);
    if let Some(id) = req.get("id") {
        m = m.with_header("id", id);
    }
    m.to_wire()
}

fn collision_reply(req: &BusMessage, shape: &RejectionShape) -> String {
    let mut response = bus::parse(&reply(req, "10")).expect("stub reply parses");
    // One wording everywhere on purpose: classification must come from the
    // body, never from this text.
    response.set("error", "stub collision diagnostic wording");
    match shape {
        RejectionShape::Structured => {
            response.body = serde_json::json!({
                "schema": bus::REGISTRATION_REJECTION_SCHEMA,
                "error_code": bus::REGISTRATION_REJECTION_NAME_TAKEN,
                "message": "stub collision diagnostic wording",
            })
            .to_string();
        }
        RejectionShape::LegacyText => {}
        RejectionShape::Malformed => {
            response.body = serde_json::json!({
                "schema": bus::REGISTRATION_REJECTION_SCHEMA,
                "error_code": "UNRECOGNISED_CODE",
                "message": "stub collision diagnostic wording",
            })
            .to_string();
        }
    }
    response.to_wire()
}

async fn run_stub(listener: TcpListener, stub: Arc<Stub>) {
    while let Ok((tcp, _)) = listener.accept().await {
        let stub = stub.clone();
        let conn_index = {
            let mut s = stub.state.lock().await;
            s.connections += 1;
            s.open_connections += 1;
            s.connections
        };
        tokio::spawn(async move {
            let ws = match tokio_tungstenite::accept_async(tcp).await {
                Ok(w) => w,
                Err(_) => return,
            };
            let (mut sink, mut stream) = ws.split();

            'conn: loop {
                let text = tokio::select! {
                    _ = stub.drop_conn1.notified(), if conn_index == 1 => {
                        let _ = sink.close().await;
                        break 'conn;
                    }
                    msg = stream.next() => match msg {
                        Some(Ok(Message::Text(t))) => t.to_string(),
                        Some(Ok(Message::Close(_))) | None => break 'conn,
                        Some(Ok(_)) => continue,
                        Some(Err(_)) => break 'conn,
                    },
                };

                let req = match bus::parse(&text) {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                if req.get("rc").is_some() {
                    stub.state.lock().await.responses.push(req);
                    continue;
                }
                let command = req.get("command").unwrap_or("").to_string();
                match command.as_str() {
                    "noded.register" => {
                        let from = req.get("from").unwrap_or("").to_string();
                        let forced_reject = stub.reject_register_on_reconnect && conn_index >= 2;
                        let collision = {
                            let mut s = stub.state.lock().await;
                            s.register_names.push(from.clone());
                            s.register_bodies.push(req.body.clone());
                            if forced_reject {
                                true
                            } else {
                                match s.active_registrations.get(&from) {
                                    Some(&owner) if owner != conn_index => true,
                                    _ => {
                                        s.active_registrations.insert(from.clone(), conn_index);
                                        false
                                    }
                                }
                            }
                        };
                        if !stub.register_delay.is_zero() {
                            tokio::select! {
                                _ = tokio::time::sleep(stub.register_delay) => {}
                                _ = stream.next() => break 'conn,
                            }
                        }
                        if collision {
                            // Keep the socket open, as the real broker does;
                            // the client must close its half-built
                            // connection itself.
                            let reply = collision_reply(&req, &stub.rejection_shape);
                            let _ = sink.send(Message::Text(reply.into())).await;
                            continue;
                        }
                        let _ = sink.send(Message::Text(reply(&req, "0").into())).await;
                        // A transport failure right after registration: the
                        // client's next subscribe write fails.
                        if stub.drop_after_register_first && conn_index == 1 {
                            let _ = sink.close().await;
                            break 'conn;
                        }
                        let flood = if conn_index == 1 {
                            stub.flood_on_register + stub.first_register_flood
                        } else {
                            stub.flood_on_register
                        };
                        for sequence in 0..flood {
                            let event = BusMessage::new()
                                .with_header("type", "request")
                                .with_header("command", "world.test.flood")
                                .with_header("from", "publisher")
                                .with_header("id", &format!("flood-{sequence}"))
                                .with_body(&sequence.to_string());
                            let _ = sink.send(Message::Text(event.to_wire().into())).await;
                        }
                        // After a re-register, push an unsolicited request:
                        // it must surface on the receiver taken before the
                        // drop.
                        if conn_index >= 2 {
                            let ping = BusMessage::new()
                                .with_header("type", "request")
                                .with_header("command", "world.test.ping")
                                .with_header("from", "noded")
                                .with_header("id", "ping-1")
                                .to_wire();
                            let _ = sink.send(Message::Text(ping.into())).await;
                            if stub.help_on_reconnect {
                                let help = BusMessage::new()
                                    .with_header("type", "request")
                                    .with_header("command", "HELP")
                                    .with_header("from", "noded")
                                    .with_header("id", "help-1")
                                    .to_wire();
                                let _ = sink.send(Message::Text(help.into())).await;
                            }
                        }
                    }
                    "topic.subscribe" => {
                        let name = req.get("name").unwrap_or("").to_string();
                        let action = stub.action_for(&name, conn_index);
                        stub.state.lock().await.subscribe_attempts += 1;
                        let (rc, disconnect) = match action {
                            SubscribeAction::Ack => (0, false),
                            SubscribeAction::Refuse(rc) => (rc, false),
                            SubscribeAction::Hold {
                                entered,
                                release,
                                rc,
                            } => {
                                entered.notify_one();
                                tokio::select! {
                                    _ = release.notified() => {}
                                    _ = stream.next() => break 'conn,
                                }
                                (rc, false)
                            }
                            SubscribeAction::Disconnect => (0, true),
                        };
                        if disconnect {
                            let _ = sink.close().await;
                            break 'conn;
                        }
                        let delay = if !stub.subscribe_delay.is_zero() {
                            stub.subscribe_delay
                        } else if conn_index == 1 {
                            stub.initial_subscribe_delay
                        } else {
                            Duration::ZERO
                        } + if conn_index >= 2 { stub.replay_delay } else { Duration::ZERO };
                        if !delay.is_zero() {
                            // Keep the broker's transport reader alive while
                            // withholding an ACK. A closed abandoned attempt
                            // must release its name before the next dial.
                            tokio::select! {
                                _ = tokio::time::sleep(delay) => {}
                                _ = stream.next() => break 'conn,
                            }
                        }
                        let mut response = bus::parse(&reply(&req, &rc.to_string()))
                            .expect("stub subscribe reply parses");
                        if rc != 0 {
                            response.set("error", "stub refused topic");
                        }
                        // Only a reply that actually reached a live client
                        // counts as an acknowledgement: a ghost ACK to a
                        // socket the client already abandoned must not.
                        let sent = sink.send(Message::Text(response.to_wire().into())).await;
                        if rc == 0 && sent.is_ok() {
                            stub.state.lock().await.subscribed.push(name);
                        }
                    }
                    "topic.unsubscribe" => {
                        let name = req.get("name").unwrap_or("").to_string();
                        let reject = stub.reject_topic.as_deref() == Some(name.as_str())
                            || stub.reject_unsubscribe.as_deref() == Some(name.as_str());
                        if !reject {
                            stub.state.lock().await.unsubscribed.push(name);
                        }
                        let rc = if reject { "10" } else { "0" };
                        let _ = sink.send(Message::Text(reply(&req, rc).into())).await;
                    }
                    "noded.deregister" => {
                        stub.state.lock().await.deregistered = true;
                        let _ = sink.send(Message::Text(reply(&req, "0").into())).await;
                    }
                    "echo.body" => {
                        let mut response = bus::parse(&reply(&req, "0")).unwrap();
                        response.body = req.body.clone();
                        let _ = sink.send(Message::Text(response.to_wire().into())).await;
                    }
                    _ => {
                        let _ = sink.send(Message::Text(reply(&req, "0").into())).await;
                    }
                }
            }

            // Every name this connection held is freed when its socket closes.
            let mut s = stub.state.lock().await;
            s.active_registrations
                .retain(|_, owner| *owner != conn_index);
            s.open_connections = s.open_connections.saturating_sub(1);
        });
    }
}

/// Poll `cond` until true or `secs` elapse.
async fn wait_until<F: Fn() -> bool>(secs: u64, cond: F) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    while tokio::time::Instant::now() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    cond()
}

async fn start(stub: &Arc<Stub>) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let handle = tokio::spawn(run_stub(listener, stub.clone()));
    (format!("ws://{address}/ws"), handle)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconnects_reregisters_replays_and_keeps_incoming_stream() {
    let stub = Stub::new(false, false);
    let (url, _acceptor) = start(&stub).await;
    let client = SupervisedClient::connect("statecache", &url)
        .await
        .expect("initial connect");
    assert_eq!(client.state(), ConnState::Connected);
    assert_eq!(client.connection_generation(), 1);
    let mut states = client.subscribe_state();

    // Taken before the bounce: it must survive the reconnect.
    let mut incoming = client.incoming().expect("incoming taken once");

    client
        .subscribe_topic("world.statecache.probe")
        .await
        .expect("subscribe while Connected");
    {
        let s = stub.state.lock().await;
        assert_eq!(s.subscribed, vec!["world.statecache.probe".to_string()]);
        assert_eq!(s.connections, 1);
        assert_eq!(s.register_names, vec!["statecache".to_string()]);
    }

    // `notify_one` keeps a permit if the connection task is between loop
    // iterations; `notify_waiters` would lose the bounce in that window.
    stub.drop_conn1.notify_one();

    // The generation advances immediately before the Connected publication,
    // so this also proves re-registration and replay completed.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            states.changed().await.expect("state sender remains live");
            if *states.borrow_and_update() == ConnState::Connected
                && client.connection_generation() >= 2
            {
                break;
            }
        }
    })
    .await
    .expect("expected a reconnect with re-registration and replay");
    {
        let s = stub.state.lock().await;
        assert_eq!(
            s.register_names,
            vec!["statecache".to_string(), "statecache".to_string()],
            "must re-register under the same name"
        );
        assert_eq!(
            s.subscribed,
            vec![
                "world.statecache.probe".to_string(),
                "world.statecache.probe".to_string(),
            ],
            "registry must be replayed on reconnect"
        );
    }

    let got = tokio::time::timeout(Duration::from_secs(5), incoming.recv())
        .await
        .expect("incoming stream must survive the reconnect")
        .expect("a command, not channel close");
    assert_eq!(got.command, "world.test.ping");
    assert_eq!(got.from, "noded");
    assert_eq!(got.id.as_deref(), Some("ping-1"));

    let r = client
        .call_with_headers(
            "noded",
            "topic.subscriber_count",
            &BTreeMap::from([("name".to_string(), "x".to_string())]),
            "",
        )
        .await;
    assert!(r.is_ok(), "outbound call while Connected: {r:?}");

    client.deregister().await.expect("deregister");
    assert!(stub.state.lock().await.deregistered);
    assert_eq!(client.state(), ConnState::ShuttingDown);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn call_returns_the_body_as_json_string_or_null() {
    let stub = Stub::new(false, false);
    let (url, _acceptor) = start(&stub).await;
    let client = SupervisedClient::connect("caller", &url).await.unwrap();
    let value = client
        .call("peer", "echo.body", serde_json::json!({"n": 1}))
        .await
        .unwrap();
    assert_eq!(value, serde_json::json!({"n": 1}));
    let text = client
        .call_with_headers("peer", "echo.body", &BTreeMap::new(), "plain text")
        .await
        .unwrap();
    assert_eq!(text, serde_json::Value::String("plain text".into()));
    let none = client
        .call("peer", "echo.body", serde_json::Value::Null)
        .await
        .unwrap();
    assert_eq!(none, serde_json::Value::Null);
    let (rc, body, error) = client
        .call_with_headers_raw("peer", "echo.body", &BTreeMap::new(), "{\"raw\":true}")
        .await
        .unwrap();
    assert_eq!((rc, body.as_str(), error), (0, "{\"raw\":true}", None));
    client.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn initial_connect_failure_is_typed_fatal() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener); // nothing listens here any more

    let url = format!("ws://127.0.0.1:{port}/ws");
    let err = match SupervisedClient::connect("statecache", &url).await {
        Ok(_) => panic!("connect to a dead port must fail"),
        Err(e) => e,
    };
    match err {
        SupervisedError::InitialConnectFailed { attempts, .. } => {
            assert_eq!(attempts, bus::MAX_INITIAL_ATTEMPTS);
        }
        other => panic!("expected InitialConnectFailed, got {other}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registration_rejection_preserves_rc_and_message() {
    let stub = Stub::new(false, false);
    let (url, _acceptor) = start(&stub).await;
    let owner = Connection::connect("taken-service", &url)
        .await
        .expect("first registration owns the name");
    let error = match Connection::connect("taken-service", &url).await {
        Ok(_) => panic!("duplicate registration must fail"),
        Err(error) => error,
    };
    assert_eq!(
        error.registration_rejection(),
        Some((10, "stub collision diagnostic wording"))
    );
    assert_eq!(
        error
            .registration_rejection_typed()
            .map(|rejection| rejection.kind()),
        Some(bus::RegistrationRejectionKind::NameTaken),
        "a finite connect exposes the same typed reason"
    );
    // The refused connection closed itself: only the owner is open.
    assert!(
        wait_until(5, || stub
            .state
            .try_lock()
            .map(|s| s.open_connections == 1)
            .unwrap_or(false))
        .await
    );

    // The supervised form reports the same refusal when it is fatal.
    let error = SupervisedClient::connect_options("taken-service", &url)
        .fatal_on_registration_rejection(true)
        .connect()
        .await
        .err()
        .expect("fatal rejection");
    assert_eq!(
        error.registration_rejection(),
        Some((10, "stub collision diagnostic wording"))
    );
    assert_eq!(
        error
            .registration_rejection_typed()
            .map(|rejection| rejection.kind()),
        Some(bus::RegistrationRejectionKind::NameTaken),
        "the finite supervised connect exposes the same typed reason"
    );
    owner.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn classification_comes_from_the_body_not_the_diagnostic_wording() {
    // Three brokers that word the refusal identically: only the one that
    // carries the structured v1 body classifies as a name collision.
    let structured = Stub::new(false, false);
    let (url, _acceptor) = start(&structured).await;
    let owner = Connection::connect("taken-service", &url)
        .await
        .expect("first registration owns the name");
    let error = match Connection::connect("taken-service", &url).await {
        Ok(_) => panic!("duplicate registration must fail"),
        Err(error) => error,
    };
    let rejection = error
        .registration_rejection_typed()
        .expect("a structured refusal is typed");
    assert_eq!(
        (rejection.rc, rejection.message.as_str()),
        (10, "stub collision diagnostic wording")
    );
    assert_eq!(rejection.kind(), bus::RegistrationRejectionKind::NameTaken);
    owner.close().await;

    let legacy = Stub::legacy_text(false, false);
    let (url, _acceptor) = start(&legacy).await;
    let owner = Connection::connect("taken-service", &url)
        .await
        .expect("first registration owns the name");
    let error = match Connection::connect("taken-service", &url).await {
        Ok(_) => panic!("duplicate registration must fail"),
        Err(error) => error,
    };
    let rejection = error
        .registration_rejection_typed()
        .expect("a legacy refusal is still typed as Unknown");
    assert_eq!(
        (rejection.rc, rejection.message.as_str()),
        (10, "stub collision diagnostic wording"),
        "identical wording to the structured case"
    );
    assert_eq!(
        rejection.kind(),
        bus::RegistrationRejectionKind::Unknown,
        "wording alone must never classify a collision"
    );
    owner.close().await;

    let malformed = Stub::malformed_body(false, false);
    let (url, _acceptor) = start(&malformed).await;
    let owner = Connection::connect("taken-service", &url)
        .await
        .expect("first registration owns the name");
    let error = match Connection::connect("taken-service", &url).await {
        Ok(_) => panic!("duplicate registration must fail"),
        Err(error) => error,
    };
    let rejection = error
        .registration_rejection_typed()
        .expect("a malformed refusal is still typed as Unknown");
    assert_eq!(
        rejection.kind(),
        bus::RegistrationRejectionKind::Unknown,
        "an unrecognised error_code must not classify a collision"
    );
    owner.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connection_state_watch_publishes_disconnect_and_reconnect_edges() {
    let stub = Stub::new(false, false);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let acceptor = tokio::spawn(run_stub(listener, stub.clone()));

    let url = format!("ws://{address}/ws");
    let client = SupervisedClient::connect("watched-service", &url)
        .await
        .expect("initial connect");
    let mut states = client.subscribe_state();
    assert_eq!(*states.borrow(), ConnState::Connected);

    acceptor.abort();
    stub.drop_conn1.notify_one();
    tokio::time::timeout(Duration::from_secs(5), states.changed())
        .await
        .expect("disconnect edge timeout")
        .expect("state sender remains live");
    assert_eq!(*states.borrow_and_update(), ConnState::Disconnected);

    let listener = TcpListener::bind(address)
        .await
        .expect("restart stub broker on the same address");
    tokio::spawn(run_stub(listener, stub));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            states.changed().await.expect("state sender remains live");
            if *states.borrow_and_update() == ConnState::Connected {
                break;
            }
        }
    })
    .await
    .expect("reconnect edge timeout");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outbound_while_disconnected_fails_fast_typed() {
    let stub = Stub::new(false, false);
    let (url, acceptor) = start(&stub).await;
    let client = SupervisedClient::connect("statecache", &url)
        .await
        .expect("initial connect");

    stub.drop_conn1.notify_one();
    acceptor.abort();
    assert!(
        wait_until(10, || client.state() == ConnState::Disconnected).await,
        "should settle into Disconnected with no broker"
    );

    let err = client
        .call("noded", "noded.list", serde_json::Value::Null)
        .await
        .expect_err("outbound while disconnected must error, not queue");
    assert!(matches!(err, SupervisedError::Disconnected), "got {err}");
    let err = client
        .subscribe_topic("world.late")
        .await
        .expect_err("subscribe while disconnected must error, not queue");
    assert!(matches!(err, SupervisedError::Disconnected), "got {err}");
    assert!(client.subscription_registry().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscribe_records_only_after_rc0() {
    let stub = Stub::new(false, false);
    let (url, _acceptor) = start(&stub).await;
    let client = SupervisedClient::connect("statecache", &url)
        .await
        .expect("initial connect");

    client
        .subscribe_topic("world.a")
        .await
        .expect("subscribe a");
    client
        .subscribe_topic("world.b")
        .await
        .expect("subscribe b");
    client
        .subscribe_topic("world.a")
        .await
        .expect("duplicate is a no-op");

    assert_eq!(
        client.subscription_registry().snapshot(),
        vec!["world.a".to_string(), "world.b".to_string()]
    );
    let s = stub.state.lock().await;
    assert_eq!(
        s.subscribed,
        vec![
            "world.a".to_string(),
            "world.b".to_string(),
            "world.a".to_string()
        ],
        "every subscribe reaches the broker"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejected_subscribe_leaves_registry_unchanged() {
    let stub = Stub::rejecting("reserved.x");
    let (url, _acceptor) = start(&stub).await;
    let client = SupervisedClient::connect("statecache", &url)
        .await
        .expect("initial connect");

    client
        .subscribe_topic("ok.a")
        .await
        .expect("subscribe ok.a");
    let err = client
        .subscribe_topic("reserved.x")
        .await
        .expect_err("a refused subscribe must error, not record");
    assert!(
        matches!(
            err,
            SupervisedError::Transport(bus::ClientError::Refused { rc: 10, .. })
        ),
        "broker rc=10 surfaces as a typed refusal, got {err}"
    );
    assert_eq!(
        client.subscription_registry().snapshot(),
        vec!["ok.a".to_string()]
    );
    assert_eq!(stub.state.lock().await.subscribe_attempts, 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unsubscribe_removes_only_after_rc0() {
    let stub = Stub::rejecting("keep");
    let (url, _acceptor) = start(&stub).await;
    let client = SupervisedClient::connect("statecache", &url)
        .await
        .expect("initial connect");

    client
        .subscribe_topic("world.a")
        .await
        .expect("subscribe a");
    client
        .subscribe_topic("world.b")
        .await
        .expect("subscribe b");
    client
        .unsubscribe_topic("world.a")
        .await
        .expect("unsubscribe a");
    assert_eq!(
        client.subscription_registry().snapshot(),
        vec!["world.b".to_string()]
    );
    assert_eq!(
        stub.state.lock().await.unsubscribed,
        vec!["world.a".to_string()]
    );

    // A refused unsubscribe leaves the topic recorded.
    client.subscription_registry().record("keep");
    let err = client
        .unsubscribe_topic("keep")
        .await
        .expect_err("a refused unsubscribe must error");
    assert!(matches!(err, SupervisedError::Transport(_)), "got {err}");
    assert_eq!(
        client.subscription_registry().snapshot(),
        vec!["world.b".to_string(), "keep".to_string()]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replay_failure_keeps_disconnected_no_false_connected() {
    let stub = Stub::new(true, false); // reject topic.subscribe on conn >= 2
    let (url, _acceptor) = start(&stub).await;
    let client = SupervisedClient::connect("statecache", &url)
        .await
        .expect("initial connect");
    let _incoming = client.incoming().expect("incoming taken once");
    client
        .subscription_registry()
        .record("world.statecache.probe");

    stub.drop_conn1.notify_one();

    // The supervisor must reconnect and attempt replay repeatedly, each
    // rejected, without ever flipping to Connected.
    let stub2 = stub.clone();
    assert!(
        wait_until(10, || {
            stub2
                .state
                .try_lock()
                .map(|s| s.connections >= 3 && s.subscribe_attempts >= 2)
                .unwrap_or(false)
        })
        .await,
        "supervisor must keep retrying register+replay after rejection"
    );

    assert_ne!(client.state(), ConnState::Connected);
    let s = stub.state.lock().await;
    assert!(
        s.subscribed.is_empty(),
        "no rejected subscribe may count: {:?}",
        s.subscribed
    );
    assert!(s.register_names.len() >= 2);
    assert!(
        s.open_connections <= 1,
        "failed-replay reconnects leaked sockets: {} open of {} total",
        s.open_connections,
        s.connections
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconnect_registration_rejection_retries_by_default_without_leak() {
    let stub = Stub::new(false, true); // reject noded.register on conn >= 2
    let (url, _acceptor) = start(&stub).await;
    let client = SupervisedClient::connect("statecache", &url)
        .await
        .expect("initial connect registers fine");
    let _incoming = client.incoming().expect("incoming taken once");

    stub.drop_conn1.notify_one();

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if stub.state.lock().await.register_names.len() >= 3 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("default policy retries rejected registrations");
    assert_ne!(client.state(), ConnState::Fatal);

    tokio::time::sleep(Duration::from_millis(300)).await;
    let s = stub.state.lock().await;
    assert!(s.connections >= 3);
    assert!(s.register_names.iter().all(|name| name == "statecache"));
    assert!(
        s.open_connections <= 1,
        "failed-register reconnects leaked sockets: {} open of {} total",
        s.open_connections,
        s.connections
    );
    drop(s);
    client.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconnect_registration_rejection_is_terminal_when_opted_in() {
    let stub = Stub::new(false, true);
    let (url, _acceptor) = start(&stub).await;
    let client = SupervisedClient::connect_options("statecache", &url)
        .fatal_on_registration_rejection(true)
        .connect()
        .await
        .expect("initial connect registers fine");
    let _incoming = client.incoming().expect("incoming taken once");
    let mut states = client.subscribe_state();

    stub.drop_conn1.notify_one();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            states.changed().await.expect("state sender remains live");
            if *states.borrow_and_update() == ConnState::Fatal {
                break;
            }
        }
    })
    .await
    .expect("opt-in rejection policy publishes a terminal state");

    let rejection = client
        .registration_rejection()
        .expect("Fatal is observed only after the exact refusal is sampleable");
    assert_eq!(
        (rejection.rc, rejection.message.as_str()),
        (10, "stub collision diagnostic wording")
    );
    assert_eq!(
        rejection.kind(),
        bus::RegistrationRejectionKind::NameTaken,
        "the supervised reconnect rejection carries the typed reason"
    );

    tokio::time::sleep(Duration::from_millis(300)).await;
    let s = stub.state.lock().await;
    assert_eq!(s.connections, 2, "opt-in rejection must not be retried");
    assert!(s.open_connections <= 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cold_start_is_prompt_generation_zero_and_all_outbound_paths_fail_fast() {
    let reserved = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/ws", reserved.local_addr().unwrap());
    drop(reserved);
    let began = std::time::Instant::now();
    let client = SupervisedClient::connect_options("cold", &url)
        .bounded_incoming(2)
        .start();
    assert!(began.elapsed() < Duration::from_millis(100));
    assert_eq!(client.state(), ConnState::Connecting);
    assert_eq!(client.connection_generation(), 0);
    assert!(client.incoming().is_none());
    let mut incoming = client.incoming_bounded().unwrap();
    assert!(client.incoming_bounded().is_none());
    assert!(matches!(
        client
            .call("noded", "noded.list", serde_json::Value::Null)
            .await,
        Err(SupervisedError::Disconnected)
    ));
    assert!(matches!(
        client.subscribe_topic("world.cold").await,
        Err(SupervisedError::Disconnected)
    ));
    assert!(matches!(
        client
            .respond_parts_shutdown_synth(0, "caller", "ping", Some("1"), 16, "stop")
            .await,
        Err(SupervisedError::Disconnected)
    ));
    assert!(client.registration_rejection().is_none());
    assert!(client.subscription_registry().is_empty());
    tokio::time::timeout(Duration::from_secs(1), client.close())
        .await
        .unwrap();
    assert_eq!(client.state(), ConnState::ShuttingDown);
    assert!(incoming.recv().await.is_none());
    client.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cold_start_survives_beyond_finite_budget_then_replays_on_the_same_receiver() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/ws", listener.local_addr().unwrap());
    let stub = Stub::flooding(1);
    let rejected = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let attempts = rejected.clone();
    let broker = stub.clone();
    let acceptor = tokio::spawn(async move {
        // Real TCP/WebSocket dial failures, rather than an application retry
        // seam. Keep this same listener and let the broker become available
        // only after the legacy initial budget would have been exhausted.
        for _ in 0..=bus::MAX_INITIAL_ATTEMPTS {
            let (socket, _) = listener.accept().await.unwrap();
            drop(socket);
            attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        run_stub(listener, broker).await;
    });
    let client = SupervisedClient::connect_options("cold-recovery", &url)
        .bounded_incoming(8)
        .start();
    let mut incoming = client.incoming_bounded().unwrap();
    client
        .subscription_registry()
        .record("world.before-registration");
    assert!(
        wait_until(90, || rejected.load(std::sync::atomic::Ordering::SeqCst)
            > bus::MAX_INITIAL_ATTEMPTS)
        .await
    );
    assert!(wait_until(90, || client.is_connected()).await);
    assert_eq!(client.connection_generation(), 1);
    let first = incoming.recv().await.unwrap();
    let BoundedIncomingEvent::Command(first) = first else {
        panic!("unexpected overflow")
    };
    assert_eq!(first.generation, 1);
    assert_eq!(
        stub.state.lock().await.subscribed,
        ["world.before-registration"]
    );
    client
        .subscribe_topic("world.after-registration")
        .await
        .unwrap();
    stub.drop_conn1.notify_one();
    assert!(wait_until(10, || client.connection_generation() == 2).await);
    let next = incoming.recv().await.unwrap();
    let BoundedIncomingEvent::Command(next) = next else {
        panic!("unexpected overflow")
    };
    assert_eq!(next.generation, 2);
    assert_eq!(
        stub.state.lock().await.subscribed,
        [
            "world.before-registration",
            "world.after-registration",
            "world.before-registration",
            "world.after-registration",
        ]
    );
    client.close().await;
    assert!(
        wait_until(5, || stub
            .state
            .try_lock()
            .map(|s| s.active_registrations.is_empty() && s.open_connections == 0)
            .unwrap_or(false))
        .await
    );
    acceptor.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_connect_still_exhausts_its_initial_attempt_budget() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/ws", listener.local_addr().unwrap());
    let refused = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let attempts = refused.clone();
    let acceptor = tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            drop(socket);
            attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    });
    let error = SupervisedClient::connect("finite", &url)
        .await
        .err()
        .unwrap();
    assert!(matches!(
        error,
        SupervisedError::InitialConnectFailed {
            attempts: bus::MAX_INITIAL_ATTEMPTS,
            ..
        }
    ));
    assert_eq!(
        refused.load(std::sync::atomic::Ordering::SeqCst),
        bus::MAX_INITIAL_ATTEMPTS
    );
    acceptor.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cold_registration_refusal_is_exact_sampleable_and_terminal() {
    let stub = Stub::new(false, false);
    let (url, acceptor) = start(&stub).await;
    let owner = Connection::connect("held", &url).await.unwrap();
    let client = SupervisedClient::connect_options("held", &url)
        .fatal_on_registration_rejection(true)
        .start();
    let mut incoming = client.incoming().unwrap();
    let mut states = client.subscribe_state();
    tokio::time::timeout(Duration::from_secs(5), async {
        while *states.borrow_and_update() != ConnState::Fatal {
            states.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    let rejection = client.registration_rejection().expect("rejection");
    assert_eq!(
        (rejection.rc, rejection.message.as_str()),
        (10, "stub collision diagnostic wording")
    );
    assert_eq!(
        rejection.kind(),
        bus::RegistrationRejectionKind::NameTaken,
        "a nonblocking start exposes the same typed reason as a finite connect"
    );
    assert_eq!(
        client.registration_rejection(),
        Some(rejection),
        "sampling is non-consuming"
    );
    assert_eq!(client.connection_generation(), 0);
    assert!(incoming.recv().await.is_none());
    owner.close().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(client.state(), ConnState::Fatal);
    assert_eq!(
        stub.state.lock().await.connections,
        2,
        "Fatal must never resume dialing"
    );
    client.close().await;
    assert_eq!(client.state(), ConnState::Fatal);
    acceptor.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cold_shutdown_deregister_and_drop_are_safe_before_any_socket() {
    let reserved = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/ws", reserved.local_addr().unwrap());
    drop(reserved);
    let client = SupervisedClient::connect_options("no-socket", &url).start();
    let mut incoming = client.incoming().unwrap();
    assert!(matches!(
        client.deregister_for_drain().await,
        Err(SupervisedError::Disconnected)
    ));
    tokio::time::timeout(Duration::from_secs(1), incoming.recv())
        .await
        .unwrap();
    client.close().await;
    let client = SupervisedClient::connect_options("no-socket", &url).start();
    let mut incoming = client.incoming().unwrap();
    client.shutdown().await;
    assert!(incoming.recv().await.is_none());
    let client = SupervisedClient::connect_options("no-socket", &url).start();
    let mut incoming = client.incoming().unwrap();
    let states = client.subscribe_state();
    drop(client);
    assert_eq!(*states.borrow(), ConnState::ShuttingDown);
    assert!(
        tokio::time::timeout(Duration::from_secs(1), incoming.recv())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closing_or_dropping_during_initial_registration_cannot_publish_or_leak() {
    for drop_handle in [false, true] {
        let mut delayed = Arc::try_unwrap(Stub::new(false, false)).ok().unwrap();
        delayed.register_delay = Duration::from_millis(400);
        let stub = Arc::new(delayed);
        let (url, acceptor) = start(&stub).await;
        let client = SupervisedClient::connect_options("initial-cancel", &url).start();
        let _incoming = client.incoming().unwrap();
        let states = client.subscribe_state();
        assert!(
            wait_until(5, || stub
                .state
                .try_lock()
                .map(|s| s.register_names.len() == 1)
                .unwrap_or(false))
            .await
        );
        if drop_handle {
            drop(client);
        } else {
            tokio::time::timeout(Duration::from_secs(1), client.close())
                .await
                .unwrap();
            assert_eq!(client.connection_generation(), 0);
            drop(client);
        }
        assert_eq!(*states.borrow(), ConnState::ShuttingDown);
        assert!(
            wait_until(5, || stub
                .state
                .try_lock()
                .map(|s| s.active_registrations.is_empty() && s.open_connections == 0)
                .unwrap_or(false))
            .await
        );
        assert_eq!(stub.state.lock().await.connections, 1);
        acceptor.abort();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_cancels_unpublished_subscription_replay() {
    let mut delayed = Arc::try_unwrap(Stub::new(false, false)).ok().unwrap();
    delayed.replay_delay = Duration::from_secs(5);
    let stub = Arc::new(delayed);
    let (url, _acceptor) = start(&stub).await;
    let client = SupervisedClient::connect("replay-cancel", &url)
        .await
        .unwrap();
    let _incoming = client.incoming().unwrap();
    client.subscribe_topic("world.slow").await.unwrap();
    stub.drop_conn1.notify_one();
    assert!(
        wait_until(5, || stub
            .state
            .try_lock()
            .map(|s| s.subscribe_attempts == 2)
            .unwrap_or(false))
        .await
    );
    tokio::time::timeout(Duration::from_secs(1), client.close())
        .await
        .expect("shutdown must cancel the replay RPC rather than wait for its acknowledgement");
    assert_eq!(client.state(), ConnState::ShuttingDown);
    assert_eq!(stub.state.lock().await.connections, 2);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(stub.state.lock().await.connections, 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deregistered_service_drains_terminal_reply_then_closes_transport() {
    let stub = Stub::flooding(1);
    let (url, _acceptor) = start(&stub).await;
    let client = SupervisedClient::connect("draining", &url).await.unwrap();
    let mut incoming = client.incoming().unwrap();
    let command = tokio::time::timeout(Duration::from_secs(5), incoming.recv())
        .await
        .unwrap()
        .unwrap();
    client.deregister_for_drain().await.unwrap();
    assert_eq!(client.state(), ConnState::ShuttingDown);
    assert!(stub.state.lock().await.deregistered);
    assert_eq!(stub.state.lock().await.open_connections, 1);
    assert!(matches!(
        client.respond(&command, 0, "ordinary").await,
        Err(SupervisedError::ShuttingDown)
    ));
    client
        .respond_parts_shutdown_synth(
            command.generation,
            &command.from,
            &command.command,
            command.id.as_deref(),
            16,
            "cancelled",
        )
        .await
        .unwrap();
    assert!(
        wait_until(5, || stub
            .state
            .try_lock()
            .map(|s| s.responses.len() == 1)
            .unwrap_or(false))
        .await
    );
    {
        let state = stub.state.lock().await;
        let reply = &state.responses[0];
        assert_eq!(reply.get("id"), command.id.as_deref());
        assert_eq!(reply.get("to"), Some(command.from.as_str()));
        assert_eq!(reply.get("rc"), Some("16"));
        assert_eq!(reply.body, "cancelled");
    }
    client.close().await;
    assert!(
        wait_until(5, || stub
            .state
            .try_lock()
            .map(|s| s.open_connections == 0)
            .unwrap_or(false))
        .await
    );
    assert!(
        client
            .respond_parts_shutdown_synth(
                command.generation,
                &command.from,
                &command.command,
                command.id.as_deref(),
                16,
                "late"
            )
            .await
            .is_err()
    );
    assert_eq!(stub.state.lock().await.connections, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_frees_the_name_and_respond_parts_frames_a_response() {
    let stub = Stub::new(false, false);
    let (url, _acceptor) = start(&stub).await;
    let client = SupervisedClient::connect("responder", &url).await.unwrap();
    client
        .respond_parts(
            client.connection_generation(),
            "caller",
            "responder.ping",
            Some("7"),
            0,
            "{\"pong\":true}",
        )
        .await
        .expect("a response is a plain send");
    client.close().await;
    assert_eq!(client.state(), ConnState::ShuttingDown);
    assert!(
        wait_until(5, || {
            stub.state
                .try_lock()
                .map(|s| s.active_registrations.is_empty() && s.open_connections == 0)
                .unwrap_or(false)
        })
        .await,
        "close must let the broker reap the name"
    );
    // After close every outbound call is refused as shutting down.
    let err = client
        .call("noded", "noded.list", serde_json::Value::Null)
        .await
        .unwrap_err();
    assert!(matches!(err, SupervisedError::ShuttingDown), "got {err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replies_retain_the_generation_that_delivered_the_request() {
    let stub = Stub::flooding(1);
    let (url, _acceptor) = start(&stub).await;
    let client = SupervisedClient::connect("responder", &url).await.unwrap();
    let mut incoming = client.incoming().unwrap();
    let old = tokio::time::timeout(Duration::from_secs(5), incoming.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(old.generation, 1);
    stub.drop_conn1.notify_one();
    let fresh = tokio::time::timeout(Duration::from_secs(5), incoming.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(fresh.generation > old.generation);
    assert!(matches!(
        client.respond(&old, 0, "stale").await,
        Err(SupervisedError::Disconnected)
    ));
    client.respond(&fresh, 0, "fresh").await.unwrap();
    client.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropped_incoming_consumer_publishes_terminal_and_frees_registration() {
    let stub = Stub::new(false, false);
    let (url, _acceptor) = start(&stub).await;
    let client = SupervisedClient::connect("responder", &url).await.unwrap();
    drop(client.incoming().unwrap());
    assert!(wait_until(5, || client.state() == ConnState::ShuttingDown).await);
    assert!(
        wait_until(5, || stub
            .state
            .try_lock()
            .map(|s| s.open_connections == 0 && s.active_registrations.is_empty())
            .unwrap_or(false))
        .await
    );
    assert!(matches!(
        client
            .call("noded", "noded.list", serde_json::Value::Null)
            .await,
        Err(SupervisedError::ShuttingDown)
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropped_quiet_bounded_consumer_frees_registration() {
    let stub = Stub::new(false, false);
    let (url, _acceptor) = start(&stub).await;
    let client = SupervisedClient::connect_options("bounded-responder", &url)
        .bounded_incoming(2)
        .connect()
        .await
        .unwrap();
    drop(client.incoming_bounded().unwrap());
    assert!(wait_until(5, || client.state() == ConnState::ShuttingDown).await);
    assert!(
        wait_until(5, || stub
            .state
            .try_lock()
            .map(|s| s.open_connections == 0 && s.active_registrations.is_empty())
            .unwrap_or(false))
        .await
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bounded_supervised_lane_reports_socket_reader_overflow_without_growing() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let stub = Stub::flooding(128);
    tokio::spawn(run_stub(listener, stub));

    let url = format!("ws://127.0.0.1:{port}/ws");
    let client = SupervisedClient::connect_options("bounded-consumer", &url)
        .bounded_incoming(2)
        .connect()
        .await
        .expect("initial bounded connect");
    assert!(
        client.incoming().is_none(),
        "bounded opt-in must not expose the default unbounded receiver"
    );
    let mut incoming = client
        .incoming_bounded()
        .expect("bounded receiver is available exactly once");

    let dropped = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match incoming.recv().await {
                Some(BoundedIncomingEvent::Overflow { dropped }) => break dropped,
                Some(BoundedIncomingEvent::Command(_)) => {}
                None => panic!("bounded lane closed before reporting overflow"),
            }
        }
    })
    .await
    .expect("flood must produce an observable overflow marker");
    assert!(dropped > 0);
    assert!(incoming.overflow_count() >= dropped);

    client.close().await;
}

/// Version-discovery contract: provenance passed to
/// `connect_supervised_with_provenance` is sent on the INITIAL register
/// AND re-sent on every reconnect (built once, cloned from SupervisorCtx).
/// A regression that sent it only on the first connect would leave a
/// reconnected citizen provenance-less in `noded.list`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconnect_resends_provenance() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let stub = Stub::new(false, false);
    tokio::spawn(run_stub(listener, stub.clone()));

    let url = format!("ws://127.0.0.1:{port}/ws");
    let prov = bus::RegisterProvenance::from_parts(
        "mix",
        "9.9.9-test",
        "deadbeefcafe",
        false,
        "2026-01-01T00:00:00Z",
        "2026-01-01T00:00:00Z".to_string(),
    );
    let client =
        SupervisedClient::connect_supervised_with_provenance("statecache", &url, Some(prov))
            .await
            .expect("initial connect");
    assert_eq!(client.state(), ConnState::Connected);

    // Initial register carried the provenance body.
    {
        let s = stub.state.lock().await;
        assert_eq!(s.register_bodies.len(), 1);
        assert!(
            s.register_bodies[0].contains("9.9.9-test"),
            "initial register must carry provenance: {:?}",
            s.register_bodies[0]
        );
    }

    // Bounce → reconnect → the SECOND register must carry it too.
    stub.drop_conn1.notify_one();
    let stub2 = stub.clone();
    assert!(
        wait_until(10, || {
            stub2
                .state
                .try_lock()
                .map(|s| s.register_bodies.len() >= 2)
                .unwrap_or(false)
        })
        .await,
        "expected a reconnect re-register"
    );
    {
        let s = stub.state.lock().await;
        assert!(
            s.register_bodies[1].contains("9.9.9-test"),
            "reconnect register must RE-SEND provenance: {:?}",
            s.register_bodies[1]
        );
    }
    // (No deregister: the contract under test — provenance re-sent on
    // reconnect — is already asserted; the client drops at scope end.)
}

/// Wait for the first fully established generation on `client`.
async fn wait_connected(client: &SupervisedClient, secs: u64) {
    assert!(
        wait_until(secs, || client.is_connected()).await,
        "expected Connected within {secs}s"
    );
}

/// Wait for the stub's rc-0 `subscribed` list to reach `expected` exactly.
/// The stub records an acknowledgement just after its reply write, so it can
/// lag the client's publication by a schedule quantum.
async fn wait_subscribed(stub: &Arc<Stub>, expected: &[&str], secs: u64) {
    let expected: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
    assert!(
        wait_until(secs, || stub
            .state
            .try_lock()
            .map(|s| s.subscribed == expected)
            .unwrap_or(false))
        .await,
        "expected subscribed to become {expected:?}"
    );
}

/// Wait for a `Fatal` edge on `states`.
async fn wait_fatal(states: &mut tokio::sync::watch::Receiver<ConnState>, secs: u64) {
    tokio::time::timeout(Duration::from_secs(secs), async {
        while *states.borrow_and_update() != ConnState::Fatal {
            states.changed().await.expect("state sender remains live");
        }
    })
    .await
    .expect("expected a terminal Fatal edge");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn declared_start_holds_until_all_acked_then_publishes_once() {
    let mut stub0 = Arc::try_unwrap(Stub::new(false, false)).ok().unwrap();
    stub0.flood_on_register = 1;
    let (action, entered, release) = hold_action();
    stub0.initial_actions.insert("decl.two".to_string(), action);
    let stub = Arc::new(stub0);
    let (url, _acceptor) = start(&stub).await;

    let client = SupervisedClient::connect_options("declared", &url)
        .with_initial_topics(vec!["decl.one".to_string(), "decl.two".to_string()])
        .start();
    // The original receiver, taken before establishment completes.
    let mut incoming = client.incoming().expect("incoming taken once");

    // Second ACK held: Connecting, generation zero, empty registry, all
    // outbound APIs fail fast, and no delivery has gone out.
    entered.notified().await;
    assert_eq!(client.state(), ConnState::Connecting);
    assert_eq!(client.connection_generation(), 0);
    assert!(client.subscription_registry().is_empty());
    assert!(matches!(
        client
            .call("noded", "noded.list", serde_json::Value::Null)
            .await,
        Err(SupervisedError::Disconnected)
    ));
    assert!(matches!(
        client.subscribe_topic("world.late").await,
        Err(SupervisedError::Disconnected)
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(200), incoming.recv())
            .await
            .is_err(),
        "no delivery may go out before the generation is published"
    );
    {
        let s = stub.state.lock().await;
        assert_eq!(s.subscribed, vec!["decl.one".to_string()]);
        assert_eq!(s.connections, 1);
    }

    release.notify_one();
    wait_connected(&client, 5).await;
    assert_eq!(client.connection_generation(), 1);
    assert_eq!(
        client.subscription_registry().snapshot(),
        vec!["decl.one".to_string(), "decl.two".to_string()]
    );
    wait_subscribed(&stub, &["decl.one", "decl.two"], 5).await;
    assert_eq!(
        stub.state.lock().await.connections,
        1,
        "one subscribe per declaration, in declaration order, on one socket"
    );
    // The flooded command waited in the native lane and is forwarded with
    // the published generation on the original receiver.
    let got = tokio::time::timeout(Duration::from_secs(5), incoming.recv())
        .await
        .expect("queued delivery forwards after publication")
        .expect("a command, not channel close");
    assert_eq!(got.command, "world.test.flood");
    assert_eq!(got.generation, 1);
    client.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn finite_connect_waits_for_all_declarations() {
    let mut stub0 = Arc::try_unwrap(Stub::new(false, false)).ok().unwrap();
    let (action, entered, release) = hold_action();
    stub0.initial_actions.insert("decl.two".to_string(), action);
    let stub = Arc::new(stub0);
    let (url, _acceptor) = start(&stub).await;

    let (result_tx, mut result_rx) = oneshot::channel();
    let connect = tokio::spawn(async move {
        let result = SupervisedClient::connect_options("finite-hold", &url)
            .with_initial_topics(vec!["decl.one".to_string(), "decl.two".to_string()])
            .connect()
            .await;
        let _ = result_tx.send(result);
    });
    entered.notified().await;
    assert!(
        tokio::time::timeout(Duration::from_millis(200), &mut result_rx).await.is_err(),
        "connect must not return while an ACK is held"
    );

    release.notify_one();
    let client = result_rx
        .await
        .expect("the finite connect reports its result")
        .expect("establishment succeeds");
    assert_eq!(client.connection_generation(), 1);
    assert_eq!(client.state(), ConnState::Connected);
    assert_eq!(
        client.subscription_registry().snapshot(),
        vec!["decl.one".to_string(), "decl.two".to_string()]
    );
    client.close().await;
    connect.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn finite_deadline_exhausts_the_budget_with_a_typed_timeout() {
    // The broker ACKs registration but withholds every subscribe reply
    // beyond the configured attempt deadline: five timed-out attempts, then
    // InitialConnectFailed whose source is a Timeout, never a refusal.
    let mut stub0 = Arc::try_unwrap(Stub::new(false, false)).ok().unwrap();
    stub0.subscribe_delay = Duration::from_secs(2);
    let stub = Arc::new(stub0);
    let (url, _acceptor) = start(&stub).await;

    let error = SupervisedClient::connect_options("five-timeouts", &url)
        .with_initial_topics(vec!["decl.slow".to_string()])
        .establishment_timeout(Duration::from_millis(300))
        .connect()
        .await
        .err()
        .expect("the finite budget must be exhausted");
    match error {
        SupervisedError::InitialConnectFailed { attempts, source } => {
            assert_eq!(attempts, bus::MAX_INITIAL_ATTEMPTS);
            assert!(
                matches!(source, bus::ClientError::Timeout { ref to } if to == "noded"),
                "a deadline is a typed Timeout, got {source}"
            );
        }
        other => panic!("expected InitialConnectFailed, got {other}"),
    }
    assert_eq!(
        stub.state.lock().await.connections,
        bus::MAX_INITIAL_ATTEMPTS as usize,
        "exactly the finite budget of dials"
    );
    assert!(
        wait_until(10, || stub
            .state
            .try_lock()
            .map(|s| s.open_connections == 0)
            .unwrap_or(false))
        .await,
        "every timed-out attempt must close its socket"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_attempt_disconnect_then_retry_sends_both_in_order() {
    // The first declaration ACKs, the second disconnects the socket: the
    // whole attempt fails (native Closed), the registry and generation stay
    // untouched, and the retry re-sends both declarations in order.
    let mut stub0 = Arc::try_unwrap(Stub::new(false, false)).ok().unwrap();
    stub0
        .initial_actions
        .insert("decl.two".to_string(), SubscribeAction::Disconnect);
    let stub = Arc::new(stub0);
    let (url, _acceptor) = start(&stub).await;

    let client = SupervisedClient::connect_options("disconnect-mid", &url)
        .with_initial_topics(vec!["decl.one".to_string(), "decl.two".to_string()])
        .start();
    wait_connected(&client, 10).await;
    assert_eq!(client.connection_generation(), 1);
    assert_eq!(
        client.subscription_registry().snapshot(),
        vec!["decl.one".to_string(), "decl.two".to_string()]
    );
    assert_eq!(
        stub.state.lock().await.connections,
        2,
        "the failed attempt is retried once"
    );
    wait_subscribed(&stub, &["decl.one", "decl.one", "decl.two"], 5).await;
    client.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_failure_after_register_fails_the_attempt_and_recovers() {
    // The socket drops right after registration: the first subscribe write
    // fails (typed Send), and the retry establishes on a fresh socket.
    let mut stub0 = Arc::try_unwrap(Stub::new(false, false)).ok().unwrap();
    stub0.drop_after_register_first = true;
    let stub = Arc::new(stub0);
    let (url, _acceptor) = start(&stub).await;

    let client = SupervisedClient::connect_options("send-failure", &url)
        .with_initial_topics(vec!["decl.one".to_string(), "decl.two".to_string()])
        .start();
    wait_connected(&client, 10).await;
    assert_eq!(client.connection_generation(), 1);
    assert_eq!(
        stub.state.lock().await.connections,
        2,
        "the retry re-sent both declarations on a fresh socket"
    );
    wait_subscribed(&stub, &["decl.one", "decl.two"], 5).await;
    client.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attempt_deadline_closes_the_still_open_socket_and_recovers() {
    // Registration ACKs but the first subscribe reply never arrives within
    // the attempt deadline while the socket stays open. The supervisor
    // deadline (not the native 60s response timeout) closes that socket,
    // releases the name, backs off and succeeds on the same outward
    // receiver. No failed attempt is externally visible.
    let mut stub0 = Arc::try_unwrap(Stub::new(false, false)).ok().unwrap();
    stub0.initial_subscribe_delay = Duration::from_secs(2);
    let stub = Arc::new(stub0);
    let (url, _acceptor) = start(&stub).await;

    let client = SupervisedClient::connect_options("deadline-recovery", &url)
        .with_initial_topics(vec!["decl.slow".to_string()])
        .establishment_timeout(Duration::from_millis(500))
        .start();
    let mut incoming = client.incoming().expect("incoming taken once");

    assert!(
        wait_until(5, || stub
            .state
            .try_lock()
            .map(|s| s.connections >= 1)
            .unwrap_or(false))
        .await
    );
    assert_eq!(client.state(), ConnState::Connecting);
    assert_eq!(client.connection_generation(), 0);

    wait_connected(&client, 10).await;
    assert_eq!(client.connection_generation(), 1);
    wait_subscribed(&stub, &["decl.slow"], 5).await;
    assert!(
        wait_until(10, || stub
            .state
            .try_lock()
            .map(|s| s.connections == 2 && s.open_connections == 1)
            .unwrap_or(false))
        .await,
        "the deadline must close the still-open first socket while the second lives"
    );
    // The same outward receiver survives: the reconnect ping arrives with
    // the published generation.
    let got = tokio::time::timeout(Duration::from_secs(5), incoming.recv())
        .await
        .expect("the outward receiver survives the failed attempt")
        .expect("a command, not channel close");
    assert_eq!(got.command, "world.test.ping");
    assert_eq!(got.generation, 1);
    client.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn declared_refusal_is_exact_terminal_fatal_and_never_retried() {
    let mut stub0 = Arc::try_unwrap(Stub::new(false, false)).ok().unwrap();
    stub0
        .all_actions
        .insert("decl.bad".to_string(), SubscribeAction::Refuse(10));
    let stub = Arc::new(stub0);
    let (url, _acceptor) = start(&stub).await;

    let client = SupervisedClient::connect_options("decl-refused", &url)
        .with_initial_topics(vec!["decl.ok".to_string(), "decl.bad".to_string()])
        .start();
    let mut incoming = client.incoming().expect("incoming taken once");
    let mut states = client.subscribe_state();
    wait_fatal(&mut states, 5).await;

    // The diagnostic is sampleable at the Fatal edge, exact and structured.
    let diagnostic = client
        .subscription_declaration_error()
        .expect("Fatal is observed only after the diagnostic is sampleable");
    assert_eq!(
        diagnostic,
        bus::SubscriptionDeclarationError::Rejected {
            topic: "decl.bad".to_string(),
            rc: 10,
            message: "stub refused topic".to_string(),
        }
    );
    assert!(
        client.registration_rejection().is_none(),
        "a subscription refusal never manufactures a registration rejection"
    );
    assert_eq!(client.connection_generation(), 0);
    let end = tokio::time::timeout(Duration::from_secs(2), incoming.recv())
        .await
        .expect("the incoming lane closes promptly after Fatal");
    assert!(end.is_none());
    tokio::time::sleep(Duration::from_millis(300)).await;
    let s = stub.state.lock().await;
    assert_eq!(s.connections, 1, "an explicit refusal is never retried");
    assert_eq!(s.open_connections, 0, "every socket is released");
    assert_eq!(s.subscribed, vec!["decl.ok".to_string()]);
    client.close().await;
    assert_eq!(client.state(), ConnState::Fatal, "Fatal is sticky");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn declared_warning_rc_is_a_refusal_not_an_ack() {
    let mut stub0 = Arc::try_unwrap(Stub::new(false, false)).ok().unwrap();
    stub0
        .all_actions
        .insert("decl.warn".to_string(), SubscribeAction::Refuse(5));
    let stub = Arc::new(stub0);
    let (url, _acceptor) = start(&stub).await;

    let client = SupervisedClient::connect_options("decl-warn", &url)
        .with_initial_topics(vec!["decl.warn".to_string()])
        .start();
    let mut states = client.subscribe_state();
    wait_fatal(&mut states, 5).await;
    let diagnostic = client.subscription_declaration_error().unwrap();
    assert_eq!(
        diagnostic,
        bus::SubscriptionDeclarationError::Rejected {
            topic: "decl.warn".to_string(),
            rc: 5,
            message: "stub refused topic".to_string(),
        },
        "a nonzero warning response is not an exact ACK"
    );
    assert_eq!(client.connection_generation(), 0);
    client.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn finite_declared_refusal_is_a_typed_error() {
    let mut stub0 = Arc::try_unwrap(Stub::new(false, false)).ok().unwrap();
    stub0
        .all_actions
        .insert("decl.bad".to_string(), SubscribeAction::Refuse(10));
    let stub = Arc::new(stub0);
    let (url, _acceptor) = start(&stub).await;

    let error = SupervisedClient::connect_options("finite-refused", &url)
        .with_initial_topics(vec!["decl.bad".to_string()])
        .connect()
        .await
        .err()
        .expect("a refused declaration is an error");
    match error {
        SupervisedError::SubscriptionDeclaration(rejection) => {
            assert_eq!(
                rejection,
                bus::SubscriptionDeclarationError::Rejected {
                    topic: "decl.bad".to_string(),
                    rc: 10,
                    message: "stub refused topic".to_string(),
                }
            );
        }
        other => panic!("expected SubscriptionDeclaration, got {other}"),
    }
    assert_eq!(
        stub.state.lock().await.connections,
        1,
        "a refused declaration is never retried"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconnect_replays_declared_then_dynamic_in_recorded_order() {
    let stub = Stub::new(false, false);
    let (url, _acceptor) = start(&stub).await;
    let client = SupervisedClient::connect_options("replay-order", &url)
        .with_initial_topics(vec!["decl.a".to_string(), "decl.b".to_string()])
        .connect()
        .await
        .expect("initial establish");
    client
        .subscribe_topic("dyn.c")
        .await
        .expect("dynamic subscribe");

    stub.drop_conn1.notify_one();
    assert!(wait_until(10, || client.connection_generation() == 2).await);
    wait_subscribed(
        &stub,
        &["decl.a", "decl.b", "dyn.c", "decl.a", "decl.b", "dyn.c"],
        5,
    )
    .await;
    client.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn declared_replay_refusal_is_terminal_but_ordinary_replay_keeps_retrying() {
    // A declared topic refused during REPLAY is terminal; the ordinary
    // replay-refusal retry contract is unchanged for non-declared topics.
    let mut stub0 = Arc::try_unwrap(Stub::new(false, false)).ok().unwrap();
    stub0
        .replay_actions
        .insert("decl.x".to_string(), SubscribeAction::Refuse(10));
    let stub = Arc::new(stub0);
    let (url, _acceptor) = start(&stub).await;
    let client = SupervisedClient::connect_options("replay-refused", &url)
        .with_initial_topics(vec!["decl.x".to_string()])
        .connect()
        .await
        .expect("initial establish");
    assert_eq!(client.connection_generation(), 1);
    let mut states = client.subscribe_state();

    stub.drop_conn1.notify_one();
    wait_fatal(&mut states, 5).await;
    let diagnostic = client.subscription_declaration_error().unwrap();
    assert_eq!(
        diagnostic,
        bus::SubscriptionDeclarationError::Rejected {
            topic: "decl.x".to_string(),
            rc: 10,
            message: "stub refused topic".to_string(),
        }
    );
    assert_eq!(
        client.connection_generation(),
        1,
        "the generation is unchanged by the terminal replay refusal"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        stub.state.lock().await.connections,
        2,
        "a declared replay refusal is never retried"
    );
    client.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordinary_replay_warning_rc_is_a_success_not_a_refusal() {
    // Legacy ordinary replay accepted any rc < 10: a warning reply to a
    // non-declared topic is a successful replay, not a refusal loop. Only
    // declared topics demand an exact rc 0.
    let mut stub0 = Arc::try_unwrap(Stub::new(false, false)).ok().unwrap();
    stub0
        .replay_actions
        .insert("dyn.warn".to_string(), SubscribeAction::Refuse(5));
    let stub = Arc::new(stub0);
    let (url, _acceptor) = start(&stub).await;

    let client = SupervisedClient::connect("warn-replay", &url)
        .await
        .unwrap();
    client
        .subscribe_topic("dyn.warn")
        .await
        .expect("dynamic subscribe");
    stub.drop_conn1.notify_one();
    assert!(wait_until(10, || client.connection_generation() == 2).await);
    assert_eq!(client.state(), ConnState::Connected);
    assert!(client.subscription_declaration_error().is_none());
    assert_eq!(
        client.subscription_registry().snapshot(),
        vec!["dyn.warn".to_string()],
        "a warning replay neither drops nor duplicates the recorded topic"
    );
    assert_eq!(
        stub.state.lock().await.connections,
        2,
        "a warning replay must not loop"
    );
    client.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn readded_declared_name_keeps_its_exact_rc0_declared_classification() {
    // A declared name that was unsubscribed and later re-added dynamically
    // retains its declared classification: on the next replay it needs an
    // exact rc 0, so a warning reply is a terminal typed refusal — ordinary
    // topics would have accepted rc 5 as success.
    let mut stub0 = Arc::try_unwrap(Stub::new(false, false)).ok().unwrap();
    stub0
        .replay_actions
        .insert("decl.x".to_string(), SubscribeAction::Refuse(5));
    let stub = Arc::new(stub0);
    let (url, _acceptor) = start(&stub).await;

    let client = SupervisedClient::connect_options("readd", &url)
        .with_initial_topics(vec!["decl.x".to_string()])
        .connect()
        .await
        .expect("initial establish");
    client
        .unsubscribe_topic("decl.x")
        .await
        .expect("unsubscribe");
    client
        .subscribe_topic("decl.x")
        .await
        .expect("re-add dynamically");
    assert_eq!(
        client.subscription_registry().snapshot(),
        vec!["decl.x".to_string()]
    );
    let mut states = client.subscribe_state();

    stub.drop_conn1.notify_one();
    wait_fatal(&mut states, 5).await;
    let diagnostic = client.subscription_declaration_error().unwrap();
    assert_eq!(
        diagnostic,
        bus::SubscriptionDeclarationError::Rejected {
            topic: "decl.x".to_string(),
            rc: 5,
            message: "stub refused topic".to_string(),
        },
        "the re-added declared name keeps its declared classification on replay"
    );
    assert!(client.registration_rejection().is_none());
    assert_eq!(client.connection_generation(), 1);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        stub.state.lock().await.connections,
        2,
        "a classified refusal is terminal, never retried"
    );
    client.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn duplicate_declarations_are_one_request_in_first_seen_order() {
    let stub = Stub::new(false, false);
    let (url, _acceptor) = start(&stub).await;
    let client = SupervisedClient::connect_options("dedup", &url)
        .with_initial_topics(vec![
            "dup.a".to_string(),
            "dup.b".to_string(),
            "dup.a".to_string(),
            "dup.c".to_string(),
        ])
        .connect()
        .await
        .expect("initial establish");
    assert_eq!(
        client.subscription_registry().snapshot(),
        vec![
            "dup.a".to_string(),
            "dup.b".to_string(),
            "dup.c".to_string()
        ]
    );
    wait_subscribed(&stub, &["dup.a", "dup.b", "dup.c"], 5).await;
    client.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_declarations_never_dial_and_are_fatal_or_typed() {
    let stub = Stub::new(false, false);
    let (url, _acceptor) = start(&stub).await;

    let cases: Vec<Vec<String>> = vec![
        vec![String::new()],
        vec!["a\rb".to_string()],
        vec!["a\nb".to_string()],
        vec!["a\0b".to_string()],
        vec!["x".repeat(1025)],
        (0..65).map(|i| format!("topic.{i}")).collect(),
        (0..64)
            .map(|i| format!("{i:03}-{}", "x".repeat(300)))
            .collect(),
    ];
    for topics in cases {
        let error = SupervisedClient::connect_options("invalid", &url)
            .with_initial_topics(topics)
            .connect()
            .await
            .err()
            .expect("invalid declarations must be a typed error");
        assert!(
            matches!(
                error,
                SupervisedError::SubscriptionDeclaration(
                    bus::SubscriptionDeclarationError::Invalid { .. }
                )
            ),
            "got {error}"
        );
    }
    // A zero establishment timeout is invalid configuration too.
    let error = SupervisedClient::connect_options("invalid", &url)
        .with_initial_topics(vec!["fine.topic".to_string()])
        .establishment_timeout(Duration::ZERO)
        .connect()
        .await
        .err()
        .expect("a zero deadline is invalid");
    assert!(matches!(
        error,
        SupervisedError::SubscriptionDeclaration(bus::SubscriptionDeclarationError::Invalid { .. })
    ));
    assert_eq!(
        stub.state.lock().await.connections,
        0,
        "invalid declarations cause zero connection attempts"
    );

    // start() yields a Fatal client with the diagnostic sampleable, the
    // incoming producer closed, and no socket opened.
    let client = SupervisedClient::connect_options("invalid", &url)
        .with_initial_topics(vec![String::new()])
        .start();
    assert_eq!(client.state(), ConnState::Fatal);
    assert_eq!(
        client.subscription_declaration_error(),
        Some(bus::SubscriptionDeclarationError::Invalid {
            index: Some(0),
            message: "topic names must not be empty".to_string(),
        })
    );
    assert!(client.incoming().unwrap().recv().await.is_none());
    let client = SupervisedClient::connect_options("invalid", &url)
        .bounded_incoming(2)
        .with_initial_topics(vec![String::new()])
        .start();
    assert!(client.incoming_bounded().unwrap().recv().await.is_none());
    assert_eq!(stub.state.lock().await.connections, 0);
    client.close().await;
    assert_eq!(client.state(), ConnState::Fatal);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn declared_unsubscribe_survives_reconnect_and_refused_unsubscribe_replays() {
    // A successful unsubscribe of a declared topic removes it permanently.
    let stub = Stub::new(false, false);
    let (url, _acceptor) = start(&stub).await;
    let client = SupervisedClient::connect_options("unsub-ok", &url)
        .with_initial_topics(vec!["decl.keep".to_string(), "decl.drop".to_string()])
        .connect()
        .await
        .expect("initial establish");
    client
        .unsubscribe_topic("decl.drop")
        .await
        .expect("unsubscribe");
    assert_eq!(
        client.subscription_registry().snapshot(),
        vec!["decl.keep".to_string()]
    );
    stub.drop_conn1.notify_one();
    assert!(wait_until(10, || client.connection_generation() == 2).await);
    wait_subscribed(&stub, &["decl.keep", "decl.drop", "decl.keep"], 5).await;
    client.close().await;

    // A refused unsubscribe leaves the registry and the replay requirement
    // intact.
    let mut stub0 = Arc::try_unwrap(Stub::new(false, false)).ok().unwrap();
    stub0.reject_unsubscribe = Some("decl.keep".to_string());
    let stub = Arc::new(stub0);
    let (url, _acceptor) = start(&stub).await;
    let client = SupervisedClient::connect_options("unsub-refused", &url)
        .with_initial_topics(vec!["decl.keep".to_string()])
        .connect()
        .await
        .expect("initial establish");
    let error = client
        .unsubscribe_topic("decl.keep")
        .await
        .err()
        .expect("a refused unsubscribe errors");
    assert!(
        matches!(error, SupervisedError::Transport(_)),
        "got {error}"
    );
    assert_eq!(
        client.subscription_registry().snapshot(),
        vec!["decl.keep".to_string()]
    );
    stub.drop_conn1.notify_one();
    assert!(wait_until(10, || client.connection_generation() == 2).await);
    wait_subscribed(&stub, &["decl.keep", "decl.keep"], 5).await;
    client.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_subscribe_ack_never_enters_the_reconnect_snapshot() {
    // The socket dies while a subscribe is in flight and no ACK ever lands:
    // the topic never enters the registry, and the reconnect snapshot (taken
    // after the transaction lock) replays the pre-ACK set only.
    let mut stub0 = Arc::try_unwrap(Stub::new(false, false)).ok().unwrap();
    stub0
        .initial_actions
        .insert("dyn.slow".to_string(), SubscribeAction::Disconnect);
    let stub = Arc::new(stub0);
    let (url, _acceptor) = start(&stub).await;

    let client = SupervisedClient::connect("stale-ack", &url).await.unwrap();
    let error = client
        .subscribe_topic("dyn.slow")
        .await
        .err()
        .expect("the subscribe dies with the socket");
    assert!(
        matches!(error, SupervisedError::Transport(_)),
        "got {error}"
    );
    assert!(
        client.subscription_registry().is_empty(),
        "a lost ACK never enters the registry"
    );
    // The reconnect replays nothing: the pre-ACK set is what was snapshotted.
    assert!(wait_until(10, || client.connection_generation() == 2).await);
    assert!(
        stub.state.lock().await.subscribed.is_empty(),
        "the never-ACKed subscribe must not replay"
    );
    client.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_finite_connect_during_a_held_ack_cleans_up() {
    let mut stub0 = Arc::try_unwrap(Stub::new(false, false)).ok().unwrap();
    let (action, entered, release) = hold_action();
    stub0
        .initial_actions
        .insert("decl.held".to_string(), action);
    let stub = Arc::new(stub0);
    let (url, _acceptor) = start(&stub).await;

    let connect = tokio::spawn(async move {
        SupervisedClient::connect_options("finite-cancel", &url)
            .with_initial_topics(vec!["decl.a".to_string(), "decl.held".to_string()])
            .connect()
            .await
    });
    entered.notified().await;
    connect.abort();
    let _ = connect.await;

    release.notify_one();
    assert!(
        wait_until(5, || stub
            .state
            .try_lock()
            .map(|s| s.open_connections == 0 && s.active_registrations.is_empty())
            .unwrap_or(false))
        .await,
        "a cancelled finite connect must release the socket and the name"
    );
    assert_eq!(stub.state.lock().await.connections, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_drop_and_deregister_during_held_declarations_never_publish() {
    // Shutdown in the middle of the FIRST held declaration ACK.
    let mut stub0 = Arc::try_unwrap(Stub::new(false, false)).ok().unwrap();
    let (action, entered, release) = hold_action();
    stub0.initial_actions.insert("decl.one".to_string(), action);
    let stub = Arc::new(stub0);
    let (url, _acceptor) = start(&stub).await;
    let client = SupervisedClient::connect_options("close-held", &url)
        .with_initial_topics(vec!["decl.one".to_string(), "decl.two".to_string()])
        .start();
    let mut incoming = client.incoming().unwrap();
    let states = client.subscribe_state();
    entered.notified().await;
    tokio::time::timeout(Duration::from_secs(1), client.close())
        .await
        .expect("close must not wait for the held ACK");
    assert_eq!(client.state(), ConnState::ShuttingDown);
    assert_eq!(client.connection_generation(), 0);
    // The late ACK racing the stop must not publish.
    release.notify_one();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(*states.borrow(), ConnState::ShuttingDown);
    assert_eq!(client.connection_generation(), 0);
    assert!(
        tokio::time::timeout(Duration::from_secs(1), incoming.recv())
            .await
            .expect("the lane closes")
            .is_none()
    );
    assert_eq!(stub.state.lock().await.connections, 1);

    // Deregister during the LAST held declaration ACK.
    let mut stub0 = Arc::try_unwrap(Stub::new(false, false)).ok().unwrap();
    let (action, entered, release) = hold_action();
    stub0.initial_actions.insert("decl.two".to_string(), action);
    let stub = Arc::new(stub0);
    let (url, _acceptor) = start(&stub).await;
    let client = SupervisedClient::connect_options("dereg-held", &url)
        .with_initial_topics(vec!["decl.one".to_string(), "decl.two".to_string()])
        .start();
    let states = client.subscribe_state();
    entered.notified().await;
    assert!(matches!(
        client.deregister().await,
        Err(SupervisedError::Disconnected)
    ));
    release.notify_one();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(*states.borrow(), ConnState::ShuttingDown);
    assert_eq!(client.connection_generation(), 0);
    assert_eq!(stub.state.lock().await.connections, 1);

    // Dropping the incoming receiver during a held declaration stops the
    // supervisor and releases the socket and the name.
    let mut stub0 = Arc::try_unwrap(Stub::new(false, false)).ok().unwrap();
    let (action, entered, release) = hold_action();
    stub0.initial_actions.insert("decl.one".to_string(), action);
    let stub = Arc::new(stub0);
    let (url, _acceptor) = start(&stub).await;
    let client = SupervisedClient::connect_options("consumer-drop", &url)
        .with_initial_topics(vec!["decl.one".to_string()])
        .start();
    let states = client.subscribe_state();
    // The subscribe is in flight (held) with the consumer still alive.
    entered.notified().await;
    drop(client.incoming().unwrap());
    release.notify_one();
    assert!(wait_until(5, || *states.borrow() == ConnState::ShuttingDown).await);
    assert_eq!(client.connection_generation(), 0);
    assert!(
        wait_until(5, || stub
            .state
            .try_lock()
            .map(|s| s.open_connections == 0 && s.active_registrations.is_empty())
            .unwrap_or(false))
        .await
    );
    assert_eq!(stub.state.lock().await.connections, 1);
    drop(client);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_during_retry_backoff_stops_dialing() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/ws", listener.local_addr().unwrap());
    let accepted = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let counts = accepted.clone();
    let acceptor = tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            counts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            drop(socket);
        }
    });
    let client = SupervisedClient::connect_options("backoff-cancel", &url).start();
    let states = client.subscribe_state();

    // Settle into a backoff window: the dial count is stable across an
    // observation shorter than the smallest backoff (250 ms).
    let mut settled = 0;
    for _ in 0..100 {
        let now = accepted.load(std::sync::atomic::Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(10)).await;
        if now >= 2 && accepted.load(std::sync::atomic::Ordering::SeqCst) == now {
            tokio::time::sleep(Duration::from_millis(80)).await;
            if accepted.load(std::sync::atomic::Ordering::SeqCst) == now {
                settled = now;
                break;
            }
        }
    }
    assert!(
        settled >= 2,
        "expected the supervisor to settle into backoff"
    );
    drop(client);
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(
        accepted.load(std::sync::atomic::Ordering::SeqCst),
        settled,
        "a dropped client must stop dialing during backoff"
    );
    assert_eq!(*states.borrow(), ConnState::ShuttingDown);
    acceptor.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bounded_flood_before_final_ack_delivers_overflow_only_after_publication() {
    let mut stub0 = Arc::try_unwrap(Stub::new(false, false)).ok().unwrap();
    stub0.flood_on_register = 32;
    let (action, entered, release) = hold_action();
    stub0.initial_actions.insert("decl.two".to_string(), action);
    let stub = Arc::new(stub0);
    let (url, _acceptor) = start(&stub).await;

    let client = SupervisedClient::connect_options("flood-held", &url)
        .bounded_incoming(2)
        .with_initial_topics(vec!["decl.one".to_string(), "decl.two".to_string()])
        .start();
    let mut incoming = client
        .incoming_bounded()
        .expect("bounded receiver available exactly once");
    entered.notified().await;
    // The native lane has already overflowed, but nothing is forwarded
    // before the generation is published.
    assert!(
        tokio::time::timeout(Duration::from_millis(200), incoming.recv())
            .await
            .is_err(),
        "no overflow may be reported before publication"
    );
    release.notify_one();
    wait_connected(&client, 5).await;
    assert_eq!(client.connection_generation(), 1);

    let mut saw_overflow = false;
    let mut commands = 0;
    for _ in 0..3 {
        match tokio::time::timeout(Duration::from_secs(2), incoming.recv())
            .await
            .expect("the lane yields promptly")
        {
            Some(BoundedIncomingEvent::Overflow { dropped }) => {
                assert_eq!(dropped, 30, "capacity 2 retains two of 32");
                saw_overflow = true;
            }
            Some(BoundedIncomingEvent::Command(command)) => {
                assert_eq!(command.generation, 1);
                commands += 1;
            }
            None => panic!("the lane closed before delivering"),
        }
    }
    assert!(saw_overflow, "the overflow must be delivered after success");
    assert_eq!(commands, 2, "the two retained commands follow");
    assert_eq!(incoming.overflow_count(), 30);
    assert!(tokio::time::timeout(Duration::from_millis(200), incoming.recv()).await.is_err(),
        "only one overflow notice and two retained commands are emitted");
    client.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_attempt_discards_its_queued_lane() {
    // The first attempt is flooded then fails on the second declaration; its
    // queued commands and overflow belong to a never-published generation
    // and must not leak into the outward lane of the successful retry.
    let mut stub0 = Arc::try_unwrap(Stub::new(false, false)).ok().unwrap();
    stub0.first_register_flood = 64;
    stub0
        .initial_actions
        .insert("decl.two".to_string(), SubscribeAction::Disconnect);
    let stub = Arc::new(stub0);
    let (url, _acceptor) = start(&stub).await;

    let client = SupervisedClient::connect_options("discard-lane", &url)
        .bounded_incoming(2)
        .with_initial_topics(vec!["decl.one".to_string(), "decl.two".to_string()])
        .connect()
        .await
        .expect("the retry establishes");
    let mut incoming = client
        .incoming_bounded()
        .expect("bounded receiver available exactly once");
    assert_eq!(
        incoming.overflow_count(),
        0,
        "the failed attempt's overflow must not leak"
    );
    let first = tokio::time::timeout(Duration::from_secs(5), incoming.recv())
        .await
        .expect("the retry's delivery arrives")
        .expect("a command, not channel close");
    match first {
        BoundedIncomingEvent::Command(command) => {
            assert_eq!(command.command, "world.test.ping");
            assert_eq!(command.generation, 1);
        }
        BoundedIncomingEvent::Overflow { dropped } => {
            panic!("unexpected overflow from the failed attempt: {dropped}")
        }
    }
    client.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconnect_resends_verb_descriptors() {
    // The manifest is carried on every connection: a reconnected client's
    // reader answers HELP with the same verbs, re-sent from scratch.
    let mut stub0 = Arc::try_unwrap(Stub::new(false, false)).ok().unwrap();
    stub0.help_on_reconnect = true;
    let stub = Arc::new(stub0);
    let (url, _acceptor) = start(&stub).await;
    let verbs = vec![bus::VerbDescriptor::new(
        "verbs.report",
        &["id"],
        "Report a value",
        false,
    )];
    let client = SupervisedClient::connect_options("verbs-svc", &url)
        .with_verbs(verbs)
        .connect()
        .await
        .expect("initial establish");

    stub.drop_conn1.notify_one();
    assert!(wait_until(10, || client.connection_generation() == 2).await);
    assert!(
        wait_until(5, || stub
            .state
            .try_lock()
            .map(|s| !s.responses.is_empty())
            .unwrap_or(false))
        .await,
        "the reconnected reader must answer HELP"
    );
    let s = stub.state.lock().await;
    let reply = &s.responses[0];
    assert_eq!(reply.get("command"), Some("HELP"));
    assert_eq!(reply.get("from"), Some("verbs-svc"));
    assert_eq!(reply.get("rc"), Some("0"));
    assert!(
        reply.body.contains("verbs.report"),
        "the verb manifest is re-sent from the reconnected client: {}",
        reply.body
    );
    drop(s);
    client.close().await;
}

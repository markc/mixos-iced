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
use tokio::sync::{Mutex, Notify};
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
    flood_on_register: usize,
}

impl Stub {
    fn flooding(commands: usize) -> Arc<Stub> {
        Arc::new(Stub {
            state: Mutex::new(StubState::default()),
            drop_conn1: Notify::new(),
            fail_replay_on_reconnect: false,
            reject_register_on_reconnect: false,
            reject_topic: None,
            flood_on_register: commands,
        })
    }
    fn new(fail_replay_on_reconnect: bool, reject_register_on_reconnect: bool) -> Arc<Stub> {
        Arc::new(Stub {
            state: Mutex::new(StubState::default()),
            drop_conn1: Notify::new(),
            fail_replay_on_reconnect,
            reject_register_on_reconnect,
            reject_topic: None,
            flood_on_register: 0,
        })
    }

    fn rejecting(topic: &str) -> Arc<Stub> {
        Arc::new(Stub {
            state: Mutex::new(StubState::default()),
            drop_conn1: Notify::new(),
            fail_replay_on_reconnect: false,
            reject_register_on_reconnect: false,
            reject_topic: Some(topic.to_string()),
            flood_on_register: 0,
        })
    }
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

fn collision_reply(req: &BusMessage) -> String {
    let mut response = bus::parse(&reply(req, "10")).expect("stub reply parses");
    response.set("error", "stub collision diagnostic wording");
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
                        if collision {
                            // Keep the socket open, as the real broker does;
                            // the client must close its half-built
                            // connection itself.
                            let _ = sink.send(Message::Text(collision_reply(&req).into())).await;
                            continue;
                        }
                        let _ = sink.send(Message::Text(reply(&req, "0").into())).await;
                        for sequence in 0..stub.flood_on_register {
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
                        }
                    }
                    "topic.subscribe" => {
                        let name = req.get("name").unwrap_or("").to_string();
                        let reject = (stub.fail_replay_on_reconnect && conn_index >= 2)
                            || stub.reject_topic.as_deref() == Some(name.as_str());
                        {
                            let mut s = stub.state.lock().await;
                            s.subscribe_attempts += 1;
                            if !reject {
                                s.subscribed.push(name);
                            }
                        }
                        let rc = if reject { "10" } else { "0" };
                        let _ = sink.send(Message::Text(reply(&req, rc).into())).await;
                    }
                    "topic.unsubscribe" => {
                        let name = req.get("name").unwrap_or("").to_string();
                        let reject = stub.reject_topic.as_deref() == Some(name.as_str());
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

    tokio::time::sleep(Duration::from_millis(300)).await;
    let s = stub.state.lock().await;
    assert_eq!(s.connections, 2, "opt-in rejection must not be retried");
    assert!(s.open_connections <= 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_frees_the_name_and_respond_parts_frames_a_response() {
    let stub = Stub::new(false, false);
    let (url, _acceptor) = start(&stub).await;
    let client = SupervisedClient::connect("responder", &url).await.unwrap();
    client
        .respond_parts("caller", "responder.ping", Some("7"), 0, "{\"pong\":true}")
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

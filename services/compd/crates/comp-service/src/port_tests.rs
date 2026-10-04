// The FakeClient harness and the tests of the worker, ingress, admission,
// dispatcher, responders, reply loop, publisher and shutdown. The pure parser
// tests live in comp-model. The command channel is `command_channel` with a
// no-op waker; the outbox comes from `crate::outbox`.

use super::*;
use std::future;

use comp_model::observation::PropValue;
use comp_model::request::SequenceStep;

type PublishedMessages = Arc<Mutex<Vec<(BTreeMap<String, String>, String)>>>;

struct FakeClient {
    incoming: Mutex<Option<tokio_mpsc::UnboundedReceiver<bus::IncomingCommand>>>,
    states: watch::Sender<ConnState>,
    hang_replies: bool,
    responses_started: Arc<AtomicUsize>,
    publish_mode: Arc<AtomicU8>,
    publish_attempts: Arc<AtomicUsize>,
    reject_publish_attempt: Arc<AtomicUsize>,
    publications: PublishedMessages,
    deregister_hangs: Arc<AtomicBool>,
    deregistered: Arc<AtomicUsize>,
    closed: Arc<AtomicUsize>,
}

impl FakeClient {
    fn new(
        initial_state: ConnState,
        hang_replies: bool,
    ) -> (
        Self,
        tokio_mpsc::UnboundedSender<bus::IncomingCommand>,
        watch::Sender<ConnState>,
    ) {
        let (commands, incoming) = tokio_mpsc::unbounded_channel();
        let (states, _) = watch::channel(initial_state);
        (
            Self {
                incoming: Mutex::new(Some(incoming)),
                states: states.clone(),
                hang_replies,
                responses_started: Arc::new(AtomicUsize::new(0)),
                publish_mode: Arc::new(AtomicU8::new(0)),
                publish_attempts: Arc::new(AtomicUsize::new(0)),
                reject_publish_attempt: Arc::new(AtomicUsize::new(usize::MAX)),
                publications: Arc::new(Mutex::new(Vec::new())),
                deregister_hangs: Arc::new(AtomicBool::new(false)),
                deregistered: Arc::new(AtomicUsize::new(0)),
                closed: Arc::new(AtomicUsize::new(0)),
            },
            commands,
            states,
        )
    }
}

impl WorkerClient for FakeClient {
    fn incoming(
        &self,
    ) -> Option<tokio_mpsc::UnboundedReceiver<bus::IncomingCommand>> {
        self.incoming
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }

    fn state(&self) -> ConnState {
        *self.states.borrow()
    }

    fn subscribe_state(&self) -> watch::Receiver<ConnState> {
        self.states.subscribe()
    }

    fn respond_parts<'a>(
        &'a self,
        _reply: &'a PendingReply,
    ) -> WorkerFuture<'a, Result<(), String>> {
        self.responses_started.fetch_add(1, Ordering::AcqRel);
        if self.hang_replies {
            Box::pin(future::pending())
        } else {
            Box::pin(future::ready(Ok(())))
        }
    }

    fn publish<'a>(
        &'a self,
        headers: &'a BTreeMap<String, String>,
        wire: &'a str,
    ) -> WorkerFuture<'a, Result<(), String>> {
        let attempt = self.publish_attempts.fetch_add(1, Ordering::AcqRel) + 1;
        if attempt == self.reject_publish_attempt.load(Ordering::Acquire) {
            return Box::pin(future::ready(Err("publication rejected".into())));
        }
        match self.publish_mode.load(Ordering::Acquire) {
            1 => Box::pin(future::ready(Err("publication rejected".into()))),
            2 => Box::pin(future::pending()),
            _ => {
                self.publications
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push((headers.clone(), wire.to_string()));
                Box::pin(future::ready(Ok(())))
            }
        }
    }

    fn deregister(&self) -> WorkerFuture<'_, Result<(), String>> {
        self.deregistered.fetch_add(1, Ordering::AcqRel);
        if self.deregister_hangs.load(Ordering::Acquire) {
            Box::pin(future::pending())
        } else {
            Box::pin(future::ready(Ok(())))
        }
    }

    fn close(&self) -> WorkerFuture<'_, ()> {
        self.closed.fetch_add(1, Ordering::AcqRel);
        Box::pin(future::ready(()))
    }

    fn subscribe_topic<'a>(&'a self, _topic: &'a str) -> WorkerFuture<'a, Result<(), String>> {
        Box::pin(future::ready(Ok(())))
    }
}

fn test_ingress() -> (PortIngress, CommandSource, Arc<AtomicUsize>) {
    let queue_depth = Arc::new(AtomicUsize::new(0));
    let (sender, source) = command_channel(PORT_QUEUE_CAPACITY, crate::channel::no_waker());
    (
        PortIngress {
            sender,
            queue_depth: Arc::clone(&queue_depth),
            control_order: Arc::new(AtomicU64::new(0)),
            agent_epoch: Arc::new(AtomicU64::new(0)),
            pending_idle_order: Arc::new(AtomicU64::new(0)),
            pending_active_order: Arc::new(AtomicU64::new(0)),
        },
        source,
        queue_depth,
    )
}

fn test_observation_args() -> (Arc<AtomicU64>, ObservationOutbox, Arc<AtomicU64>) {
    let lost = Arc::new(AtomicU64::new(0));
    let (_producer, receiver) = crate::outbox::outbox(Arc::clone(&lost), Arc::new(AtomicU64::new(0)));
    (Arc::new(AtomicU64::new(0)), receiver, lost)
}

fn command(command: &str, id: usize) -> bus::IncomingCommand {
    bus::IncomingCommand {
        from: "test-caller".into(),
        command: command.into(),
        id: Some(id.to_string()),
        args: Value::Null,
        body: String::new(),
        headers: BTreeMap::new(),
    }
}

async fn wait_for_broker(broker: &AtomicU8, expected: u8) {
    tokio::time::timeout(Duration::from_secs(1), async {
        while broker.load(Ordering::Acquire) != expected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("worker publishes broker edge");
}

async fn wait_for_counter(counter: &AtomicUsize, expected: usize, message: &'static str) {
    tokio::time::timeout(Duration::from_secs(1), async {
        while counter.load(Ordering::Acquire) != expected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect(message);
}

async fn next_port_command(source: &CommandSource) -> PortCommand {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            match source.try_recv() {
                Ok(command) => return command,
                Err(mpsc::TryRecvError::Empty) => tokio::task::yield_now().await,
                Err(mpsc::TryRecvError::Disconnected) => {
                    panic!("port source disconnected")
                }
            }
        }
    })
    .await
    .expect("worker admits command")
}

#[tokio::test(start_paused = true)]
async fn worker_refusal_stays_retrying_without_panicking() {
    let (_shutdown_tx, mut shutdown) = watch::channel(false);
    let broker = AtomicU8::new(BROKER_CONNECTED);
    let attempts = AtomicUsize::new(0);
    let (_sender, protocol_source) = command_channel(1, crate::channel::no_waker());
    let result = connect_loop(
        &mut shutdown,
        &broker,
        || {
            attempts.fetch_add(1, Ordering::Relaxed);
            std::future::ready(Err::<(), _>(ConnectAttemptError::Retry(
                "connection refused".into(),
            )))
        },
        Duration::from_millis(1),
    );
    tokio::pin!(result);
    tokio::select! {
        _ = &mut result => panic!("refused connector must keep retrying"),
        _ = async {
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_millis(250)).await;
            tokio::task::yield_now().await;
        } => {}
    }
    assert_eq!(broker.load(Ordering::Acquire), BROKER_RETRYING);
    assert!(attempts.load(Ordering::Relaxed) >= 1);
    assert!(matches!(
        protocol_source.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn complete_worker_loop_tracks_state_edges_without_polling() {
    let (ingress, _source, _) = test_ingress();
    let broker = Arc::new(AtomicU8::new(BROKER_RETRYING));
    let reply_timeouts = Arc::new(AtomicU64::new(0));
    let (publish_timeouts, observations, lost_count) = test_observation_args();
    let (shutdown_tx, shutdown) = watch::channel(false);
    let (client, _commands, states) = FakeClient::new(ConnState::Connected, false);
    let mut client = Some(client);
    assert_eq!(broker.load(Ordering::Acquire), BROKER_RETRYING);
    let worker = tokio::spawn(worker_loop(
        "comp-nested".into(),
        ingress,
        Arc::clone(&broker),
        reply_timeouts,
        publish_timeouts,
        observations,
        Arc::new(tokio::sync::Notify::new()),
        lost_count,
        shutdown,
        move || future::ready(Ok(client.take().expect("one connection attempt"))),
    ));

    wait_for_broker(&broker, BROKER_CONNECTED).await;
    states.send_replace(ConnState::Disconnected);
    wait_for_broker(&broker, BROKER_RETRYING).await;
    states.send_replace(ConnState::Connected);
    wait_for_broker(&broker, BROKER_CONNECTED).await;

    shutdown_tx.send_replace(true);
    worker.await.expect("worker exits cleanly");
}

#[tokio::test]
async fn complete_worker_loop_terminates_on_fatal_reconnect_collision() {
    let (ingress, _source, _) = test_ingress();
    let broker = Arc::new(AtomicU8::new(BROKER_RETRYING));
    let (_shutdown_tx, shutdown) = watch::channel(false);
    let (client, _commands, states) = FakeClient::new(ConnState::Connected, false);
    let mut client = Some(client);
    let (publish_timeouts, observations, lost_count) = test_observation_args();
    let worker = tokio::spawn(worker_loop(
        "comp-nested".into(),
        ingress,
        Arc::clone(&broker),
        Arc::new(AtomicU64::new(0)),
        publish_timeouts,
        observations,
        Arc::new(tokio::sync::Notify::new()),
        lost_count,
        shutdown,
        move || future::ready(Ok(client.take().expect("one connection attempt"))),
    ));

    wait_for_broker(&broker, BROKER_CONNECTED).await;
    states.send_replace(ConnState::Fatal);
    tokio::time::timeout(Duration::from_secs(1), worker)
        .await
        .expect("fatal reconnect collision terminates worker")
        .expect("worker exits cleanly");
    assert_eq!(broker.load(Ordering::Acquire), BROKER_RETRYING);
}

#[tokio::test]
async fn complete_worker_loop_terminates_on_registration_rejection_without_renaming() {
    let (ingress, _source, _) = test_ingress();
    let broker = Arc::new(AtomicU8::new(BROKER_CONNECTED));
    let attempts = Arc::new(AtomicUsize::new(0));
    let attempts_for_connector = Arc::clone(&attempts);
    let (_shutdown_tx, shutdown) = watch::channel(false);
    let (publish_timeouts, observations, lost_count) = test_observation_args();
    worker_loop(
        "comp-nested".into(),
        ingress,
        Arc::clone(&broker),
        Arc::new(AtomicU64::new(0)),
        publish_timeouts,
        observations,
        Arc::new(tokio::sync::Notify::new()),
        lost_count,
        shutdown,
        move || {
            attempts_for_connector.fetch_add(1, Ordering::Relaxed);
            future::ready(Err::<FakeClient, _>(
                ConnectAttemptError::RegistrationRejected {
                    service: "comp-nested".into(),
                    rc: 10,
                    message: "registration refused".into(),
                },
            ))
        },
    )
    .await;
    assert_eq!(attempts.load(Ordering::Relaxed), 1);
    assert_eq!(broker.load(Ordering::Acquire), BROKER_RETRYING);
}

#[test]
fn reply_headroom_covers_maximal_headers_and_exact_body_limit_fits() {
    let maximal_service = format!("a{}", "z".repeat(30));
    assert!(validate_service_name(&maximal_service).is_ok());
    let reply = PendingReply {
        from: maximal_service.clone(),
        command: "comp.props.describe".into(),
        id: Some(format!("noded-{}", u64::MAX)),
        rc: 0,
        body: Arc::from("x".repeat(MAX_REPLY_BODY_BYTES)),
    };
    let wire_bytes = reply_wire_bytes(&maximal_service, &reply);
    let header_and_framing = wire_bytes - reply.body.len();
    assert!(
        header_and_framing <= comp_model::reply::REPLY_WIRE_HEADROOM_BYTES,
        "{header_and_framing} header/framing bytes exceed the documented reserve"
    );
    assert!(wire_bytes <= MAX_REPLY_WIRE_BYTES);

    let checked = enforce_reply_wire_limit(&maximal_service, reply)
        .expect("maximal canonical reply headers fit");
    assert_eq!(checked.rc, 0);
    assert_eq!(checked.body.len(), MAX_REPLY_BODY_BYTES);
}

#[test]
fn measured_wire_overflow_becomes_too_large() {
    let reply = PendingReply {
        from: "requester".into(),
        command: "comp.props.get".into(),
        id: Some("noded-1".into()),
        rc: 0,
        body: Arc::from("x".repeat(MAX_REPLY_WIRE_BYTES)),
    };
    assert!(reply_wire_bytes("comp-nested", &reply) > MAX_REPLY_WIRE_BYTES);

    let checked =
        enforce_reply_wire_limit("comp-nested", reply).expect("too_large response fits");
    assert_eq!(checked.rc, 10);
    assert_eq!(
        serde_json::from_str::<Value>(&checked.body).expect("too_large JSON"),
        serde_json::json!({
            "error": "too_large",
            "error_code": "too_large",
            "limit_bytes": MAX_REPLY_BODY_BYTES,
            "hint": "read a subtree",
        })
    );
    assert!(reply_wire_bytes("comp-nested", &checked) <= MAX_REPLY_WIRE_BYTES);
}

#[tokio::test]
async fn ping_ignores_malformed_body_while_property_reads_reject_it() {
    let (ingress, source, _) = test_ingress();
    let mut responders = JoinSet::new();
    let responder_permits = Arc::new(Semaphore::new(PORT_QUEUE_CAPACITY));
    let (reply_sender, mut replies) = tokio_mpsc::channel(2);
    let reply_timeouts = Arc::new(AtomicU64::new(0));

    let mut ping = command("comp.ping", 1);
    ping.body = "{".into();
    handle_incoming(
        &ingress,
        &mut responders,
        &responder_permits,
        &reply_sender,
        &reply_timeouts,
        "comp-nested",
        ping,
    );
    let reply = replies.recv().await.expect("ping reply queued");
    assert_eq!(reply.rc, 0);
    assert_eq!(reply.body.as_ref(), "{\"pong\":true}");

    let mut get = command("comp.props.get", 2);
    get.body = "{".into();
    handle_incoming(
        &ingress,
        &mut responders,
        &responder_permits,
        &reply_sender,
        &reply_timeouts,
        "comp-nested",
        get,
    );
    let reply = replies.recv().await.expect("malformed read reply queued");
    assert_eq!(reply.rc, 10);
    assert_eq!(
        reply.body.as_ref(),
        "{\"error\":\"unknown_path\",\"error_code\":\"unknown_path\"}"
    );
    assert!(matches!(source.try_recv(), Err(mpsc::TryRecvError::Empty)));
}

fn local_set_command(id: usize, path: &str, value: Value) -> bus::IncomingCommand {
    let mut command = command("comp.props.set", id);
    command.args = json!({"path": path, "value": value});
    command.body = command.args.to_string();
    command
        .headers
        .insert("broker_origin".into(), "local".into());
    command
}

/// Mesh law (2026-09-15): being on the mesh is the whole authorization.
/// A mesh-stamped caller with peer/identity headers, a caller with no
/// origin stamp at all, and one with an unusual name all reach the
/// calloop exactly like a local one, for writes and pointer watches.
#[tokio::test]
async fn mesh_and_unstamped_callers_reach_set_and_pointer_watch() {
    let mut mesh = local_set_command(1, "input.corners.enabled", json!(true));
    mesh.headers.insert("broker_origin".into(), "mesh".into());
    mesh.headers
        .insert("source_peer".into(), "beta.example".into());
    mesh.headers.insert("signed_ident".into(), "opaque".into());
    let mut unstamped = local_set_command(2, "input.corners.enabled", json!(false));
    unstamped.headers.clear();
    let mut odd_caller = local_set_command(3, "input.corners.dwell_ms", json!(250));
    odd_caller.from = "anonymous".into();
    let mut mesh_pointer = command("comp.pointer.watch", 4);
    mesh_pointer
        .headers
        .insert("broker_origin".into(), "mesh".into());
    mesh_pointer
        .headers
        .insert("source_peer".into(), "beta.example".into());

    for command in [mesh, unstamped, odd_caller, mesh_pointer] {
        let (ingress, source, _) = test_ingress();
        let mut responders = JoinSet::new();
        let permits = Arc::new(Semaphore::new(PORT_QUEUE_CAPACITY));
        let (reply_sender, mut replies) = tokio_mpsc::channel(2);
        let reply_timeouts = Arc::new(AtomicU64::new(0));
        let verb = command.command.clone();
        let id = command.id.clone();
        handle_incoming(
            &ingress,
            &mut responders,
            &permits,
            &reply_sender,
            &reply_timeouts,
            "comp-nested",
            command,
        );
        match source.try_recv() {
            Ok(PortCommand::Set(_)) => assert_eq!(verb, "comp.props.set", "{id:?}"),
            Ok(PortCommand::PointerWatch(_)) => {
                assert_eq!(verb, "comp.pointer.watch", "{id:?}")
            }
            _ => panic!("{verb} {id:?} was not admitted"),
        }
        assert!(
            replies.try_recv().is_err(),
            "{verb} {id:?} got an early refusal"
        );
        responders.abort_all();
    }
}

#[tokio::test]
async fn region_mesh_dispatch_uses_long_pool_and_replies_once() {
    let (ingress, source, depth) = test_ingress();
    let mut responders = JoinSet::new();
    let permits = Arc::new(Semaphore::new(PORT_QUEUE_CAPACITY));
    let long_permits = Arc::new(Semaphore::new(LONG_VERB_PERMITS));
    // Model seven other long operations occupying the production pool.
    let other_operations = long_permits
        .clone()
        .try_acquire_many_owned((LONG_VERB_PERMITS - 1) as u32)
        .unwrap();
    let (reply_sender, mut replies) = tokio_mpsc::channel(8);
    let reply_timeouts = Arc::new(AtomicU64::new(0));
    for index in 0..2 {
        let mut incoming = command("comp.region.select", index);
        incoming.from = "mesh-agent".into();
        incoming.args = json!({"output":"Output-1","timeout_ms":55_000});
        incoming.body = incoming.args.to_string();
        dispatch_incoming(
            &ingress,
            &mut responders,
            &permits,
            &long_permits,
            &reply_sender,
            &reply_timeouts,
            "comp",
            incoming,
        );
    }
    let busy = replies.try_recv().unwrap();
    assert_eq!(busy.id.as_deref(), Some("1"));
    assert_eq!(busy.rc, 10);
    assert_eq!(
        serde_json::from_str::<Value>(&busy.body).unwrap()["error"],
        "busy"
    );
    assert_eq!(permits.available_permits(), PORT_QUEUE_CAPACITY);
    assert_eq!(long_permits.available_permits(), 0);
    let Ok(PortCommand::Long(mut request)) = source.try_recv() else {
        panic!("region must use LongAdmission");
    };
    assert!(
        matches!(request.op.take(),Some(LongOp::RegionSelect{output:Some(name),timeout})
        if name=="Output-1" && timeout==Duration::from_secs(55))
    );
    request.slot.take();
    assert_eq!(depth.load(Ordering::Acquire), 0);
    assert!(replies.try_recv().is_err(), "selection is still pending");
    request
        .reply
        .take()
        .unwrap()
        .send(ControlReply::Body(
            json!({"version":1,"status":"cancelled","reason":"escape"}),
        ))
        .unwrap();
    responders.join_next().await.unwrap().unwrap();
    let reply = replies.try_recv().unwrap();
    assert_eq!(reply.from, "mesh-agent");
    assert_eq!(reply.id.as_deref(), Some("0"));
    assert_eq!(reply.rc, 0);
    assert_eq!(
        serde_json::from_str::<Value>(&reply.body).unwrap()["status"],
        "cancelled"
    );
    assert!(replies.try_recv().is_err(), "exactly one terminal reply");
    assert_eq!(long_permits.available_permits(), 1);
    drop(other_operations);
    assert_eq!(long_permits.available_permits(), LONG_VERB_PERMITS);
}

/// noded's registry diffs reach the compositor as the full live set,
/// unanswered (a topic delivery is not a verb); other props diffs, and a
/// malformed set, change nothing.
#[tokio::test]
async fn registry_diffs_reach_the_compositor_as_the_live_set() {
    let (ingress, source, _depth) = test_ingress();
    let mut responders = JoinSet::new();
    let permits = Arc::new(Semaphore::new(PORT_QUEUE_CAPACITY));
    let long_permits = Arc::new(Semaphore::new(LONG_VERB_PERMITS));
    let (reply_sender, mut replies) = tokio_mpsc::channel(8);
    let reply_timeouts = Arc::new(AtomicU64::new(0));
    // The live broker's shape (noded 0.18.2): no `from`, the publisher
    // stamped as `broker_service`, origin local. A delivery stamped as some
    // other publisher is not the registry.
    for (index, (publisher, body)) in [
        ("noded", json!({"path":"services.registered","old":["quoin"],"new":["noded","comp-nested"]})),
        ("noded", json!({"path":"mesh.peers","old":[],"new":["x"]})),
        ("noded", json!({"path":"services.registered","old":[],"new":["noded", 7]})),
        ("shell", json!({"path":"services.registered","old":[],"new":["shell"]})),
    ]
    .into_iter()
    .enumerate()
    {
        let mut delivery = command("props.changed", index);
        delivery.from = String::new();
        delivery.id = None;
        delivery.body = body.to_string();
        delivery.args = body;
        delivery.headers.insert("topic".into(), REGISTRY_TOPIC.into());
        delivery.headers.insert("broker_service".into(), publisher.into());
        delivery.headers.insert("broker_origin".into(), "local".into());
        dispatch_incoming(
            &ingress,
            &mut responders,
            &permits,
            &long_permits,
            &reply_sender,
            &reply_timeouts,
            "comp-nested",
            delivery,
        );
    }
    let Ok(PortCommand::ServicesLive(live)) = source.try_recv() else {
        panic!("the registry diff reaches the compositor");
    };
    assert_eq!(live, std::collections::BTreeSet::from(["noded".to_owned(), "comp-nested".to_owned()]));
    assert!(source.try_recv().is_err(), "nothing else is admitted");
    assert!(replies.try_recv().is_err(), "a topic delivery is never answered");
}

#[tokio::test]
async fn panel_verbs_dispatch_by_literal_command_under_a_non_default_service() {
    let (ingress, source, _depth) = test_ingress();
    let mut responders = JoinSet::new();
    let permits = Arc::new(Semaphore::new(PORT_QUEUE_CAPACITY));
    let long_permits = Arc::new(Semaphore::new(LONG_VERB_PERMITS));
    let (reply_sender, mut replies) = tokio_mpsc::channel(8);
    let reply_timeouts = Arc::new(AtomicU64::new(0));
    let hold = json!({"output":"Output-1","edge":"left","surface":"quoin.panel.1",
        "holder":"popup","acquire":true});
    for (index, (verb, args)) in [
        ("comp.panel.hold", hold.clone()),
        ("comp-nested.panel.hold", hold),
        (
            "comp.panel.mode",
            json!({"output":"Output-1","edge":"left","surface":"quoin.panel.1",
                "mode":"hidden","sticky":true}),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let mut incoming = command(verb, index);
        incoming.body = args.to_string();
        incoming.args = args;
        dispatch_incoming(
            &ingress,
            &mut responders,
            &permits,
            &long_permits,
            &reply_sender,
            &reply_timeouts,
            "comp-nested",
            incoming,
        );
    }
    // The service-prefixed spelling is not a verb, and a typo names its field.
    let mut refusals = BTreeMap::new();
    for _ in 0..2 {
        let reply = replies.try_recv().unwrap();
        let body: Value = serde_json::from_str(&reply.body).unwrap();
        refusals.insert(reply.id.clone().unwrap(), (reply.rc, body));
    }
    assert_eq!(refusals["1"].0, 10);
    assert_eq!(refusals["1"].1["error"], "unknown_verb");
    assert_eq!(refusals["2"].0, 10);
    assert_eq!(refusals["2"].1["error"], "invalid_args");
    assert_eq!(refusals["2"].1["field"], "sticky");
    // The literal verb reaches the compositor thread under any service name.
    let Ok(PortCommand::Panel(mut request)) = source.try_recv() else {
        panic!("comp.panel.hold must be admitted");
    };
    assert_eq!(request.op.surface, "quoin.panel.1");
    assert_eq!(request.op.acquire, Some(true));
    assert!(source.try_recv().is_err(), "refused requests are never admitted");
    request
        .reply
        .take()
        .unwrap()
        .send(ControlReply::Body(json!({"accepted":true,"surface":"quoin.panel.1"})))
        .unwrap();
    responders.join_next().await.unwrap().unwrap();
    let reply = replies.try_recv().unwrap();
    assert_eq!((reply.id.as_deref(), reply.rc), (Some("0"), 0));
}

#[tokio::test]
async fn window_wait_and_forced_close_take_the_long_pool() {
    let (ingress, source, depth) = test_ingress();
    let mut responders = JoinSet::new();
    let permits = Arc::new(Semaphore::new(PORT_QUEUE_CAPACITY));
    let long_permits = Arc::new(Semaphore::new(LONG_VERB_PERMITS));
    let (reply_sender, _replies) = tokio_mpsc::channel(8);
    let reply_timeouts = Arc::new(AtomicU64::new(0));
    for (index, (verb, args)) in [
        (
            "comp.window.wait",
            json!({"match": {"id": 7}, "until": "gone", "timeout_ms": 30_000}),
        ),
        (
            "comp.window.close",
            json!({"id": 7, "generation": 3, "force": true}),
        ),
        ("comp.window.close", json!({"id": 7, "generation": 3})),
    ]
    .into_iter()
    .enumerate()
    {
        let mut incoming = command(verb, index);
        incoming.body = args.to_string();
        incoming.args = args;
        dispatch_incoming(
            &ingress,
            &mut responders,
            &permits,
            &long_permits,
            &reply_sender,
            &reply_timeouts,
            "comp-nested",
            incoming,
        );
    }
    let Ok(PortCommand::Long(wait)) = source.try_recv() else {
        panic!("wait is long");
    };
    let Ok(PortCommand::Long(close)) = source.try_recv() else {
        panic!("forced close is long");
    };
    let Ok(PortCommand::Window(polite)) = source.try_recv() else {
        panic!("polite close is a window op");
    };
    assert!(matches!(wait.op, Some(LongOp::Wait(_))));
    assert!(matches!(close.op, Some(LongOp::ForceClose { .. })));
    assert_eq!(
        polite.op,
        WindowOp::Close {
            id: 7,
            generation: 3
        }
    );
    assert_eq!(long_permits.available_permits(), LONG_VERB_PERMITS - 2);
    assert_eq!(depth.load(Ordering::Acquire), 3);
    drop((wait, close));
    assert_eq!(depth.load(Ordering::Acquire), 1);
    responders.abort_all();
}

#[tokio::test]
async fn mesh_window_verbs_cross_ingress_in_order() {
    let (ingress, source, _) = test_ingress();
    let mut responders = JoinSet::new();
    let permits = Arc::new(Semaphore::new(PORT_QUEUE_CAPACITY));
    let (reply_sender, mut replies) = tokio_mpsc::channel(4);
    let reply_timeouts = Arc::new(AtomicU64::new(0));
    let mut minimize = command("comp.window.minimize", 1);
    minimize.args = json!({"id": 7, "generation": 3});
    minimize.body = minimize.args.to_string();
    minimize
        .headers
        .insert("broker_origin".into(), "mesh".into());
    let mut restore = command("comp.window.restore", 2);
    restore.args = json!({});
    restore.body = restore.args.to_string();
    let mut malformed = command("comp.window.restore", 3);
    malformed.body = "{".into();
    for command in [minimize, restore, malformed] {
        handle_incoming(
            &ingress,
            &mut responders,
            &permits,
            &reply_sender,
            &reply_timeouts,
            "comp-nested",
            command,
        );
    }
    let PortCommand::Window(first) = source.try_recv().expect("minimize admitted") else {
        panic!("window command expected");
    };
    let PortCommand::Window(second) = source.try_recv().expect("restore admitted") else {
        panic!("window command expected");
    };
    assert_eq!(
        first.op,
        WindowOp::Minimize {
            id: 7,
            generation: 3
        }
    );
    assert_eq!(second.op, WindowOp::Restore { target: None });
    assert!(first.order < second.order, "arrival order is kept");
    assert!(matches!(source.try_recv(), Err(mpsc::TryRecvError::Empty)));
    let refused = replies.recv().await.expect("malformed body refused");
    assert_eq!(refused.id.as_deref(), Some("3"));
    assert_eq!(refused.rc, 10);
    responders.abort_all();
}

#[tokio::test]
async fn input_verbs_cross_ingress_in_order_and_long_verbs_release_their_slot() {
    let (ingress, source, depth) = test_ingress();
    let mut responders = JoinSet::new();
    let permits = Arc::new(Semaphore::new(PORT_QUEUE_CAPACITY));
    let long_permits = Arc::new(Semaphore::new(1));
    let (reply_sender, mut replies) = tokio_mpsc::channel(8);
    let reply_timeouts = Arc::new(AtomicU64::new(0));
    let mut dispatch = |verb: &str, id: usize, args: Value| {
        let mut incoming = command(verb, id);
        incoming.body = args.to_string();
        incoming.args = args;
        incoming
            .headers
            .insert("broker_origin".into(), "mesh".into());
        dispatch_incoming(
            &ingress,
            &mut responders,
            &permits,
            &long_permits,
            &reply_sender,
            &reply_timeouts,
            "comp-nested",
            incoming,
        );
    };
    dispatch("comp.input.pointer.move", 1, json!({"x": 1, "y": 2}));
    dispatch(
        "comp.input.sequence",
        2,
        json!({"steps": [{"verb": "comp.input.release_all", "delay_ms": 50}]}),
    );
    // The long pool has one permit and it is held.
    dispatch(
        "comp.input.sequence",
        3,
        json!({"steps": [{"verb": "comp.input.release_all"}]}),
    );
    dispatch("comp.input.key", 4, json!({"text": "ok"}));
    dispatch("comp.input.teleport", 5, json!({}));
    dispatch("comp.input.key", 6, json!({"key": "a", "hold": true}));

    let Ok(PortCommand::Input(first)) = source.try_recv() else {
        panic!("move admitted");
    };
    let Ok(PortCommand::Long(mut second)) = source.try_recv() else {
        panic!("sequence admitted");
    };
    let Ok(PortCommand::Input(third)) = source.try_recv() else {
        panic!("text admitted");
    };
    assert!(first.order < second.order && second.order < third.order);
    assert_eq!(third.op, InputOp::OnSeat {
        seat: SeatKind::Agent,
        op: Box::new(InputOp::Text("ok".into())),
    });
    assert!(matches!(source.try_recv(), Err(mpsc::TryRecvError::Empty)));
    assert_eq!(depth.load(Ordering::Acquire), 3);
    // Taking the long request off the queue frees its slot while the
    // verb itself keeps waiting.
    assert!(second.slot.take().is_some());
    assert_eq!(depth.load(Ordering::Acquire), 2);

    let mut refusals = BTreeMap::new();
    for _ in 0..3 {
        let reply = replies.recv().await.expect("refusal");
        refusals.insert(
            reply.id.clone().unwrap(),
            serde_json::from_str::<Value>(&reply.body).unwrap(),
        );
    }
    assert_eq!(refusals["3"]["error"], "busy");
    assert_eq!(refusals["5"]["error"], "unknown_verb");
    assert_eq!(refusals["6"]["error"], "invalid_args");

    // The long reply waits for its own budget, not the 2 s snapshot one.
    let _ = second
        .reply
        .take()
        .unwrap()
        .send(ControlReply::Body(json!({"steps": []})));
    let reply = replies.recv().await.expect("sequence reply");
    assert_eq!(reply.id.as_deref(), Some("2"));
    assert_eq!(reply.rc, 0);
    responders.abort_all();
}

#[test]
fn long_admission_budget_is_the_verb_deadline_plus_slack() {
    let (ingress, _source, _) = test_ingress();
    let admission = ingress
        .request_long(LongOp::Sequence(vec![SequenceStep {
            verb: "comp.input.release_all",
            op: InputOp::ReleaseAll,
            delay: Duration::from_millis(1500),
        }]))
        .expect("admitted");
    assert_eq!(
        admission.timeout(),
        Duration::from_millis(1500) + LONG_VERB_SLACK
    );
}

#[tokio::test]
async fn authorised_set_crosses_ingress_and_preserves_response_correlation() {
    let (ingress, source, _) = test_ingress();
    let mut responders = JoinSet::new();
    let permits = Arc::new(Semaphore::new(PORT_QUEUE_CAPACITY));
    let (reply_sender, mut replies) = tokio_mpsc::channel(2);
    let reply_timeouts = Arc::new(AtomicU64::new(0));
    let command = local_set_command(37, "input.corners.dwell_ms", json!(250));

    handle_incoming(
        &ingress,
        &mut responders,
        &permits,
        &reply_sender,
        &reply_timeouts,
        "comp-nested",
        command,
    );
    let PortCommand::Set(request) = source.try_recv().expect("set admitted") else {
        panic!("set command expected");
    };
    assert_eq!(request.path, "input.corners.dwell_ms");
    assert_eq!(request.value, json!(250));
    request
        .reply
        .expect("set reply sender")
        .send(ControlReply::Set {
            path: "input.corners.dwell_ms".into(),
            old: PropValue::U64(200),
            new: PropValue::U64(250),
            persisted: None,
        })
        .expect("responder remains live");
    responders
        .join_next()
        .await
        .expect("responder completes")
        .expect("task");
    let reply = replies.recv().await.expect("correlated reply");
    assert_eq!(reply.id.as_deref(), Some("37"));
    assert_eq!(reply.command, "comp.props.set");
    assert_eq!(reply.rc, 0);
}

#[tokio::test]
async fn invalid_sets_cannot_exhaust_ingress_or_responder_permits() {
    let (ingress, source, _) = test_ingress();
    let mut responders = JoinSet::new();
    let permits = Arc::new(Semaphore::new(PORT_QUEUE_CAPACITY));
    let (reply_sender, mut replies) = tokio_mpsc::channel(PORT_QUEUE_CAPACITY + 1);
    let reply_timeouts = Arc::new(AtomicU64::new(0));

    for id in 0..PORT_QUEUE_CAPACITY {
        handle_incoming(
            &ingress,
            &mut responders,
            &permits,
            &reply_sender,
            &reply_timeouts,
            "comp-nested",
            local_set_command(id, "input.corners.dwell_ms", json!(5001)),
        );
    }
    for _ in 0..PORT_QUEUE_CAPACITY {
        let reply = replies.recv().await.expect("invalid-value reply");
        assert_eq!(reply.rc, 10);
        assert_eq!(
            serde_json::from_str::<Value>(&reply.body).unwrap()["error"],
            "invalid_value"
        );
    }
    assert!(responders.is_empty());
    assert_eq!(permits.available_permits(), PORT_QUEUE_CAPACITY);
    assert_eq!(ingress.depth(), 0);
    assert!(matches!(source.try_recv(), Err(mpsc::TryRecvError::Empty)));

    handle_incoming(
        &ingress,
        &mut responders,
        &permits,
        &reply_sender,
        &reply_timeouts,
        "comp-nested",
        command("comp.info", PORT_QUEUE_CAPACITY),
    );
    assert!(matches!(source.try_recv(), Ok(PortCommand::Snapshot(_))));
}

#[tokio::test]
async fn non_finite_json_set_is_rejected_before_admission() {
    let (ingress, source, _) = test_ingress();
    let mut responders = JoinSet::new();
    let permits = Arc::new(Semaphore::new(PORT_QUEUE_CAPACITY));
    let (reply_sender, mut replies) = tokio_mpsc::channel(2);
    let reply_timeouts = Arc::new(AtomicU64::new(0));
    let mut command = local_set_command(3, "input.corners.velocity_max_px_s", json!(1500.0));
    command.body = "{\"path\":\"input.corners.velocity_max_px_s\",\"value\":NaN}".into();
    handle_incoming(
        &ingress,
        &mut responders,
        &permits,
        &reply_sender,
        &reply_timeouts,
        "comp-nested",
        command,
    );
    let reply = replies.recv().await.expect("invalid-value reply");
    assert_eq!(reply.rc, 10);
    assert_eq!(
        serde_json::from_str::<Value>(&reply.body).unwrap()["error"],
        "invalid_value"
    );
    assert!(matches!(source.try_recv(), Err(mpsc::TryRecvError::Empty)));
}

#[tokio::test]
async fn exact_noded_topic_lifecycle_notices_cross_as_watch_state_only() {
    let (ingress, source, _) = test_ingress();
    let mut responders = JoinSet::new();
    let permits = Arc::new(Semaphore::new(PORT_QUEUE_CAPACITY));
    let (reply_sender, _replies) = tokio_mpsc::channel(2);
    let reply_timeouts = Arc::new(AtomicU64::new(0));
    // Either spelling of the broker: `from: noded`, or the live shape (no
    // `from`, `broker_service: noded`, origin local).
    for (verb, active, stamped) in [
        ("topic.active", true, false),
        ("topic.idle", false, false),
        ("topic.active", true, true),
        ("topic.idle", false, true),
    ] {
        let mut command = command(verb, 1);
        if stamped {
            command.from = String::new();
            command.headers.insert("broker_service".into(), "noded".into());
            command.headers.insert("broker_origin".into(), "local".into());
        } else {
            command.from = "noded".into();
        }
        command
            .headers
            .insert("name".into(), "comp-nested.props.changed".into());
        handle_incoming(
            &ingress,
            &mut responders,
            &permits,
            &reply_sender,
            &reply_timeouts,
            "comp-nested",
            command,
        );
        let PortCommand::WatchState {
            active: observed, ..
        } = source.try_recv().expect("notice staged")
        else {
            panic!("watch-state command expected");
        };
        assert_eq!(observed, active);
    }
    // Another service's `topic.idle` (stamped as itself) is not the broker's.
    let mut forged = command("topic.idle", 2);
    forged.from = "shell".into();
    forged.headers.insert("broker_service".into(), "shell".into());
    forged.headers.insert("broker_origin".into(), "local".into());
    forged
        .headers
        .insert("name".into(), "comp-nested.props.changed".into());
    handle_incoming(
        &ingress,
        &mut responders,
        &permits,
        &reply_sender,
        &reply_timeouts,
        "comp-nested",
        forged,
    );
    assert!(
        !matches!(source.try_recv(), Ok(PortCommand::WatchState { .. })),
        "another service cannot steer the watch state"
    );
}

/// `from: noded` with `broker_origin: mesh` is what a WireGuard client of
/// a pre-0.18 noded that registered the name `noded` looks like; it is
/// not this node's broker (defence in depth): it must not turn
/// this compositor's props publishing off, nor rewrite its live-service
/// set. A `local` stamp is still honoured.
#[tokio::test]
async fn mesh_origin_noded_claims_do_not_steer_the_broker_state() {
    let (ingress, source, _) = test_ingress();
    let mut responders = JoinSet::new();
    let permits = Arc::new(Semaphore::new(PORT_QUEUE_CAPACITY));
    let long_permits = Arc::new(Semaphore::new(LONG_VERB_PERMITS));
    let (reply_sender, mut replies) = tokio_mpsc::channel(4);
    let reply_timeouts = Arc::new(AtomicU64::new(0));
    let idle = |origin: &str| {
        let mut idle = command("topic.idle", 1);
        idle.from = "noded".into();
        idle.id = None;
        idle.headers
            .insert("name".into(), "comp-nested.props.changed".into());
        idle.headers.insert("broker_origin".into(), origin.into());
        idle.headers
            .insert("source_peer".into(), "beta.example".into());
        idle
    };
    let body = json!({"path":"services.registered","old":["quoin"],"new":["noded"]});
    let mut registry = command("props.changed", 2);
    registry.from = "noded".into();
    registry.id = None;
    registry.body = body.to_string();
    registry.args = body;
    registry
        .headers
        .insert("topic".into(), REGISTRY_TOPIC.into());
    registry
        .headers
        .insert("broker_origin".into(), "mesh".into());
    // A `local` spelling beside a non-local one is still refused.
    let mut mixed = idle("local");
    mixed.headers.insert("Broker_Origin".into(), "mesh".into());
    for forged in [idle("mesh"), idle("MESH"), mixed, registry] {
        dispatch_incoming(
            &ingress,
            &mut responders,
            &permits,
            &long_permits,
            &reply_sender,
            &reply_timeouts,
            "comp-nested",
            forged,
        );
    }
    assert!(
        matches!(source.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "a mesh-origin noded claim reaches nothing"
    );
    assert!(replies.try_recv().is_err(), "and is never answered");

    dispatch_incoming(
        &ingress,
        &mut responders,
        &permits,
        &long_permits,
        &reply_sender,
        &reply_timeouts,
        "comp-nested",
        idle("local"),
    );
    let Ok(PortCommand::WatchState { active, .. }) = source.try_recv() else {
        panic!("the local broker's idle notice still lands");
    };
    assert!(!active);
}

#[test]
fn both_lifecycle_directions_coalesce_latest_wins_when_ingress_is_full() {
    let (ingress, _source, _) = test_ingress();
    let _admissions = (0..PORT_QUEUE_CAPACITY)
        .map(|_| ingress.request_snapshot().expect("fill ingress"))
        .collect::<Vec<_>>();
    ingress.set_watch_state(false);
    let first_idle = ingress.pending_idle_order.load(Ordering::Acquire);
    ingress.set_watch_state(true);
    let active = ingress.pending_active_order.load(Ordering::Acquire);
    ingress.set_watch_state(false);
    let final_idle = ingress.pending_idle_order.load(Ordering::Acquire);
    assert_ne!(first_idle, 0);
    assert!(first_idle < active && active < final_idle);
}

#[tokio::test]
async fn saturated_ingress_returns_busy_before_set_reaches_calloop() {
    let (ingress, source, _) = test_ingress();
    let _admissions = (0..PORT_QUEUE_CAPACITY)
        .map(|_| ingress.request_snapshot().expect("fill ingress"))
        .collect::<Vec<_>>();
    let mut responders = JoinSet::new();
    let permits = Arc::new(Semaphore::new(PORT_QUEUE_CAPACITY));
    let (reply_sender, mut replies) = tokio_mpsc::channel(2);
    let reply_timeouts = Arc::new(AtomicU64::new(0));
    handle_incoming(
        &ingress,
        &mut responders,
        &permits,
        &reply_sender,
        &reply_timeouts,
        "comp-nested",
        local_set_command(3, "input.corners.dwell_ms", json!(250)),
    );
    let reply = replies.recv().await.expect("busy reply");
    assert_eq!(
        reply.body.as_ref(),
        "{\"error\":\"busy\",\"error_code\":\"busy\"}"
    );
    for _ in 0..PORT_QUEUE_CAPACITY {
        assert!(matches!(source.try_recv(), Ok(PortCommand::Snapshot(_))));
    }
}

#[tokio::test]
async fn publisher_gaps_each_topic_no_later_than_its_next_record_or_idle_flush() {
    let lost = Arc::new(AtomicU64::new(0));
    let (mut producer, receiver) = crate::outbox::test_outbox(Arc::clone(&lost), 2);
    let notifier = producer.notifier();
    producer.offer(ObservationRecord::PropsChanged {
        path: "input.corners.enabled".into(),
        old: PropValue::Bool(true),
        new: PropValue::Bool(false),
        unix_ms: 0,
        cause: "props.set",
        event_seq: 1,
    });
    for event_seq in 2..=4 {
        producer.offer(ObservationRecord::FocusChanged {
            keyboard: Some(event_seq),
            previous: None,
            exclusive_latch: None,
            event_seq,
        });
    }
    let (client, _commands, _states) = FakeClient::new(ConnState::Connected, false);
    let publications = Arc::clone(&client.publications);
    let publish_timeouts = Arc::new(AtomicU64::new(0));
    let (shutdown_tx, shutdown) = watch::channel(false);
    let task = tokio::spawn(publisher_loop(
        Arc::new(client),
        Arc::from("comp-nested"),
        receiver,
        notifier,
        Arc::clone(&lost),
        Arc::clone(&publish_timeouts),
        shutdown,
    ));
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if publications
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .len()
                >= 4
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("both bounded-lane survivors and both affected-topic gaps publish");
    shutdown_tx.send_replace(true);
    task.await.expect("publisher exits");

    let published = publications
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let first_survivor =
        bus::parse(&published[0].1).expect("first survivor parses");
    assert_eq!(
        published[0].0.get("name").map(String::as_str),
        Some("comp-nested.focus.changed")
    );
    assert_eq!(first_survivor.get("event_seq"), Some("3"));

    let focus_gap = bus::parse(&published[1].1).expect("focus gap parses");
    assert_eq!(
        published[1].0.get("name").map(String::as_str),
        Some("comp-nested.focus.changed")
    );
    assert_eq!(focus_gap.get("command"), Some("focus.changed"));
    assert_eq!(focus_gap.get("event_seq"), Some("2"));
    assert_eq!(
        serde_json::from_str::<Value>(&focus_gap.body).unwrap(),
        json!({"gap": true, "lost_count": 2, "cause": "outbox.overflow"})
    );

    let second_survivor =
        bus::parse(&published[2].1).expect("second survivor parses");
    assert_eq!(second_survivor.get("event_seq"), Some("4"));

    let props_gap = bus::parse(&published[3].1).expect("props gap parses");
    assert_eq!(
        published[3].0.get("name").map(String::as_str),
        Some("comp-nested.props.changed")
    );
    assert_eq!(props_gap.get("command"), Some("props.changed"));
    assert_eq!(props_gap.get("event_seq"), Some("2"));
    assert_eq!(
        serde_json::from_str::<Value>(&props_gap.body).unwrap(),
        json!({"gap": true, "lost_count": 2, "cause": "outbox.overflow"})
    );
    assert_eq!(publish_timeouts.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn consecutive_carried_intervals_coalesce_to_one_gap_before_the_survivor() {
    let lost = Arc::new(AtomicU64::new(0));
    let (mut producer, receiver) = crate::outbox::test_outbox(Arc::clone(&lost), 1);
    let notifier = producer.notifier();
    for event_seq in 1..=4 {
        producer.offer(ObservationRecord::FocusChanged {
            keyboard: Some(event_seq),
            previous: None,
            exclusive_latch: None,
            event_seq,
        });
    }
    let (client, _commands, _states) = FakeClient::new(ConnState::Connected, false);
    let publications = Arc::clone(&client.publications);
    let (shutdown_tx, shutdown) = watch::channel(false);
    let task = tokio::spawn(publisher_loop(
        Arc::new(client),
        Arc::from("comp-nested"),
        receiver,
        notifier,
        Arc::clone(&lost),
        Arc::new(AtomicU64::new(0)),
        shutdown,
    ));
    tokio::time::timeout(Duration::from_secs(1), async {
        while publications
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
            != 2
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("one coalesced gap and the sole survivor publish");
    shutdown_tx.send_replace(true);
    task.await.expect("publisher exits");

    let published = publications
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let gap = bus::parse(&published[0].1).expect("gap parses");
    assert_eq!(gap.get("event_seq"), Some("3"));
    assert_eq!(
        serde_json::from_str::<Value>(&gap.body).unwrap(),
        json!({"gap": true, "lost_count": 3, "cause": "outbox.overflow"})
    );
    let survivor = bus::parse(&published[1].1).expect("survivor parses");
    assert_eq!(survivor.get("event_seq"), Some("4"));
    assert_eq!(lost.load(Ordering::Acquire), 3);
}

#[tokio::test(start_paused = true)]
async fn failed_idle_gap_retries_on_broker_reconnect_without_a_new_record() {
    let lost = Arc::new(AtomicU64::new(0));
    let (mut producer, receiver) = crate::outbox::test_outbox(Arc::clone(&lost), 1);
    let notifier = producer.notifier();
    producer.offer(ObservationRecord::PropsChanged {
        path: "input.corners.enabled".into(),
        old: PropValue::Bool(true),
        new: PropValue::Bool(false),
        unix_ms: 0,
        cause: "props.set",
        event_seq: 1,
    });
    producer.offer(ObservationRecord::FocusChanged {
        keyboard: Some(2),
        previous: None,
        exclusive_latch: None,
        event_seq: 2,
    });
    notifier.notified().await;

    let (ingress, _source, _) = test_ingress();
    let broker = Arc::new(AtomicU8::new(BROKER_RETRYING));
    let reply_timeouts = Arc::new(AtomicU64::new(0));
    let publish_timeouts = Arc::new(AtomicU64::new(0));
    let (shutdown_tx, shutdown) = watch::channel(false);
    let (client, _commands, states) = FakeClient::new(ConnState::Connected, false);
    client.reject_publish_attempt.store(2, Ordering::Release);
    let publish_mode = Arc::clone(&client.publish_mode);
    let publications = Arc::clone(&client.publications);
    let mut client = Some(client);
    let worker = tokio::spawn(worker_loop(
        "comp-nested".into(),
        ingress,
        Arc::clone(&broker),
        reply_timeouts,
        Arc::clone(&publish_timeouts),
        receiver,
        notifier,
        Arc::clone(&lost),
        shutdown,
        move || future::ready(Ok(client.take().expect("one connection attempt"))),
    ));

    wait_for_broker(&broker, BROKER_CONNECTED).await;
    tokio::time::timeout(Duration::from_secs(1), async {
        while publish_timeouts.load(Ordering::Acquire) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the idle-flush props gap fails once");
    assert_eq!(
        publications
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len(),
        1,
        "only the focus survivor publishes before broker recovery"
    );

    publish_mode.store(1, Ordering::Release);
    states.send_replace(ConnState::Disconnected);
    wait_for_broker(&broker, BROKER_RETRYING).await;
    tokio::time::timeout(Duration::from_secs(1), async {
        while publish_timeouts.load(Ordering::Acquire) != 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the disconnected edge wakes the retained gap retry");

    publish_mode.store(0, Ordering::Release);
    states.send_replace(ConnState::Connected);
    wait_for_broker(&broker, BROKER_CONNECTED).await;
    tokio::time::timeout(Duration::from_secs(1), async {
        while publications
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
            != 2
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the reconnect edge publishes the retained gap without new data");

    shutdown_tx.send_replace(true);
    worker.await.expect("worker exits cleanly");
    let published = publications
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let survivor = bus::parse(&published[0].1).expect("survivor parses");
    assert_eq!(survivor.get("event_seq"), Some("2"));
    let gap = bus::parse(&published[1].1).expect("gap parses");
    assert_eq!(gap.get("command"), Some("props.changed"));
    assert_eq!(gap.get("event_seq"), Some("1"));
    assert_eq!(
        serde_json::from_str::<Value>(&gap.body).unwrap(),
        json!({"gap": true, "lost_count": 1, "cause": "outbox.overflow"})
    );
    assert_eq!(lost.load(Ordering::Acquire), 1);
}

#[tokio::test(start_paused = true)]
async fn failed_gap_after_event_sequence_exhaustion_retries_on_backoff_without_data() {
    let lost = Arc::new(AtomicU64::new(0));
    let (mut producer, receiver) = crate::outbox::test_outbox(Arc::clone(&lost), 1);
    let notifier = producer.notifier();
    producer.offer(ObservationRecord::PropsChanged {
        path: "input.corners.enabled".into(),
        old: PropValue::Bool(true),
        new: PropValue::Bool(false),
        unix_ms: 0,
        cause: "props.set",
        event_seq: u64::MAX - 1,
    });
    producer.offer(ObservationRecord::FocusChanged {
        keyboard: None,
        previous: Some(1),
        exclusive_latch: None,
        event_seq: u64::MAX,
    });
    notifier.notified().await;

    let (client, _commands, _states) = FakeClient::new(ConnState::Connected, false);
    client.reject_publish_attempt.store(2, Ordering::Release);
    let publish_attempts = Arc::clone(&client.publish_attempts);
    let publications = Arc::clone(&client.publications);
    let publish_timeouts = Arc::new(AtomicU64::new(0));
    let (shutdown_tx, shutdown) = watch::channel(false);
    let task = tokio::spawn(publisher_loop(
        Arc::new(client),
        Arc::from("comp-nested"),
        receiver,
        notifier,
        Arc::clone(&lost),
        Arc::clone(&publish_timeouts),
        shutdown,
    ));
    while publish_timeouts.load(Ordering::Acquire) != 1 {
        tokio::task::yield_now().await;
    }
    tokio::task::yield_now().await;
    assert_eq!(publish_attempts.load(Ordering::Acquire), 2);
    assert_eq!(
        publications
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len(),
        1
    );

    tokio::time::advance(Duration::from_millis(999)).await;
    tokio::task::yield_now().await;
    assert_eq!(
        publish_attempts.load(Ordering::Acquire),
        2,
        "the failed gap waits for its one-second first backoff"
    );
    tokio::time::advance(Duration::from_millis(1)).await;
    while publications
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .len()
        != 2
    {
        tokio::task::yield_now().await;
    }

    shutdown_tx.send_replace(true);
    task.await.expect("publisher exits");
    let published = publications
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let exhausted_sequence = u64::MAX.to_string();
    let last_lost_sequence = (u64::MAX - 1).to_string();
    let survivor = bus::parse(&published[0].1).expect("survivor parses");
    assert_eq!(survivor.get("event_seq"), Some(exhausted_sequence.as_str()));
    let gap = bus::parse(&published[1].1).expect("gap parses");
    assert_eq!(gap.get("command"), Some("props.changed"));
    assert_eq!(gap.get("event_seq"), Some(last_lost_sequence.as_str()));
    assert_eq!(publish_attempts.load(Ordering::Acquire), 3);
    assert_eq!(publish_timeouts.load(Ordering::Acquire), 1);
    assert_eq!(lost.load(Ordering::Acquire), 1);
}

#[tokio::test]
#[should_panic(expected = "observation producer disconnected before port shutdown")]
async fn observation_lane_disconnect_without_shutdown_violates_lifecycle() {
    let lost = Arc::new(AtomicU64::new(0));
    let (producer, receiver) = crate::outbox::outbox(Arc::clone(&lost), Arc::new(AtomicU64::new(0)));
    let notifier = producer.notifier();
    drop(producer);
    let (client, _commands, _states) = FakeClient::new(ConnState::Connected, false);
    let (_shutdown_tx, shutdown) = watch::channel(false);
    publisher_loop(
        Arc::new(client),
        Arc::from("comp-nested"),
        receiver,
        notifier,
        lost,
        Arc::new(AtomicU64::new(0)),
        shutdown,
    )
    .await;
}

#[test]
fn failed_gap_retry_backoff_doubles_and_caps_at_thirty_seconds() {
    let mut delay = None;
    for expected in [1, 2, 4, 8, 16, 30, 30] {
        arm_gap_retry(&mut delay);
        assert_eq!(delay, Some(Duration::from_secs(expected)));
    }
}

#[tokio::test]
async fn successful_gap_topics_are_not_republished_after_a_later_gap_fails() {
    let lost = Arc::new(AtomicU64::new(0));
    let (mut producer, receiver) = crate::outbox::test_outbox(Arc::clone(&lost), 2);
    let notifier = producer.notifier();
    producer.offer(ObservationRecord::PropsChanged {
        path: "input.corners.enabled".into(),
        old: PropValue::Bool(true),
        new: PropValue::Bool(false),
        unix_ms: 0,
        cause: "props.set",
        event_seq: 1,
    });
    producer.offer(ObservationRecord::FocusChanged {
        keyboard: Some(2),
        previous: None,
        exclusive_latch: None,
        event_seq: 2,
    });
    for event_seq in 3..=4 {
        producer.offer(ObservationRecord::SurfaceMapped {
            id: event_seq,
            role: "toplevel".into(),
            foreign_id: None,
            window: Default::default(),
            event_seq,
        });
    }
    let (client, _commands, _states) = FakeClient::new(ConnState::Connected, false);
    client.reject_publish_attempt.store(4, Ordering::Release);
    let publications = Arc::clone(&client.publications);
    let publish_timeouts = Arc::new(AtomicU64::new(0));
    let (shutdown_tx, shutdown) = watch::channel(false);
    let task = tokio::spawn(publisher_loop(
        Arc::new(client),
        Arc::from("comp-nested"),
        receiver,
        notifier,
        Arc::clone(&lost),
        Arc::clone(&publish_timeouts),
        shutdown,
    ));
    tokio::time::timeout(Duration::from_secs(1), async {
        while publish_timeouts.load(Ordering::Acquire) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the later idle-flush focus gap fails after the props gap succeeds");
    producer.offer(ObservationRecord::FocusChanged {
        keyboard: Some(5),
        previous: Some(4),
        exclusive_latch: None,
        event_seq: 5,
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        while publications
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
            != 5
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("remaining focus gap and survivor publish");
    shutdown_tx.send_replace(true);
    task.await.expect("publisher exits");

    let published = publications
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert_eq!(
        published
            .iter()
            .filter(|(headers, _)| headers
                .get("name")
                .is_some_and(|name| name.ends_with("props.changed")))
            .count(),
        1,
        "the successful props gap is acknowledged before the focus gap fails"
    );
    assert_eq!(lost.load(Ordering::Acquire), 2);
    let first_survivor =
        bus::parse(&published[0].1).expect("first survivor parses");
    assert_eq!(first_survivor.get("event_seq"), Some("3"));
    let second_survivor =
        bus::parse(&published[1].1).expect("second survivor parses");
    assert_eq!(second_survivor.get("event_seq"), Some("4"));
    let props_gap = bus::parse(&published[2].1).expect("props gap parses");
    assert_eq!(props_gap.get("event_seq"), Some("2"));
    let focus_gap = bus::parse(&published[3].1).expect("focus gap parses");
    assert_eq!(focus_gap.get("event_seq"), Some("2"));
    assert_eq!(
        serde_json::from_str::<Value>(&focus_gap.body).unwrap(),
        json!({"gap": true, "lost_count": 2, "cause": "outbox.overflow"})
    );
    let survivor = bus::parse(&published[4].1).expect("survivor parses");
    assert_eq!(survivor.get("event_seq"), Some("5"));
    assert_eq!(publish_timeouts.load(Ordering::Acquire), 1);
}

#[tokio::test(start_paused = true)]
async fn idle_publisher_has_no_retry_timer_when_nothing_is_pending() {
    let lost = Arc::new(AtomicU64::new(0));
    let (mut producer, receiver) = crate::outbox::outbox(Arc::clone(&lost), Arc::new(AtomicU64::new(0)));
    let notifier = producer.notifier();
    let (client, _commands, _states) = FakeClient::new(ConnState::Connected, false);
    let publish_attempts = Arc::clone(&client.publish_attempts);
    let publications = Arc::clone(&client.publications);
    let (shutdown_tx, shutdown) = watch::channel(false);
    let task = tokio::spawn(publisher_loop(
        Arc::new(client),
        Arc::from("comp-nested"),
        receiver,
        notifier,
        lost,
        Arc::new(AtomicU64::new(0)),
        shutdown,
    ));
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(3_600)).await;
    tokio::task::yield_now().await;
    assert_eq!(publish_attempts.load(Ordering::Acquire), 0);
    assert!(
        publications
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_empty()
    );
    tokio::time::advance(Duration::from_secs(3_600)).await;
    tokio::task::yield_now().await;
    assert_eq!(
        publish_attempts.load(Ordering::Acquire),
        0,
        "idle publisher performs no timer-driven publication attempt"
    );
    assert!(
        publications
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_empty(),
        "idle publisher has no timer or polling wake"
    );

    producer.offer(ObservationRecord::FocusChanged {
        keyboard: Some(1),
        previous: None,
        exclusive_latch: None,
        event_seq: 1,
    });
    tokio::time::timeout(Duration::from_millis(1), async {
        while publications
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_empty()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("successful offer wakes idle publisher without advancing time");
    shutdown_tx.send_replace(true);
    task.await.expect("publisher exits");
}

#[tokio::test]
async fn rejected_publication_gaps_every_topic_in_the_discarded_backlog() {
    let lost = Arc::new(AtomicU64::new(0));
    let (mut producer, receiver) = crate::outbox::outbox(Arc::clone(&lost), Arc::new(AtomicU64::new(0)));
    let notifier = producer.notifier();
    producer.offer(ObservationRecord::PropsChanged {
        path: "input.corners.enabled".into(),
        old: PropValue::Bool(true),
        new: PropValue::Bool(false),
        unix_ms: 0,
        cause: "props.set",
        event_seq: 1,
    });
    producer.offer(ObservationRecord::FocusChanged {
        keyboard: Some(2),
        previous: Some(1),
        exclusive_latch: None,
        event_seq: 2,
    });
    let (client, _commands, _states) = FakeClient::new(ConnState::Connected, false);
    client.publish_mode.store(1, Ordering::Release);
    let mode = Arc::clone(&client.publish_mode);
    let publications = Arc::clone(&client.publications);
    let publish_timeouts = Arc::new(AtomicU64::new(0));
    let (shutdown_tx, shutdown) = watch::channel(false);
    let task = tokio::spawn(publisher_loop(
        Arc::new(client),
        Arc::from("comp-nested"),
        receiver,
        notifier,
        Arc::clone(&lost),
        Arc::clone(&publish_timeouts),
        shutdown,
    ));
    tokio::time::timeout(Duration::from_secs(1), async {
        while lost.load(Ordering::Acquire) != 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("failed record and backlog become loss");
    mode.store(0, Ordering::Release);
    producer.offer(ObservationRecord::FocusChanged {
        keyboard: Some(3),
        previous: Some(2),
        exclusive_latch: None,
        event_seq: 3,
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        while publications
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
            != 3
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("publisher-loss gaps and survivor publish");
    shutdown_tx.send_replace(true);
    task.await.expect("publisher exits");
    let published = publications
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let focus_gap = bus::parse(&published[0].1).expect("focus gap parses");
    assert_eq!(
        published[0].0.get("name").map(String::as_str),
        Some("comp-nested.focus.changed")
    );
    assert_eq!(focus_gap.get("event_seq"), Some("2"));
    assert_eq!(
        serde_json::from_str::<Value>(&focus_gap.body).unwrap(),
        json!({"gap": true, "lost_count": 2, "cause": "publisher.loss"})
    );
    let survivor = bus::parse(&published[1].1).expect("survivor parses");
    assert_eq!(survivor.get("command"), Some("focus.changed"));
    assert_eq!(survivor.get("event_seq"), Some("3"));
    let props_gap = bus::parse(&published[2].1).expect("props gap parses");
    assert_eq!(
        published[2].0.get("name").map(String::as_str),
        Some("comp-nested.props.changed")
    );
    assert_eq!(props_gap.get("event_seq"), Some("2"));
    assert_eq!(
        serde_json::from_str::<Value>(&props_gap.body).unwrap(),
        json!({"gap": true, "lost_count": 2, "cause": "publisher.loss"})
    );
    assert_eq!(publish_timeouts.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn overflow_after_failed_backlog_drain_still_gaps_its_topics() {
    let lost = Arc::new(AtomicU64::new(0));
    let (mut producer, receiver) = crate::outbox::test_outbox(Arc::clone(&lost), 2);
    let notifier = producer.notifier();
    producer.offer(ObservationRecord::FocusChanged {
        keyboard: Some(1),
        previous: None,
        exclusive_latch: None,
        event_seq: 1,
    });
    producer.offer(ObservationRecord::FocusChanged {
        keyboard: Some(2),
        previous: Some(1),
        exclusive_latch: None,
        event_seq: 2,
    });
    let (client, _commands, _states) = FakeClient::new(ConnState::Connected, false);
    client.publish_mode.store(1, Ordering::Release);
    let mode = Arc::clone(&client.publish_mode);
    let publications = Arc::clone(&client.publications);
    let publish_timeouts = Arc::new(AtomicU64::new(0));
    let (shutdown_tx, shutdown) = watch::channel(false);
    let task = tokio::spawn(publisher_loop(
        Arc::new(client),
        Arc::from("comp-nested"),
        receiver,
        notifier,
        Arc::clone(&lost),
        Arc::clone(&publish_timeouts),
        shutdown,
    ));
    tokio::time::timeout(Duration::from_secs(1), async {
        while lost.load(Ordering::Acquire) != 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("failed publication drains the first backlog");

    mode.store(0, Ordering::Release);
    producer.offer(ObservationRecord::PropsChanged {
        path: "input.corners.enabled".into(),
        old: PropValue::Bool(true),
        new: PropValue::Bool(false),
        unix_ms: 0,
        cause: "props.set",
        event_seq: 3,
    });
    producer.offer(ObservationRecord::FocusChanged {
        keyboard: Some(4),
        previous: Some(2),
        exclusive_latch: None,
        event_seq: 4,
    });
    producer.offer(ObservationRecord::FocusChanged {
        keyboard: Some(5),
        previous: Some(4),
        exclusive_latch: None,
        event_seq: 5,
    });

    tokio::time::timeout(Duration::from_secs(1), async {
        while publications
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
            < 4
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("publisher-loss and carried overflow gaps both publish");
    shutdown_tx.send_replace(true);
    task.await.expect("publisher exits");

    let published = publications
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let focus_gap = bus::parse(&published[0].1).expect("focus gap parses");
    assert_eq!(focus_gap.get("command"), Some("focus.changed"));
    assert_eq!(focus_gap.get("event_seq"), Some("2"));
    assert_eq!(
        serde_json::from_str::<Value>(&focus_gap.body).unwrap(),
        json!({"gap": true, "lost_count": 3, "cause": "publisher.loss"})
    );
    let first_survivor = bus::parse(&published[1].1).expect("survivor parses");
    assert_eq!(first_survivor.get("event_seq"), Some("4"));
    let second_survivor =
        bus::parse(&published[2].1).expect("second survivor parses");
    assert_eq!(second_survivor.get("event_seq"), Some("5"));
    let props_gap = bus::parse(&published[3].1).expect("props gap parses");
    assert_eq!(props_gap.get("command"), Some("props.changed"));
    assert_eq!(props_gap.get("event_seq"), Some("3"));
    assert_eq!(
        serde_json::from_str::<Value>(&props_gap.body).unwrap(),
        json!({"gap": true, "lost_count": 3, "cause": "outbox.overflow"})
    );
    assert_eq!(publish_timeouts.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn rejected_gap_discards_its_survivor_and_recovers_as_publisher_loss() {
    let lost = Arc::new(AtomicU64::new(0));
    let (mut producer, receiver) = crate::outbox::outbox(Arc::clone(&lost), Arc::new(AtomicU64::new(0)));
    let notifier = producer.notifier();
    for sequence in 1..=3 {
        producer.offer(ObservationRecord::FocusChanged {
            keyboard: Some(sequence),
            previous: None,
            exclusive_latch: None,
            event_seq: sequence,
        });
    }
    let (client, _commands, _states) = FakeClient::new(ConnState::Connected, false);
    client.publish_mode.store(1, Ordering::Release);
    let mode = Arc::clone(&client.publish_mode);
    let publications = Arc::clone(&client.publications);
    let publish_timeouts = Arc::new(AtomicU64::new(0));
    let (shutdown_tx, shutdown) = watch::channel(false);
    let task = tokio::spawn(publisher_loop(
        Arc::new(client),
        Arc::from("comp-nested"),
        receiver,
        notifier,
        Arc::clone(&lost),
        Arc::clone(&publish_timeouts),
        shutdown,
    ));
    tokio::time::timeout(Duration::from_secs(1), async {
        while lost.load(Ordering::Acquire) != 3 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("failed gap discards every surviving record");
    mode.store(0, Ordering::Release);
    producer.offer(ObservationRecord::FocusChanged {
        keyboard: Some(4),
        previous: Some(3),
        exclusive_latch: None,
        event_seq: 4,
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        while publications
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
            != 2
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("replacement gap and survivor publish");
    shutdown_tx.send_replace(true);
    task.await.expect("publisher exits");
    let published = publications
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let gap = bus::parse(&published[0].1).expect("gap parses");
    assert_eq!(gap.get("event_seq"), Some("3"));
    assert_eq!(
        serde_json::from_str::<Value>(&gap.body).unwrap(),
        json!({"gap": true, "lost_count": 3, "cause": "publisher.loss"})
    );
    assert_eq!(publish_timeouts.load(Ordering::Acquire), 1);
}

#[tokio::test(start_paused = true)]
async fn stalled_publication_times_out_without_blocking_shutdown() {
    let lost = Arc::new(AtomicU64::new(0));
    let (mut producer, receiver) = crate::outbox::outbox(Arc::clone(&lost), Arc::new(AtomicU64::new(0)));
    let notifier = producer.notifier();
    producer.offer(ObservationRecord::FocusChanged {
        keyboard: None,
        previous: Some(1),
        exclusive_latch: None,
        event_seq: 1,
    });
    let (client, _commands, _states) = FakeClient::new(ConnState::Connected, false);
    client.publish_mode.store(2, Ordering::Release);
    let publish_timeouts = Arc::new(AtomicU64::new(0));
    let (shutdown_tx, shutdown) = watch::channel(false);
    let task = tokio::spawn(publisher_loop(
        Arc::new(client),
        Arc::from("comp-nested"),
        receiver,
        notifier,
        Arc::clone(&lost),
        Arc::clone(&publish_timeouts),
        shutdown,
    ));
    tokio::task::yield_now().await;
    tokio::time::advance(PUBLISH_TIMEOUT).await;
    tokio::task::yield_now().await;
    assert_eq!(publish_timeouts.load(Ordering::Acquire), 1);
    assert_eq!(lost.load(Ordering::Acquire), 1);
    shutdown_tx.send_replace(true);
    tokio::time::timeout(Duration::from_millis(1), task)
        .await
        .expect("publisher shutdown is bounded")
        .expect("publisher exits");
}

#[tokio::test(start_paused = true)]
async fn stalled_publisher_does_not_block_commands_and_worker_shutdown_is_bounded() {
    let (ingress, _source, _) = test_ingress();
    let broker = Arc::new(AtomicU8::new(BROKER_RETRYING));
    let reply_timeouts = Arc::new(AtomicU64::new(0));
    let publish_timeouts = Arc::new(AtomicU64::new(0));
    let lost = Arc::new(AtomicU64::new(0));
    let (mut producer, observations) = crate::outbox::outbox(Arc::clone(&lost), Arc::new(AtomicU64::new(0)));
    let notifier = producer.notifier();
    producer.offer(ObservationRecord::FocusChanged {
        keyboard: Some(1),
        previous: None,
        exclusive_latch: None,
        event_seq: 1,
    });
    let (client, commands, _states) = FakeClient::new(ConnState::Connected, false);
    client.publish_mode.store(2, Ordering::Release);
    let responses_started = Arc::clone(&client.responses_started);
    let mut client = Some(client);
    let (shutdown_tx, shutdown) = watch::channel(false);
    let worker = tokio::spawn(worker_loop(
        "comp-nested".into(),
        ingress,
        Arc::clone(&broker),
        reply_timeouts,
        publish_timeouts,
        observations,
        notifier,
        lost,
        shutdown,
        move || future::ready(Ok(client.take().expect("one connection attempt"))),
    ));
    wait_for_broker(&broker, BROKER_CONNECTED).await;
    commands.send(command("comp.ping", 1)).expect("worker live");
    wait_for_counter(&responses_started, 1, "ping replied while publish hangs").await;
    shutdown_tx.send_replace(true);
    tokio::time::timeout(Duration::from_millis(1), worker)
        .await
        .expect("worker shutdown does not await publisher deadline")
        .expect("worker exits");
}

#[tokio::test]
async fn bounded_ingress_releases_depth_through_production_admission_completion() {
    let (ingress, source, queue_depth) = test_ingress();
    let mut admissions = Vec::new();
    for _ in 0..PORT_QUEUE_CAPACITY {
        admissions.push(ingress.request_snapshot().expect("request admitted"));
    }
    assert!(ingress.request_snapshot().is_err());
    assert_eq!(queue_depth.load(Ordering::Acquire), PORT_QUEUE_CAPACITY);

    for _ in 0..PORT_QUEUE_CAPACITY {
        let PortCommand::Snapshot(request) = source.try_recv().expect("staged request") else {
            panic!("snapshot request expected");
        };
        drop(request);
    }
    for admission in admissions {
        assert!(admission.receive().await.is_err());
    }
    assert_eq!(queue_depth.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn completed_responders_are_reaped_and_seventeenth_command_is_admitted() {
    let (ingress, source, queue_depth) = test_ingress();
    let broker = Arc::new(AtomicU8::new(BROKER_RETRYING));
    let reply_timeouts = Arc::new(AtomicU64::new(0));
    let (shutdown_tx, shutdown) = watch::channel(false);
    let (client, commands, _states) = FakeClient::new(ConnState::Connected, false);
    let mut client = Some(client);
    let (publish_timeouts, observations, lost_count) = test_observation_args();
    let worker = tokio::spawn(worker_loop(
        "comp-nested".into(),
        ingress,
        Arc::clone(&broker),
        reply_timeouts,
        publish_timeouts,
        observations,
        Arc::new(tokio::sync::Notify::new()),
        lost_count,
        shutdown,
        move || future::ready(Ok(client.take().expect("one connection attempt"))),
    ));
    wait_for_broker(&broker, BROKER_CONNECTED).await;

    for id in 0..PORT_QUEUE_CAPACITY {
        commands
            .send(command("comp.info", id))
            .expect("worker live");
    }
    for _ in 0..PORT_QUEUE_CAPACITY {
        let PortCommand::Snapshot(request) = next_port_command(&source).await else {
            panic!("snapshot request expected");
        };
        drop(request);
    }
    tokio::time::timeout(Duration::from_secs(1), async {
        while queue_depth.load(Ordering::Acquire) != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("production responder completion releases all admissions");

    for id in 100..164 {
        commands
            .send(command("comp.ping", id))
            .expect("worker live");
    }
    commands
        .send(command("comp.info", PORT_QUEUE_CAPACITY))
        .expect("worker live");
    let PortCommand::Snapshot(request) = next_port_command(&source).await else {
        panic!("snapshot request expected");
    };
    drop(request);

    shutdown_tx.send_replace(true);
    worker.await.expect("worker exits cleanly");
}

#[tokio::test(start_paused = true)]
async fn worker_loop_abandons_black_holed_replies_and_stays_responsive() {
    let (ingress, source, queue_depth) = test_ingress();
    let broker = Arc::new(AtomicU8::new(BROKER_RETRYING));
    let reply_timeouts = Arc::new(AtomicU64::new(0));
    let (shutdown_tx, shutdown) = watch::channel(false);
    let (client, commands, _states) = FakeClient::new(ConnState::Connected, true);
    let responses_started = Arc::clone(&client.responses_started);
    let mut client = Some(client);
    let (publish_timeouts, observations, lost_count) = test_observation_args();
    let worker = tokio::spawn(worker_loop(
        "comp-nested".into(),
        ingress,
        Arc::clone(&broker),
        Arc::clone(&reply_timeouts),
        publish_timeouts,
        observations,
        Arc::new(tokio::sync::Notify::new()),
        lost_count,
        shutdown,
        move || future::ready(Ok(client.take().expect("one connection attempt"))),
    ));
    wait_for_broker(&broker, BROKER_CONNECTED).await;

    commands
        .send(command("comp.ping", 1))
        .expect("worker admits ping");
    commands
        .send(command("comp.unknown", 2))
        .expect("worker admits error reply");
    wait_for_counter(&responses_started, 1, "first reply send starts").await;
    assert_eq!(queue_depth.load(Ordering::Acquire), 0);

    tokio::time::advance(REPLY_SEND_TIMEOUT).await;
    wait_for_counter(&responses_started, 2, "second reply send starts").await;
    assert_eq!(reply_timeouts.load(Ordering::Acquire), 1);
    tokio::time::advance(REPLY_SEND_TIMEOUT).await;
    tokio::task::yield_now().await;
    assert_eq!(reply_timeouts.load(Ordering::Acquire), 2);
    assert_eq!(queue_depth.load(Ordering::Acquire), 0);

    commands
        .send(command("comp.info", 3))
        .expect("worker remains responsive");
    let PortCommand::Snapshot(request) = next_port_command(&source).await else {
        panic!("snapshot request expected");
    };
    assert_eq!(queue_depth.load(Ordering::Acquire), 1);
    drop(request);
    wait_for_counter(&queue_depth, 0, "later admission releases depth").await;
    wait_for_counter(&responses_started, 3, "later error reply send starts").await;
    tokio::time::advance(REPLY_SEND_TIMEOUT).await;
    tokio::task::yield_now().await;
    assert_eq!(reply_timeouts.load(Ordering::Acquire), 3);

    shutdown_tx.send_replace(true);
    worker.await.expect("worker exits cleanly");
}

#[tokio::test(start_paused = true)]
async fn reply_sender_abandons_black_holed_reply_after_deadline_and_counts_it() {
    let (client, _commands, _states) = FakeClient::new(ConnState::Connected, true);
    let client = Arc::new(client);
    let reply_timeouts = Arc::new(AtomicU64::new(0));
    let (sender, receiver) = tokio_mpsc::channel(1);
    let task = tokio::spawn(reply_loop(
        client,
        Arc::from("comp-nested"),
        receiver,
        Arc::clone(&reply_timeouts),
    ));
    sender
        .send(PendingReply::new(
            command("comp.ping", 1),
            (0, Arc::from("{}")),
        ))
        .await
        .expect("reply lane open");
    tokio::task::yield_now().await;
    tokio::time::advance(REPLY_SEND_TIMEOUT).await;
    tokio::task::yield_now().await;
    assert_eq!(reply_timeouts.load(Ordering::Acquire), 1);
    drop(sender);
    task.await.expect("reply sender exits");
}

#[tokio::test]
async fn graceful_shutdown_deregisters_then_closes_when_broker_answers() {
    let (client, _commands, _states) = FakeClient::new(ConnState::Connected, false);
    let deregistered = Arc::clone(&client.deregistered);
    let closed = Arc::clone(&client.closed);

    graceful_client_shutdown(&client).await;

    assert_eq!(deregistered.load(Ordering::Acquire), 1);
    assert_eq!(closed.load(Ordering::Acquire), 1);
}

#[tokio::test(start_paused = true)]
async fn graceful_shutdown_closes_within_budget_when_deregister_hangs() {
    let (client, _commands, _states) = FakeClient::new(ConnState::Connected, false);
    client.deregister_hangs.store(true, Ordering::Release);
    let deregistered = Arc::clone(&client.deregistered);
    let closed = Arc::clone(&client.closed);
    let shutdown = tokio::spawn(async move {
        graceful_client_shutdown(&client).await;
    });
    tokio::task::yield_now().await;
    tokio::time::advance(CLIENT_SHUTDOWN_BUDGET).await;

    shutdown.await.expect("bounded shutdown completes");
    assert_eq!(deregistered.load(Ordering::Acquire), 1);
    assert_eq!(closed.load(Ordering::Acquire), 1);
}

/// `compd.truth` is routed before `classify` (which would answer it
/// `unknown_verb`), admitted as an ordered control, and answered with the
/// engine's body; a near-miss name stays `unknown_verb`.
#[tokio::test]
async fn compd_truth_is_admitted_as_a_control_and_near_misses_stay_unknown() {
    let (ingress, source, _) = test_ingress();
    let mut responders = JoinSet::new();
    let permits = Arc::new(Semaphore::new(PORT_QUEUE_CAPACITY));
    let (reply_sender, mut replies) = tokio_mpsc::channel(2);
    let reply_timeouts = Arc::new(AtomicU64::new(0));
    handle_incoming(
        &ingress,
        &mut responders,
        &permits,
        &reply_sender,
        &reply_timeouts,
        "comp-nested",
        command(TRUTH_VERB, 1),
    );
    let Ok(PortCommand::Truth(request)) = source.try_recv() else {
        panic!("compd.truth was not admitted as a truth control");
    };
    let _ = request.reply.send(ControlReply::Body(json!({"revision": 3})));
    let reply = replies.recv().await.expect("truth reply queued");
    assert_eq!(reply.rc, 0);
    assert_eq!(reply.body.as_ref(), "{\"revision\":3}");

    handle_incoming(
        &ingress,
        &mut responders,
        &permits,
        &reply_sender,
        &reply_timeouts,
        "comp-nested",
        command("comp.truth", 2),
    );
    let refused = replies.recv().await.expect("unknown verb refused");
    assert_eq!(refused.rc, 10);
    assert!(refused.body.contains("unknown_verb"), "{}", refused.body);
    assert!(source.try_recv().is_err(), "a near miss is never admitted");
    responders.abort_all();
}

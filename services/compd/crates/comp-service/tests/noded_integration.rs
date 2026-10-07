// The port against a real noded. Ignored by default; run on
// a host with a broker:
//
//   cargo test -p comp-service --test noded_integration -- --ignored --nocapture
//
// `MIXOS_NODED_URL` picks the broker (default ws://127.0.0.1:4200/ws) and
// `COMPD_ITEST_SERVICE` the registered name (default `compd-itest`, so a
// live comp / comp-nested is never displaced). Verbs stay literal `comp.*`
// whatever the service is registered as.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use bus::SupervisedClient;
use comp_model::observation::{CornerConfig, ObservationRecord, PanelRequest, PropValue};
use comp_model::reply::ControlReply;
use comp_model::request::{InputOp, LongOp, SelectionIdentity, WindowOp};
use comp_model::snapshot::{
    BindingsSnapshot, CompSnapshot, DecorationSnapshot, FocusSnapshot, FocusWindowSnapshot,
    FullTreeCache, InfoSnapshot, InputSnapshot, ReadScopes, WorkspacesSnapshot, XwaylandSnapshot,
};
use comp_service::{CompEngine, LongReply, PortContext, PortIdentity, default_noded_url, prepare};

const DEADLINE: Duration = Duration::from_secs(10);

struct Engine {
    context: Arc<PortContext>,
}

impl CompEngine for Engine {
    fn snapshot(&mut self, _scopes: &ReadScopes) -> Option<CompSnapshot> {
        let context = &self.context;
        Some(CompSnapshot {
            occlusion: Default::default(),
            info: InfoSnapshot {
                service: Arc::clone(&context.service),
                version: Arc::clone(&context.version),
                backend: context.backend,
                engine: context.engine,
                instance: Arc::clone(&context.instance),
                explicit_sync_advertised: false,
                explicit_sync_healthy: true,
            },
            outputs: Default::default(),
            surfaces: Default::default(),
            windows: Default::default(),
            workspaces: WorkspacesSnapshot {
                count: 4,
                current: 1,
                outputs: Default::default(),
                list: Vec::new(),
            },
            sources: Default::default(),
            stack: Vec::new(),
            focus: FocusSnapshot {
                keyboard: None,
                exclusive_latch: None,
                pointer: None,
                pointer_grab: "none",
                session_lock: "none",
                window: FocusWindowSnapshot::default(),
            },
            decoration: DecorationSnapshot {
                enabled: true,
                style: "mac",
            },
            bindings: BindingsSnapshot {
                enabled: true,
                profile: "nested",
                table: Vec::new(),
            },
            input: InputSnapshot {
                seats: None,
                last_origin: None,
                corners: CornerConfig::default().into(),
                host: None,
            },
            xwayland: XwaylandSnapshot {
                enabled: false,
                persist_path: Arc::from("/tmp/compd-itest-xwayland"),
                display: None,
                state: "off",
                failures: 0,
            },
            dmabuf: Default::default(),
            port: context.port_snapshot(0),
            full_tree: FullTreeCache::default(),
        })
    }
    fn set(&mut self, _path: &str, _value: &Value, _generation: Option<u64>) -> ControlReply {
        ControlReply::refused("busy", Value::Null)
    }
    fn window(&mut self, _op: &WindowOp) -> ControlReply {
        ControlReply::refused("busy", Value::Null)
    }
    fn input(&mut self, _op: &InputOp) -> ControlReply {
        ControlReply::refused("busy", Value::Null)
    }
    fn panel(&mut self, _request: &PanelRequest) -> ControlReply {
        ControlReply::refused("busy", Value::Null)
    }
    fn region_cancel(&mut self, _selection: &SelectionIdentity) -> ControlReply {
        ControlReply::refused("busy", Value::Null)
    }
    fn start_long(&mut self, _op: LongOp, reply: LongReply, _admitted: Instant) {
        reply.send(ControlReply::refused("busy", Value::Null));
    }
    fn services_live(&mut self, _live: &std::collections::BTreeSet<String>) {}
    fn watch_props(&mut self, _active: bool) -> bool {
        true
    }
    fn renew_pointer_lease(&mut self) {}
}

async fn eventually<F, Fut>(what: &str, mut attempt: F) -> Value
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<Value>>,
{
    let start = Instant::now();
    loop {
        if let Some(value) = attempt().await {
            return value;
        }
        assert!(
            start.elapsed() < DEADLINE,
            "{what}: no answer within {DEADLINE:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
#[ignore = "needs a running noded with a broker"]
async fn comp_port_registers_answers_and_publishes_through_noded() {
    let service = std::env::var("COMPD_ITEST_SERVICE").unwrap_or_else(|_| "compd-itest".into());
    let url = default_noded_url();

    // Engine thread: services the port only when the waker fires.
    let (wake, woken) = mpsc::channel::<()>();
    let waker: comp_service::Waker = Arc::new(move || {
        let _ = wake.send(());
    });
    let (wiring, starter) = prepare(
        PortIdentity {
            service: service.clone(),
            version: env!("CARGO_PKG_VERSION").into(),
            backend: "nested",
            engine: "itest",
            noded_url: url.clone(),
        },
        waker,
    )
    .expect("prepare");
    let comp_service::PortWiring {
        service: mut port,
        observation_producer: mut producer,
        context,
    } = wiring;
    let mut worker = starter.start().expect("start the port worker");
    let stop = Arc::new(AtomicBool::new(false));
    let engine_stop = Arc::clone(&stop);
    let engine_context = Arc::clone(&context);
    let engine_thread = thread::spawn(move || {
        let mut engine = Engine {
            context: engine_context,
        };
        while !engine_stop.load(Ordering::Acquire) {
            match woken.recv_timeout(Duration::from_millis(200)) {
                Ok(()) => {
                    port.service(&mut engine);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    });

    let client = SupervisedClient::connect_options("compd-itest-client", &url)
        .connect()
        .await
        .expect("connect the test client");

    // Registration is asynchronous: ping until the service answers.
    let (caller, name) = (&client, service.as_str());
    let pong = eventually("comp.ping", || async move {
        caller.call(name, "comp.ping", json!({})).await.ok()
    })
    .await;
    assert_eq!(pong, json!({"pong": true}));

    let info = client
        .call(&service, "comp.info", json!({}))
        .await
        .expect("comp.info");
    assert_eq!(info["service"], json!(service), "comp.info: {info}");
    assert_eq!(info["engine"], "itest", "comp.info: {info}");

    let got = client
        .call(&service, "comp.props.get", json!({"path": "info.service"}))
        .await
        .expect("comp.props.get");
    assert!(got.to_string().contains(&service), "comp.props.get: {got}");

    // A refused control (rc 10) reaches the caller with its error text.
    let refused = client
        .call(&service, "comp.window.restore", json!({}))
        .await
        .expect_err("the fake engine refuses window verbs");
    assert!(refused.to_string().contains("busy"), "{refused}");

    // Topic publish: subscribe, let noded announce topic.active, offer one
    // record, and watch it arrive with the topic header.
    let topic = format!("{service}.props.changed");
    let mut incoming = client.incoming().expect("the client's incoming stream");
    client.subscribe_topic(&topic).await.expect("subscribe");
    let watch = client
        .call(&service, "comp.props.watch", json!({}))
        .await
        .expect("comp.props.watch");
    assert_eq!(watch["topic"], json!(topic), "comp.props.watch: {watch}");
    tokio::time::sleep(Duration::from_millis(300)).await;
    let seq = producer
        .offer_next(|event_seq| ObservationRecord::PropsChanged {
            path: "decoration.style".into(),
            old: PropValue::String("mac".into()),
            new: PropValue::String("win".into()),
            unix_ms: 0,
            cause: "itest",
            event_seq,
        })
        .expect("the outbox accepts the record");
    let delivery = tokio::time::timeout(DEADLINE, async {
        while let Some(message) = incoming.recv().await {
            if message.headers.get("topic").map(String::as_str) == Some(topic.as_str()) {
                return message;
            }
        }
        panic!("incoming stream closed before the topic delivery");
    })
    .await
    .expect("a props.changed delivery within the deadline");
    assert!(
        delivery.body.contains("decoration.style"),
        "{}",
        delivery.body
    );
    assert!(
        delivery.body.contains(&seq.to_string()),
        "{}",
        delivery.body
    );

    // Graceful shutdown deregisters; the name is free again.
    worker.begin_shutdown();
    worker.finish();
    stop.store(true, Ordering::Release);
    engine_thread.join().expect("engine thread");
    let gone = eventually("deregistration", || async move {
        // `noded.list` answers a list of service names or `{name, …}` records.
        let listed = caller.call("noded", "noded.list", Value::Null).await.ok()?;
        let live = listed.as_array()?.iter().any(|entry| {
            entry
                .as_str()
                .or_else(|| entry.get("name").and_then(Value::as_str))
                == Some(name)
        });
        (!live).then_some(Value::Null)
    })
    .await;
    assert_eq!(gone, Value::Null);
    client.close().await;
}

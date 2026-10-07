// The engine half of the port, on a fake engine. Each test names the rule it
// pins.

use std::sync::atomic::AtomicUsize;

use comp_model::observation::CornerConfig;
use comp_model::request::{KeySpec, PressAction, SelectionIdentity, SequenceStep};
use comp_model::snapshot::{
    BindingsSnapshot, DecorationSnapshot, FocusSnapshot, FocusWindowSnapshot, FullTreeCache,
    InfoSnapshot, InputSnapshot, WorkspacesSnapshot, XwaylandSnapshot,
};
use surfaces::SeatKind;

use super::*;
use crate::port::test_wiring;

#[derive(Default)]
struct FakeEngine {
    calls: Vec<String>,
    scopes: Vec<ReadScopes>,
    longs: Vec<LongReply>,
}

fn snapshot(context: &PortContext) -> CompSnapshot {
    CompSnapshot {
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
            persist_path: Arc::from("/tmp/xwayland-enabled"),
            display: None,
            state: "off",
            failures: 0,
        },
        dmabuf: Default::default(),
        port: context.port_snapshot(0),
        full_tree: FullTreeCache::default(),
    }
}

struct Engine {
    inner: FakeEngine,
    context: Arc<PortContext>,
    /// Combine adjacent agent `Text` ops (stands in for motion coalescing).
    coalesce: bool,
}

impl CompEngine for Engine {
    fn snapshot(&mut self, scopes: &ReadScopes) -> Option<CompSnapshot> {
        self.inner.calls.push("snapshot".into());
        self.inner.scopes.push(scopes.clone());
        Some(snapshot(&self.context))
    }
    fn set(&mut self, path: &str, _value: &Value, generation: Option<u64>) -> ControlReply {
        self.inner.calls.push(format!("set {path} {generation:?}"));
        ControlReply::Body(json!({"path": path}))
    }
    fn window(&mut self, op: &WindowOp) -> ControlReply {
        self.inner.calls.push(format!("window {op:?}"));
        ControlReply::Body(json!({"window": true}))
    }
    fn input(&mut self, op: &InputOp) -> ControlReply {
        self.inner.calls.push(format!("input {op:?}"));
        ControlReply::Body(json!({"injected": 1}))
    }
    fn panel(&mut self, request: &PanelRequest) -> ControlReply {
        self.inner.calls.push(format!("panel {}", request.sender));
        ControlReply::Body(json!({"accepted": true}))
    }
    fn region_cancel(&mut self, selection: &SelectionIdentity) -> ControlReply {
        self.inner.calls.push(format!(
            "region_cancel {} {}",
            selection.owner, selection.generation
        ));
        ControlReply::Body(json!({"cancelled": true}))
    }
    fn start_long(&mut self, op: LongOp, reply: LongReply, _admitted: Instant) {
        self.inner
            .calls
            .push(format!("long {}", matches!(op, LongOp::Sequence(_))));
        self.inner.longs.push(reply);
    }
    fn services_live(&mut self, live: &BTreeSet<String>) {
        self.inner.calls.push(format!("live {}", live.len()));
    }
    fn watch_props(&mut self, active: bool) -> bool {
        self.inner.calls.push(format!("watch {active}"));
        true
    }
    fn renew_pointer_lease(&mut self) {
        self.inner.calls.push("lease".into());
    }
    fn truth(&mut self) -> Value {
        self.inner.calls.push("truth".into());
        json!({"revision": self.inner.calls.len()})
    }
    fn coalesce_input(&self, previous: &InputOp, next: &InputOp) -> Option<InputOp> {
        let text = |op: &InputOp| match op {
            InputOp::OnSeat {
                seat: SeatKind::Agent,
                op,
            } => match op.as_ref() {
                InputOp::Text(text) => Some(text.clone()),
                _ => None,
            },
            _ => None,
        };
        let (previous, next) = (text(previous)?, text(next)?);
        self.coalesce
            .then(|| agent_text(&format!("{previous}{next}")))
    }
}

fn agent_text(text: &str) -> InputOp {
    InputOp::OnSeat {
        seat: SeatKind::Agent,
        op: Box::new(InputOp::Text(text.into())),
    }
}

/// Adjacent agent inputs the engine combines are delivered once; every reply describes that delivery, marked
/// `coalesced` with the run length. A different op ends the run.
#[tokio::test]
async fn adjacent_agent_inputs_the_engine_combines_are_delivered_once() {
    let (mut wiring, starter) = test_wiring("comp-nested");
    let ingress = starter.ingress().clone();
    let first = ingress.request_input(agent_text("a")).unwrap();
    let second = ingress.request_input(agent_text("b")).unwrap();
    let third = ingress.request_input(agent_text("c")).unwrap();
    let key = ingress.request_input(agent_key()).unwrap();
    let mut engine = engine(&wiring.context);
    engine.coalesce = true;
    wiring.service.service(&mut engine);
    assert_eq!(
        engine.inner.calls,
        [
            format!("input {:?}", agent_text("abc")),
            format!("input {:?}", agent_key()),
        ]
    );
    for admission in [first, second, third] {
        assert_eq!(
            admission.receive().await.unwrap().wire_json(),
            json!({"injected": 1, "coalesced": 3})
        );
    }
    assert_eq!(
        key.receive().await.unwrap().wire_json(),
        json!({"injected": 1})
    );
}

fn engine(context: &Arc<PortContext>) -> Engine {
    Engine {
        inner: FakeEngine::default(),
        context: Arc::clone(context),
        coalesce: false,
    }
}

fn agent_key() -> InputOp {
    InputOp::OnSeat {
        seat: SeatKind::Agent,
        op: Box::new(InputOp::Key {
            key: KeySpec::Name("a".into()),
            action: PressAction::Both,
            modifiers: Vec::new(),
        }),
    }
}

/// A region cancel is a short control: the engine answers it in its own
/// pass, in arrival order, without any long-verb permit.
#[tokio::test]
async fn region_cancel_reaches_the_engine_as_a_short_control() {
    let (mut wiring, starter) = test_wiring("comp-nested");
    let ingress = starter.ingress().clone();
    let admission = ingress
        .request_region_cancel(SelectionIdentity {
            instance: "ab12".into(),
            owner: "a3f9c2d1-4e7b-4a1c-9d8e-5f6b7c8d9e0f".into(),
            generation: 3,
        })
        .unwrap();
    let mut engine = engine(&wiring.context);
    assert!(!wiring.service.service(&mut engine), "nothing left over");
    assert_eq!(
        engine.inner.calls,
        ["region_cancel a3f9c2d1-4e7b-4a1c-9d8e-5f6b7c8d9e0f 3".to_string()]
    );
    assert_eq!(
        admission.receive().await.unwrap().wire_json(),
        json!({"cancelled": true})
    );
    assert_eq!(ingress.depth(), 0, "every slot came back");
}

/// Mutations run in arrival order and every admission is answered exactly
/// once, through its own reply.
#[tokio::test]
async fn controls_run_in_arrival_order_and_each_admission_gets_its_reply() {
    let (mut wiring, starter) = test_wiring("comp-nested");
    let ingress = starter.ingress().clone();
    let set = ingress
        .request_set_fenced("windows.s7.minimized".into(), json!(true), Some(3))
        .unwrap();
    let window = ingress
        .request_window(WindowOp::Restore { target: None })
        .unwrap();
    let input = ingress.request_input(agent_key()).unwrap();
    let mut engine = engine(&wiring.context);
    assert!(!wiring.service.service(&mut engine), "nothing left over");
    assert_eq!(
        engine.inner.calls,
        [
            "set windows.s7.minimized Some(3)".to_string(),
            "window Restore { target: None }".to_string(),
            format!("input {:?}", agent_key()),
        ]
    );
    assert_eq!(
        set.receive().await.unwrap().wire_json()["path"],
        "windows.s7.minimized"
    );
    assert_eq!(window.receive().await.unwrap().wire_json()["window"], true);
    assert_eq!(input.receive().await.unwrap().wire_json()["injected"], 1);
    assert_eq!(ingress.depth(), 0, "every slot came back");
}

/// A read admitted after a still-queued control waits for it, and the agent batch bound (8) leaves a suffix queued with
/// the waker fired again.
#[tokio::test]
async fn reads_wait_behind_queued_controls_and_agent_batches_are_bounded() {
    let (mut wiring, starter) = test_wiring("comp-nested");
    let ingress = starter.ingress().clone();
    let inputs = (0..10)
        .map(|_| ingress.request_input(agent_key()).unwrap())
        .collect::<Vec<_>>();
    let read = ingress
        .request_snapshot_scoped(Some("info".into()))
        .unwrap();
    let mut engine = engine(&wiring.context);
    assert!(wiring.service.service(&mut engine), "a suffix stays queued");
    assert_eq!(engine.inner.calls.len(), AGENT_CONTROL_BATCH);
    assert!(
        !engine.inner.calls.iter().any(|call| call == "snapshot"),
        "the read waits"
    );
    assert!(!wiring.service.service(&mut engine));
    assert_eq!(engine.inner.calls.len(), 11);
    assert_eq!(
        engine.inner.calls.last().map(String::as_str),
        Some("snapshot")
    );
    assert_eq!(
        engine.inner.scopes,
        [ReadScopes::Paths(vec!["info".into()])]
    );
    for input in inputs {
        assert!(input.receive().await.is_ok());
    }
    assert_eq!(
        read.receive().await.unwrap().info.service.as_ref(),
        "comp-nested"
    );
}

/// Queued reads share one snapshot scoped to everything they asked.
#[tokio::test]
async fn ready_reads_share_one_scoped_snapshot() {
    let (mut wiring, starter) = test_wiring("comp-nested");
    let ingress = starter.ingress().clone();
    let first = ingress
        .request_snapshot_scoped(Some("windows.s3".into()))
        .unwrap();
    let second = ingress
        .request_snapshot_scoped(Some("info".into()))
        .unwrap();
    let mut engine = engine(&wiring.context);
    wiring.service.service(&mut engine);
    assert_eq!(engine.inner.calls, ["snapshot"]);
    assert_eq!(
        engine.inner.scopes,
        [ReadScopes::Paths(vec!["windows.s3".into(), "info".into()])]
    );
    let (first, second) = (
        first.receive().await.unwrap(),
        second.receive().await.unwrap(),
    );
    assert!(Arc::ptr_eq(&first, &second));
    let whole = ingress.request_snapshot().unwrap();
    wiring.service.service(&mut engine);
    assert_eq!(engine.inner.scopes.last(), Some(&ReadScopes::All));
    assert!(whole.receive().await.is_ok());
}

/// Human input clears every agent admission still queued, and only those.
#[tokio::test]
async fn human_input_refuses_queued_agent_admissions() {
    let (mut wiring, starter) = test_wiring("comp-nested");
    let ingress = starter.ingress().clone();
    let agent = ingress.request_input(agent_key()).unwrap();
    let human = ingress.request_input(InputOp::Text("x".into())).unwrap();
    let sequence = ingress
        .request_long(LongOp::Sequence(vec![SequenceStep {
            verb: "comp.input.key",
            op: agent_key(),
            delay: Duration::ZERO,
        }]))
        .unwrap();
    wiring.context.clear_agent();
    let mut engine = engine(&wiring.context);
    wiring.service.service(&mut engine);
    assert_eq!(engine.inner.calls, [r#"input Text("x")"#]);
    let refused = agent.receive().await.unwrap().wire_json();
    assert_eq!(
        refused,
        json!({"error": "input_cleared", "error_code": "input_cleared", "seat": "agent", "released": true})
    );
    assert_eq!(
        sequence.receive().await.unwrap().wire_json()["error"],
        "input_cleared"
    );
    assert!(human.receive().await.is_ok());
    assert_eq!(ingress.depth(), 0);
}

/// A long verb's slot is released when the engine takes it; the reply comes
/// through the `LongReply` whenever the verb resolves.
#[tokio::test]
async fn long_verbs_release_their_slot_and_answer_later() {
    let (mut wiring, starter) = test_wiring("comp-nested");
    let ingress = starter.ingress().clone();
    let long = ingress
        .request_long(LongOp::Sequence(vec![SequenceStep {
            verb: "comp.input.release_all",
            op: InputOp::ReleaseAll,
            delay: Duration::from_millis(10),
        }]))
        .unwrap();
    assert_eq!(ingress.depth(), 1);
    let mut engine = engine(&wiring.context);
    wiring.service.service(&mut engine);
    assert_eq!(engine.inner.calls, ["long true"]);
    assert_eq!(ingress.depth(), 0, "the slot is free while the verb waits");
    let reply = engine.inner.longs.pop().unwrap();
    assert!(!reply.is_closed());
    reply.send(ControlReply::Body(json!({"steps": []})));
    assert_eq!(
        long.receive().await.unwrap().wire_json()["steps"],
        json!([])
    );
}

/// `props.watch` seeds the baseline once and answers with the topic and the
/// sequence watermark; the broker's `topic.idle` drops it, and a
/// pointer watch renews the lease.
#[tokio::test]
async fn watches_seed_the_baseline_and_idle_drops_it() {
    let (mut wiring, starter) = test_wiring("comp-nested");
    let ingress = starter.ingress().clone();
    wiring
        .context
        .event_seq
        .store(41, std::sync::atomic::Ordering::Release);
    let watch = ingress.request_watch().unwrap();
    let pointer = ingress.request_pointer_watch().unwrap();
    let mut engine = engine(&wiring.context);
    wiring.service.service(&mut engine);
    assert_eq!(engine.inner.calls, ["lease", "watch true"]);
    assert!(wiring.service.watch_active());
    assert_eq!(
        watch.receive().await.unwrap().wire_json(),
        json!({"topic": "comp-nested.props.changed", "event_seq": 41, "lost_count": 0})
    );
    assert_eq!(
        pointer.receive().await.unwrap().wire_json(),
        json!({"version": 1, "topic": "comp-nested.pointer.changed", "lease_ms": 3000})
    );
    ingress.set_watch_state(false);
    wiring.service.service(&mut engine);
    assert!(!wiring.service.watch_active());
    assert_eq!(
        engine.inner.calls.last().map(String::as_str),
        Some("watch false")
    );
    // Unchanged state seeds nothing.
    ingress.set_watch_state(false);
    let before = engine.inner.calls.len();
    wiring.service.service(&mut engine);
    assert_eq!(engine.inner.calls.len(), before);
}

/// A full lane coalesces lifecycle notices into the pending orders, latest
/// wins, and the next pass applies it.
#[tokio::test]
async fn a_coalesced_lifecycle_notice_is_applied_on_the_next_pass() {
    let (mut wiring, starter) = test_wiring("comp-nested");
    let ingress = starter.ingress().clone();
    let _held = (0..PORT_QUEUE_CAPACITY)
        .map(|_| ingress.request_snapshot().unwrap())
        .collect::<Vec<_>>();
    ingress.set_watch_state(true);
    assert_ne!(
        wiring
            .context
            .pending_active_order
            .load(std::sync::atomic::Ordering::Acquire),
        0
    );
    let mut engine = engine(&wiring.context);
    wiring.service.service(&mut engine);
    assert!(engine.inner.calls.contains(&"watch true".to_string()));
    assert!(wiring.service.watch_active());
}

/// The engine is woken by the waker on every admission, never by a poll.
#[test]
fn every_admission_wakes_the_engine() {
    let wakes = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&wakes);
    let waker: crate::channel::Waker = Arc::new(move || {
        counter.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    });
    let identity = crate::port::PortIdentity {
        service: "comp-nested".into(),
        version: "0.0.0".into(),
        backend: "nested",
        engine: "test",
        noded_url: "ws://127.0.0.1:1/ws".into(),
    };
    let (wiring, starter) = crate::port::prepare(identity, waker).unwrap();
    let ingress = starter.ingress().clone();
    let _ = ingress.request_watch().unwrap();
    let _ = ingress.request_snapshot().unwrap();
    ingress.services_live(BTreeSet::from(["noded".to_string()]));
    assert_eq!(wakes.load(std::sync::atomic::Ordering::Acquire), 3);
    assert_eq!(
        wiring.context.instance.len(),
        32,
        "a random 128-bit instance id"
    );
    assert!(
        crate::port::prepare(
            crate::port::PortIdentity {
                service: "Comp".into(),
                version: "0".into(),
                backend: "kms",
                engine: "test",
                noded_url: String::new(),
            },
            crate::channel::no_waker(),
        )
        .is_err(),
        "the service name follows the ABP grammar"
    );
}

/// The port row reads the shared counters.
#[test]
fn port_snapshot_reads_the_shared_counters() {
    let context = PortContext::new("comp", "1", "kms", "gles", "abc");
    context
        .queue_depth
        .store(2, std::sync::atomic::Ordering::Release);
    context
        .broker
        .store(BROKER_CONNECTED, std::sync::atomic::Ordering::Release);
    let port = context.port_snapshot(1);
    assert_eq!(
        (
            port.level,
            port.queue_depth,
            port.slug_collisions,
            port.broker
        ),
        ("L2", 2, 1, "connected")
    );
}

/// `compd.truth` is a control: answered from `CompEngine::truth` after the
/// mutations admitted before it in the same pass (the revision it reports
/// counts them).
#[tokio::test]
async fn truth_is_answered_after_earlier_mutations() {
    let (mut wiring, starter) = test_wiring("comp-nested");
    let ingress = starter.ingress().clone();
    let set = ingress
        .request_set("input.corners.enabled".into(), json!(true))
        .unwrap();
    let truth = ingress.request_truth().unwrap();
    let mut engine = engine(&wiring.context);
    wiring.service.service(&mut engine);
    assert_eq!(
        engine.inner.calls,
        ["set input.corners.enabled None", "truth"]
    );
    assert!(set.receive().await.is_ok());
    assert_eq!(
        truth.receive().await.unwrap().wire_json(),
        json!({"revision": 2})
    );
    assert_eq!(ingress.depth(), 0);
}

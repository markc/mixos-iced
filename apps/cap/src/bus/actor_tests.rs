// SPDX-License-Identifier: MIT OR Apache-2.0
//! Actual production noded and the actual worker, with a deliberately paused GUI.
use super::*;
use application::iced::futures::StreamExt;

async fn observed(
    probe: &mut tokio::sync::watch::Receiver<ActorProbe>,
    predicate: impl Fn(&ActorProbe) -> bool,
) -> ActorProbe {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let state = probe.borrow_and_update().clone();
            assert_eq!(
                state.invariant_faults, 0,
                "actual actor invariant: {state:?}"
            );
            assert!(
                state.active <= ACCEPTED_CAP
                    && state.reply_tasks <= 1
                    && state.operations <= OPERATION_CAP + CLEANUP_CAP
            );
            #[cfg(feature = "acceptance")]
            assert!(state.fixture_waits <= 2 && state.fixture_controls <= 4 && state.fixture_active <= 6);
            if predicate(&state) {
                return state;
            }
            probe
                .changed()
                .await
                .expect("actual worker retired before checkpoint");
        }
    })
    .await
    .expect("actual worker checkpoint deadline")
}

struct Stop(BusHandle);
impl Drop for Stop {
    fn drop(&mut self) {
        self.0.quit();
        self.0.wait_done(Duration::from_secs(5));
    }
}

#[cfg(feature = "acceptance")]
#[test]
fn fixture_waits_leave_release_headroom_and_retire_on_native_loss_and_shutdown() {
    use application::acceptance::{Fixture, Launch, frames::Target};
    let mut broker = term_test_broker::Broker::start_stable();
    let (fixture, _inspector_task) = Fixture::new::<crate::app::Message>(
        Launch { run: "cap-owned".into(), instance: 73 }, crate::acceptance::POINTS,
        vec![application::inspect::Target::new("root", application::iced::widget::Id::from(crate::acceptance::ROOT_ID))],
        application::inspect::Limits::new(),
    ).unwrap();
    let (probe, mut observation) = tokio::sync::watch::channel(ActorProbe::default());
    let (send, receive) = mpsc::channel(OUTBOX_CAP);
    let (handle, mut ui, _bootstrap, mut events) = start_configured_channels(
        "actor-cap-fixture", &broker.url, None, send, receive, Some(fixture), Some(probe)).unwrap();
    let _stop = Stop(handle.clone());
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let first = observed(&mut observation, |state| state.connected).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                ui.reconcile(handle.settings_generation());
                ui.drain_with(|| handle.settings_generation(), |_| {});
                if ui.preparation_evidence().current { break; }
                assert!(ui.preparation_evidence().fault.is_none(), "{:?}", ui.preparation_evidence().fault);
                events.next().await.expect("actual preparation wake");
            }
        }).await.expect("real native bootstrap activation");
        let caller = Arc::new(NodedClient::connect_anonymous(&broker.url).await.unwrap());
        // This target deliberately has no native presented observation. It
        // holds a waiter; the actor test never manufactures frame success.
        let window = application::iced::window::Id::unique();
        let stamp = ui.session().frame_stamp().unwrap();
        handle.fixture_frames.as_ref().unwrap().publish(Target { window, stamp: Some(stamp) }).unwrap();
        let identity = json!({"run":"cap-owned","instance":73,"generation":first.generation});
        let description = caller.call("actor-cap-fixture", "app.acceptance.describe", identity).await.unwrap();
        assert_eq!(description["pid"], std::process::id());
        assert_eq!(description["frames"]["enabled"], true);
        let arm = caller.call("actor-cap-fixture", "app.acceptance.barrier.arm",
            json!({"run":"cap-owned","instance":73,"generation":first.generation,"point":"cap.prepare","token":"held"})).await.unwrap();
        assert_eq!(arm["ok"], true);
        let reference = json!({"run":"cap-owned","instance":73,"generation":first.generation,"token":"held","sequence":arm["sequence"]});
        let barrier_wait = {
            let caller = caller.clone(); let reference = reference.clone();
            tokio::spawn(async move { caller.call("actor-cap-fixture", "app.acceptance.barrier.wait", reference).await })
        };
        let frame_request = json!({"run":"cap-owned","instance":73,"generation":first.generation,
            "window":window.raw(),"activation_epoch":stamp.activation_epoch,"local_revision":stamp.local_revision,"timeout_ms":10000});
        let frame_wait = {
            let caller = caller.clone(); let request = frame_request.clone();
            tokio::spawn(async move { caller.call("actor-cap-fixture", "app.acceptance.frame.wait", request).await })
        };
        observed(&mut observation, |state| state.fixture_waits == 2).await;
        assert!(!barrier_wait.is_finished() && !frame_wait.is_finished());
        assert_eq!(observation.borrow().pending, 0, "fixture does not allocate a product ticket");
        let third = caller.call_with_headers_raw("actor-cap-fixture", "app.acceptance.barrier.wait", &BTreeMap::new(), &reference.to_string()).await.unwrap();
        assert_eq!(third.0, 10);
        let held = caller.call("actor-cap-fixture", "app.acceptance.barrier.state", reference.clone()).await.unwrap();
        assert_eq!(held["state"], "armed");
        ui.retry_preparation(handle.settings_generation()).unwrap();
        let reached = tokio::time::timeout(Duration::from_secs(3), barrier_wait).await.unwrap().unwrap().unwrap();
        assert_eq!(reached["state"], "reached", "actual Cap prepare hook blocks before publication");
        let released = tokio::time::timeout(Duration::from_secs(2),
            caller.call("actor-cap-fixture", "app.acceptance.barrier.release", reference.clone())).await.unwrap().unwrap();
        assert_eq!(released["state"], "released");
        observed(&mut observation, |state| state.fixture_waits == 1 && state.fixture_controls == 0).await;
        assert!(!frame_wait.is_finished());
        broker.bounce();
        let next = observed(&mut observation, |state| state.connected && state.generation > first.generation && state.fixture_waits == 0).await;
        let retired = tokio::time::timeout(Duration::from_secs(5), frame_wait).await.unwrap().unwrap();
        assert!(retired.is_err() || retired.as_ref().is_ok_and(|value| value["ok"] == false), "lost frame wait cannot claim success: {retired:?}");
        assert!(!handle.frames.snapshot().closed, "surviving window keeps its observer");
        assert_eq!(handle.frames.snapshot().live_generation, Some(next.generation));
        assert!(handle.frames.snapshot().last_presented.is_none());
        caller.close().await;
        let caller = Arc::new(NodedClient::connect_anonymous(&broker.url).await.unwrap());
        let stale = caller.call("actor-cap-fixture", "app.acceptance.barrier.release", reference).await.unwrap();
        assert_eq!(stale["ok"], false, "old native generation cannot release replacement work");
        let arm = caller.call("actor-cap-fixture", "app.acceptance.barrier.arm",
            json!({"run":"cap-owned","instance":73,"generation":next.generation,"point":"cap.prepare","token":"shutdown"})).await.unwrap();
        assert_eq!(arm["ok"], true);
        let reference = json!({"run":"cap-owned","instance":73,"generation":next.generation,"token":"shutdown","sequence":arm["sequence"]});
        let barrier_wait = {
            let caller = caller.clone();
            tokio::spawn(async move { caller.call("actor-cap-fixture", "app.acceptance.barrier.wait", reference).await })
        };
        let frame_wait = {
            let caller = caller.clone(); let mut request = frame_request;
            request["generation"] = json!(next.generation);
            tokio::spawn(async move { caller.call("actor-cap-fixture", "app.acceptance.frame.wait", request).await })
        };
        observed(&mut observation, |state| state.fixture_waits == 2).await;
        handle.quit();
        for waiting in [barrier_wait, frame_wait] {
            let result = tokio::time::timeout(Duration::from_secs(3), waiting).await.unwrap().unwrap();
            assert!(result.is_err() || result.as_ref().is_ok_and(|value| value["ok"] == false), "shutdown wait cannot claim success: {result:?}");
        }
        handle.wait_done(Duration::from_secs(5));
        assert!(*handle.done.0.lock().unwrap(), "real actor shutdown receipt");
        assert!(handle.frames.snapshot().closed);
        caller.close().await;
    });
}

fn reconnect(gui_capacity: usize) {
    let mut broker = term_test_broker::Broker::start_stable();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (probe, mut observation) = tokio::sync::watch::channel(ActorProbe::default());
        let (handle, _ui, _bootstrap, mut events) =
            start_inner("actor-cap", &broker.url, None, gui_capacity, Some(probe)).unwrap();
        let _stop = Stop(handle.clone());
        let mut state = observed(&mut observation, |state| state.connected).await;
        assert_eq!(
            handle.service_name(),
            "actor-cap",
            "actual custom registered supervisor identity"
        );
        let caller = NodedClient::connect_anonymous(&broker.url).await.unwrap();
        let oversized = format!(
            "{{{}}}",
            " ".repeat(application::describe::MAX_REQUEST_BYTES)
        );
        for body in ["{", "[]", "null", r#"{"extra":true}"#, oversized.as_str()] {
            let (rc, body, _) = tokio::time::timeout(
                Duration::from_secs(5),
                caller.call_with_headers_raw("actor-cap", "app.describe", &BTreeMap::new(), body),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(rc, 10);
            let refusal: Value = serde_json::from_str(&body).unwrap();
            assert!(refusal["error"].is_string());
            assert!(refusal["describe_code"].is_string());
        }
        caller.close().await;
        assert_eq!(
            observation.borrow().pending,
            0,
            "invalid descriptions bypass paused GUI"
        );
        let mut decisive = None;
        for batch in 0..5 {
            let caller = Arc::new(NodedClient::connect_anonymous(&broker.url).await.unwrap());
            let mut calls = tokio::task::JoinSet::new();
            for sequence in 0..32 {
                let caller = caller.clone();
                calls.spawn(async move {
                    tokio::time::timeout(
                        Duration::from_secs(20),
                        caller.call_with_headers_raw(
                            "actor-cap",
                            "cap.ping",
                            &BTreeMap::new(),
                            &json!({"batch":batch,"sequence":sequence}).to_string(),
                        ),
                    )
                    .await
                    .unwrap()
                });
            }
            let generation = state.generation;
            state = observed(&mut observation, |state| {
                state.generation == generation && state.pending == 32
            })
            .await;
            assert_eq!(state.active, 32);
            let stalled = state.reliable > 2;
            broker.bounce();
            state = observed(&mut observation, |state| {
                state.connected
                    && state.generation > generation
                    && state.pending == 0
                    && state.reliable == 0
            })
            .await;
            while let Some(result) = calls.join_next().await {
                assert!(
                    result.unwrap().is_err(),
                    "a lost command was replayed or acknowledged"
                );
            }
            caller.close().await;
            if stalled {
                let caller = Arc::new(NodedClient::connect_anonymous(&broker.url).await.unwrap());
                let mut calls = tokio::task::JoinSet::new();
                for sequence in 0..32 {
                    let caller = caller.clone();
                    calls.spawn(async move {
                        let body = json!({"batch":"new","sequence":sequence}).to_string();
                        let response = tokio::time::timeout(
                            Duration::from_secs(20),
                            caller.call_with_headers_raw(
                                "actor-cap",
                                "cap.ping",
                                &BTreeMap::new(),
                                &body,
                            ),
                        )
                        .await
                        .unwrap()
                        .unwrap();
                        assert_eq!(response.0, 0);
                        assert_eq!(response.1, body);
                    });
                }
                let generation = state.generation;
                observed(&mut observation, |state| {
                    state.generation == generation && state.pending == 32 && state.reliable > 2
                })
                .await;
                decisive = Some((caller, calls));
                break;
            }
        }
        let (caller, mut calls) = decisive.expect("actual GUI channel must reach backpressure");
        let mut sequences = std::collections::BTreeSet::new();
        tokio::time::timeout(Duration::from_secs(15), async {
            while sequences.len() < 32 {
                if let Delivery::Command(command) =
                    events.next().await.expect("worker closed during GUI drain")
                {
                    if !handle.is_current(&command.id) {
                        continue;
                    }
                    let body: Value = serde_json::from_str(&command.body).unwrap();
                    assert_eq!(body["batch"], "new");
                    assert!(sequences.insert(body["sequence"].as_u64().unwrap()));
                    handle.respond(command.id.clone(), 0, command.body);
                    handle.respond(command.id, 10, "{\"duplicate\":true}".into());
                }
            }
        })
        .await
        .unwrap();
        while let Some(result) = calls.join_next().await {
            result.unwrap();
        }
        observed(&mut observation, |state| {
            state.pending == 0 && state.active == 0 && state.replies == 0 && state.reply_tasks == 0
        })
        .await;
        let calling = caller.clone();
        let quit = tokio::spawn(async move {
            calling
                .call_with_headers_raw("actor-cap", "cap.quit", &BTreeMap::new(), "{}")
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Delivery::Command(command) =
                    events.next().await.expect("worker closed before quit")
                {
                    assert_eq!(command.verb, "cap.quit");
                    assert!(handle.is_current(&command.id));
                    handle.respond(command.id, 0, "{\"quitting\":true}".into());
                    handle.quit();
                    handle.quit();
                    break;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), quit)
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .0,
            0
        );
        handle.wait_done(Duration::from_secs(5));
        assert!(*handle.done.0.lock().unwrap(), "actual worker completion");
        caller.close().await;
    });
}

#[test]
fn actual_broker_reconnect_with_tight_stalled_gui() {
    reconnect(1);
}
#[test]
fn actual_broker_reconnect_with_production_stalled_gui() {
    reconnect(64);
}

#[test]
fn actual_cleanup_calls_and_quit_progress_with_gui_and_ordinary_work_full() {
    let broker = term_test_broker::Broker::start_stable();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (probe, mut observation) = tokio::sync::watch::channel(ActorProbe::default());
        let (handle, _ui, _bootstrap, _events) =
            start_inner("actor-cap-cleanup", &broker.url, None, 1, Some(probe)).unwrap();
        let _stop = Stop(handle.clone());
        observed(&mut observation, |state| state.connected).await;
        let caller = Arc::new(NodedClient::connect_anonymous(&broker.url).await.unwrap());
        let mut calls = tokio::task::JoinSet::new();
        for sequence in 0..32 {
            let caller = caller.clone();
            calls.spawn(async move {
                caller
                    .call_with_headers_raw(
                        "actor-cap-cleanup",
                        "cap.ping",
                        &BTreeMap::new(),
                        &json!({"sequence":sequence}).to_string(),
                    )
                    .await
            });
        }
        observed(&mut observation, |state| {
            state.pending == 32 && state.reliable > 2
        })
        .await;
        let mut delays = Vec::with_capacity(OPERATION_CAP);
        for _ in 0..OPERATION_CAP {
            let mut delay = Box::pin(handle.delay(Duration::from_secs(60)));
            std::future::poll_fn(|cx| match std::future::Future::poll(delay.as_mut(), cx) {
                std::task::Poll::Pending => std::task::Poll::Ready(()),
                std::task::Poll::Ready(result) => {
                    panic!("long accepted delay completed: {result:?}")
                }
            })
            .await;
            delays.push(delay);
        }
        observed(&mut observation, |state| state.operations == OPERATION_CAP).await;
        assert_eq!(handle.outgoing.counts().active, OPERATION_CAP);
        assert_eq!(
            handle
                .call(
                    "comp-fixture",
                    "comp.windows.list",
                    json!({}),
                    Duration::from_secs(2)
                )
                .await
                .unwrap_err(),
            "Bus worker busy"
        );
        let comp = Arc::new(
            SupervisedClient::connect_options("comp-fixture", &broker.url)
                .bounded_incoming(4)
                .connect()
                .await
                .unwrap(),
        );
        let mut incoming = comp.incoming_bounded().unwrap();
        let server = comp.clone();
        let responding = tokio::spawn(async move {
            for verb in ["comp.region.cancel", "comp.window.restore"] {
                let Some(BoundedIncomingEvent::Command(command)) = incoming.recv().await else {
                    panic!("actual cleanup request missing")
                };
                assert_eq!(command.command, verb);
                server
                    .respond(&command, 0, "{\"retired\":true}")
                    .await
                    .unwrap();
            }
        });
        for verb in ["comp.region.cancel", "comp.window.restore"] {
            assert_eq!(
                tokio::time::timeout(
                    Duration::from_secs(5),
                    handle.call(
                        "comp-fixture",
                        verb,
                        json!({"token":"fixture"}),
                        Duration::from_secs(2)
                    )
                )
                .await
                .unwrap()
                .unwrap()["retired"],
                true
            );
        }
        responding.await.unwrap();
        handle.quit();
        handle.wait_done(Duration::from_secs(5));
        assert!(*handle.done.0.lock().unwrap());
        drop(delays);
        assert_eq!(
            handle.outgoing.counts().active,
            0,
            "owning runtime destroyed cancelled operations"
        );
        assert_eq!(handle.cleanup.counts().active, 0);
        caller.close().await;
        while let Some(result) = calls.join_next().await {
            assert!(result.unwrap().is_err());
        }
        comp.close().await;
    });
}

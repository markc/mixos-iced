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

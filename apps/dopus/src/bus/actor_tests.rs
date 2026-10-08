// SPDX-License-Identifier: MIT OR Apache-2.0
//! Actual production noded and the actual worker, with a deliberately paused GUI.
use super::*;
use application::iced::futures::StreamExt;
use serde_json::{Value, json};

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
                state.active <= PENDING_BOUND
                    && state.reply_tasks <= JOB_BOUND
                    && state.themes <= THEME_QUEUE_BOUND
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
        let _ = self.0.wait_done(Duration::from_secs(5));
    }
}

#[tokio::test]
async fn raw_describe_refusals_do_not_admit_work_into_a_paused_gui() {
    let broker = term_test_broker::Broker::start_stable();
    let (probe, mut observation) = tokio::sync::watch::channel(ActorProbe::default());
    let (handle, _paused_gui) = actor::start("dopus-description-overridden", &broker.url, false, 1, Some(probe)).unwrap();
    let _stop = Stop(handle.clone());
    observed(&mut observation, |state| state.connected).await;
    assert_eq!(handle.service_name(), "dopus-description-overridden");
    let caller = NodedClient::connect_anonymous(&broker.url).await.unwrap();
    for body in ["{".into(), "null".into(), "[]".into(), "{\"extra\":true}".into(),
        format!("{{{}}}", " ".repeat(application::describe::MAX_REQUEST_BYTES))] {
        let (rc, body, _) = tokio::time::timeout(Duration::from_secs(5),
            caller.call_with_headers_raw("dopus-description-overridden", "app.describe", &BTreeMap::new(), &body)).await.unwrap().unwrap();
        let value: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(rc, 10);
        assert_eq!(value["error_code"], "INVALID_ARGUMENT");
        assert!(value["describe_code"].is_string());
    }
    assert_eq!(observation.borrow().pending, 0);
    handle.quit();
    // This join blocks only the test, while the actor owns its own runtime.
    handle.wait_done(Duration::from_secs(4)).unwrap();
    caller.close().await;
}

fn reconnect(gui_capacity: usize, settings: bool) {
    let mut broker = term_test_broker::Broker::start_stable();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (probe, mut observation) = tokio::sync::watch::channel(ActorProbe::default());
        let (handle, mut events) = actor::start(
            "actor-dopus",
            &broker.url,
            settings,
            gui_capacity,
            Some(probe),
        )
        .unwrap();
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
                            "actor-dopus",
                            "dopus.ping",
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
                                "actor-dopus",
                                "dopus.ping",
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
                .call_with_headers_raw("actor-dopus", "dopus.quit", &BTreeMap::new(), "{}")
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Delivery::Command(command) =
                    events.next().await.expect("worker closed before quit")
                {
                    assert_eq!(command.verb, "dopus.quit");
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
        handle.wait_done(Duration::from_secs(5)).unwrap();
        assert!(
            handle.done.0.lock().unwrap().finished,
            "actual worker completion"
        );
        caller.close().await;
    });
}

#[test]
fn actual_broker_reconnect_with_tight_stalled_gui() {
    reconnect(1, true);
}
#[test]
fn actual_broker_reconnect_with_production_stalled_gui() {
    reconnect(64, true);
}

#[test]
fn actual_headless_actor_reconnects_with_stalled_frontend() {
    reconnect(1, false);
}

fn theme(id: Option<Request>, operation: &str) -> ThemeRequest {
    ThemeRequest {
        reply_id: id,
        binding: settings::Binding {
            instance: "fixture".into(),
            profile: "default".into(),
        },
        expected_incarnation: "fixture".into(),
        expected_revision: settings::Revision(1),
        operation_id: operation.into(),
        changes: BTreeMap::from([("appearance.mode".into(), json!("dark"))]),
        scheme: "ocean".into(),
        mode: "dark".into(),
    }
}

#[test]
fn actual_theme_origin_is_never_promoted_to_local_and_clones_mutate_once() {
    let broker = term_test_broker::Broker::start_stable();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let authority = Arc::new(
            SupervisedClient::connect_options("settingsd", &broker.url)
                .bounded_incoming(32)
                .connect()
                .await
                .unwrap(),
        );
        let mut incoming = authority.incoming_bounded().unwrap();
        let server = authority.clone();
        let replies = tokio::spawn(async move {
            for operation in ["local-allowed", "bus-allowed"] {
                for expected in ["settings.validate", "settings.apply"] {
                    loop {
                        let Some(BoundedIncomingEvent::Command(command)) =
                            tokio::time::timeout(Duration::from_secs(5), incoming.recv())
                                .await
                                .unwrap()
                        else {
                            panic!("actual settings request missing")
                        };
                        if !matches!(
                            command.command.as_str(),
                            "settings.validate" | "settings.apply"
                        ) {
                            server
                                .respond(&command, 10, "{\"status\":\"fixture_read_unavailable\"}")
                                .await
                                .unwrap();
                            continue;
                        }
                        assert_eq!(command.command, expected);
                        let body: Value = serde_json::from_str(&command.body).unwrap();
                        assert_eq!(
                            body["operation_id"], operation,
                            "missing Bus origin dispatched a mutation"
                        );
                        let status = if expected == "settings.validate" {
                            "valid"
                        } else {
                            "changed"
                        };
                        server
                            .respond(&command, 0, &json!({"status":status}).to_string())
                            .await
                            .unwrap();
                        break;
                    }
                }
            }
        });
        let (probe, mut observation) = tokio::sync::watch::channel(ActorProbe::default());
        let (handle, mut events) =
            actor::start("dopus-theme-origin", &broker.url, true, 64, Some(probe)).unwrap();
        let _stop = Stop(handle.clone());
        let state = observed(&mut observation, |state| state.connected).await;
        handle
            .theme_apply(theme(
                Some(Request::new(999, state.generation)),
                "must-not-run",
            ))
            .unwrap();
        handle.theme_apply(theme(None, "local-allowed")).unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Delivery::ThemeApplied(result) = events
                    .next()
                    .await
                    .expect("worker closed before local theme completion")
                {
                    result.unwrap();
                    break;
                }
            }
        })
        .await
        .unwrap();
        observed(&mut observation, |state| state.themes == 0).await;
        assert_eq!(handle.themes.counts().active, 0);
        let caller = Arc::new(NodedClient::connect_anonymous(&broker.url).await.unwrap());
        let calling = caller.clone();
        let call = tokio::spawn(async move {
            calling
                .call_with_headers_raw(
                    "dopus-theme-origin",
                    "dopus.theme.set",
                    &BTreeMap::new(),
                    "{}",
                )
                .await
        });
        let command = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Delivery::Command(command) = events
                    .next()
                    .await
                    .expect("worker closed before Bus theme command")
                {
                    break command;
                }
            }
        })
        .await
        .unwrap();
        handle
            .theme_apply(theme(Some(command.id.clone()), "bus-allowed"))
            .unwrap();
        assert!(
            handle
                .theme_apply(theme(Some(command.id.clone()), "must-not-run"))
                .is_err()
        );
        handle.respond(command.id, 10, "{\"duplicate\":true}".into());
        let (rc, body, _) = tokio::time::timeout(Duration::from_secs(5), call)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(rc, 0);
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap()["mode"],
            "dark"
        );
        replies.await.unwrap();
        observed(&mut observation, |state| {
            state.themes == 0 && state.active == 0
        })
        .await;
        assert_eq!(handle.themes.counts().active, 0);
        handle.quit();
        handle.wait_done(Duration::from_secs(5)).unwrap();
        caller.close().await;
        authority.close().await;
    });
}

#[test]
fn actual_old_generation_never_dispatches_a_theme_mutation_after_reconnect() {
    let mut broker = term_test_broker::Broker::start_stable();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (probe, mut observation) = tokio::sync::watch::channel(ActorProbe::default());
        let (handle, _events) =
            actor::start("dopus-theme-fence", &broker.url, true, 1, Some(probe)).unwrap();
        let _stop = Stop(handle.clone());
        let old = observed(&mut observation, |state| state.connected)
            .await
            .generation;
        broker.bounce();
        let current = observed(&mut observation, |state| {
            state.connected && state.generation > old
        })
        .await
        .generation;
        let authority = Arc::new(
            SupervisedClient::connect_options("settingsd", &broker.url)
                .bounded_incoming(16)
                .connect()
                .await
                .unwrap(),
        );
        let mut incoming = authority.incoming_bounded().unwrap();
        let result = theme_apply(
            handle.client.as_ref().unwrap(),
            Some(old),
            Instant::now() + Duration::from_secs(2),
            &theme(None, "must-not-run"),
        )
        .await;
        assert_eq!(result.0, 10);
        assert!(result.2.unwrap_err().contains("no call sent"));
        let server = authority.clone();
        let sentinel = tokio::spawn(async move {
            loop {
                let Some(BoundedIncomingEvent::Command(command)) =
                    tokio::time::timeout(Duration::from_secs(5), incoming.recv())
                        .await
                        .expect("native sentinel deadline")
                else {
                    panic!("native authority stream closed")
                };
                if matches!(
                    command.command.as_str(),
                    "settings.validate" | "settings.apply"
                ) {
                    assert_eq!(command.command, "settings.validate");
                    assert_eq!(
                        serde_json::from_str::<Value>(&command.body).unwrap()["operation_id"],
                        "current-sentinel",
                        "retired mutation reached the current native authority"
                    );
                    server
                        .respond(&command, 0, "{\"status\":\"valid\"}")
                        .await
                        .unwrap();
                    break;
                }
                server
                    .respond(&command, 10, "{\"status\":\"fixture_read_unavailable\"}")
                    .await
                    .unwrap();
            }
        });
        assert_eq!(
            tokio::time::timeout(
                Duration::from_secs(5),
                call_settings(
                    handle.client.as_ref().unwrap(),
                    current,
                    "settings.validate",
                    json!({"operation_id":"current-sentinel"})
                )
            )
            .await
            .unwrap()
            .unwrap()["status"],
            "valid"
        );
        sentinel.await.unwrap();
        handle.quit();
        handle.wait_done(Duration::from_secs(5)).unwrap();
        authority.close().await;
    });
}

// The region, agent-control and agent-seat argument parsers, the engine-free
// request tests, and the classify, error_code and key-order tests at the end.
// The long-admission test reads `LongOp::admission_timeout` directly.

use super::*;
use crate::reply::{validate_service_name, with_error_code};
use crate::snapshot::ReadScopes;
use surfaces::WindowTargetError;

#[test]
fn region_arguments_are_strict_and_leave_reply_margin() {
    for args in [
        json!({"timeout_ms":0}),
        json!({"timeout_ms":55_001}),
        json!({"timeout_ms":2.5}),
        json!({"output":""}),
        json!({"region":{}}),
    ] {
        assert!(parse_region_select(&args).is_err(), "{args}");
    }
    let op = parse_region_select(&json!({"output":"Output-1","timeout_ms":55_000})).unwrap();
    assert_eq!(op.budget(), Duration::from_secs(59));
    assert!(op.budget() < LONG_VERB_MAX);
    let reply_budget =
        Duration::from_secs(55) + REGION_CLEANUP_BUDGET;
    assert_eq!(reply_budget, Duration::from_secs(58));
    assert_eq!(op.budget(), reply_budget + Duration::from_secs(1));
    assert_eq!(
        op.budget().min(LONG_VERB_MAX) + LONG_VERB_SLACK,
        reply_budget + Duration::from_secs(2)
    );
    assert!(
        matches!(parse_region_select(&json!({})).unwrap(),LongOp::RegionSelect {output:None,timeout} if timeout==Duration::from_secs(30))
    );
}

#[test]
fn state_verbs_are_registered_fenced_and_strict() {
    for (verb, state, enabled) in [
        ("comp.window.maximize", WindowState::Maximized, true),
        ("comp.window.unmaximize", WindowState::Maximized, false),
        ("comp.window.fullscreen", WindowState::Fullscreen, true),
        ("comp.window.unfullscreen", WindowState::Fullscreen, false),
    ] {
        assert_eq!(window_verb(verb), Some(verb));
        assert_eq!(parse_window_verb(verb, &json!({"id":7,"generation":3})),
            Ok(WindowVerb::Op(WindowOp::State { id:7, generation:3, state, enabled, output:None })));
        for args in [json!({}), json!({"id":7}), json!({"id":7,"generation":null}),
            json!({"id":7,"generation":3,"typo":true})] {
            assert!(parse_window_verb(verb, &args).is_err(), "{verb}: {args}");
        }
        let output = json!({"id":7,"generation":3,"output":"Output-1"});
        assert_eq!(parse_window_verb(verb, &output).is_ok(), verb == "comp.window.fullscreen");
    }
    assert!(parse_window_verb("comp.window.fullscreen", &json!({"id":7,"generation":3,"output":""})).is_err());
}

#[test]
fn state_waits_parse_and_name_the_committed_condition() {
    for until in [WaitUntil::Maximized, WaitUntil::Unmaximized, WaitUntil::Fullscreen, WaitUntil::Unfullscreen] {
        assert!(matches!(parse_window_verb("comp.window.wait", &json!({
            "match":{"id":7,"generation":3}, "until":until.name(),
        })), Ok(WindowVerb::Long(LongOp::Wait(spec))) if spec.until == until));
    }
}

#[test]
fn targeted_input_is_strict_and_sequences_share_the_parser() {
    for (verb, mut args) in [
        ("comp.input.key", json!({"key":"a"})),
        ("comp.input.key", json!({"text":"hello"})),
        ("comp.input.pointer.button", json!({"button":"left"})),
    ] {
        args["seat"] = json!("human");
        args["window"] = json!({"id":7,"generation":3});
        assert!(matches!(parse_input_op(verb, &args), Ok(InputOp::Targeted {
            id:7, generation:3, raise:true, ..
        })));
        args["raise"] = json!(false);
        assert!(matches!(parse_input_op(verb, &args), Ok(InputOp::Targeted { raise:false, .. })));
        assert!(parse_sequence(&json!({"steps":[{"verb":verb,"args":args}]})).is_ok());
        for window in [json!({"id":7}), json!({"generation":3}), json!({"id":7,"generation":3,"raise":true}), json!(7)] {
            args["window"] = window;
            assert!(parse_input_op(verb, &args).is_err(), "{verb}: {args}");
        }
        args.as_object_mut().unwrap().remove("window");
        assert!(parse_input_op(verb, &args).is_err(), "raise needs window");
    }
}

#[test]
fn seat_defaults_raise_policy_and_release_all_are_explicit() {
    let bare = parse_input_op("comp.input.key", &json!({"key":"a","raise":true})).unwrap_err().wire_json();
    let human = parse_input_op("comp.input.key", &json!({"seat":"human","key":"a","raise":true})).unwrap_err().wire_json();
    assert_eq!(bare, human);
    assert_eq!(bare["range"], "requires window");
    let window = json!({"id":1,"generation":2});
    assert!(matches!(parse_input_op("comp.input.key", &json!({"window":window,"key":"a","seat":"human"})).unwrap(), InputOp::Targeted { raise:true, .. }));
    let InputOp::OnSeat { seat, op } = parse_input_op("comp.input.key", &json!({"window":window,"key":"a"})).unwrap() else { panic!("default agent wrapper") };
    assert_eq!(seat, SeatKind::Agent);
    assert!(matches!(*op, InputOp::Targeted { raise:false, .. }));
    assert!(parse_input_op("comp.input.key", &json!({"window":window,"key":"a","raise":true})).is_err());
    let InputOp::OnSeat { seat, op } = parse_input_op("comp.input.key", &json!({"window":window,"key":"a","seat":"agent"})).unwrap() else { panic!("agent wrapper") };
    assert_eq!(seat, SeatKind::Agent);
    assert!(matches!(*op, InputOp::Targeted { raise:false, .. }));
    assert!(parse_input_op("comp.input.key", &json!({"window":window,"key":"a","seat":"agent","raise":true})).is_err());
    for seat in [json!(null), json!(false), json!("other")] {
        assert!(parse_input_op("comp.input.key", &json!({"key":"a","seat":seat})).is_err());
    }
    assert_eq!(parse_input_op("comp.input.release_all", &json!({})).unwrap(), InputOp::ReleaseAll);
    assert_eq!(parse_input_op("comp.input.release_all", &json!({"seat":"human"})).unwrap(), InputOp::OnSeat { seat: SeatKind::Human, op: Box::new(InputOp::ReleaseAll) });
    assert_eq!(parse_input_op("comp.input.release_all", &json!({"seat":"agent"})).unwrap(), InputOp::OnSeat { seat: SeatKind::Agent, op: Box::new(InputOp::ReleaseAll) });
    assert_eq!(parse_input_op("comp.input.release_all", &Value::Null).unwrap(), InputOp::ReleaseAll);
}

#[test]
fn every_delivery_verb_defaults_to_agent_and_bare_sequence_cleanup_stays_both() {
    assert_eq!(DEFAULT_INPUT_SEAT, SeatKind::Agent);
    for (verb, args) in [
        ("comp.input.key", json!({"key":"a"})),
        ("comp.input.key", json!({"text":"a"})),
        ("comp.input.pointer.move", json!({"x":1,"y":2})),
        ("comp.input.pointer.button", Value::Null),
        ("comp.input.pointer.scroll", json!({"dy":15})),
    ] {
        assert!(matches!(parse_input_op(verb, &args).unwrap(), InputOp::OnSeat { seat: SeatKind::Agent, .. }), "{verb}");
    }
    let LongOp::Sequence(steps) = parse_sequence(&json!({"steps":[
        {"verb":"comp.input.key","args":{"text":"a"}},
        {"verb":"comp.input.key","args":{"text":"b","seat":"human"}},
        {"verb":"comp.input.release_all"}
    ]})).unwrap() else { panic!("default sequence") };
    assert!(matches!(steps[0].op, InputOp::OnSeat { seat: SeatKind::Agent, .. }));
    assert_eq!(steps[1].op, InputOp::Text("b".into()));
    assert_eq!(steps[2].op, InputOp::ReleaseAll);
}

#[test]
fn sequence_seat_is_inherited_and_each_step_can_override_it() {
    let LongOp::SeatedSequence { seat, steps } = parse_sequence(&json!({"seat":"agent","steps":[
        {"verb":"comp.input.key","args":{"text":"a"}},
        {"verb":"comp.input.key","args":{"text":"b","seat":"human"}},
        {"verb":"comp.input.release_all"}
    ]})).unwrap() else { panic!("seated sequence") };
    assert_eq!(seat, SeatKind::Agent);
    assert!(matches!(steps[0].op, InputOp::OnSeat { seat:SeatKind::Agent, .. }));
    assert_eq!(steps[1].op, InputOp::Text("b".into()));
    assert!(matches!(steps[2].op, InputOp::OnSeat { seat:SeatKind::Agent, .. }));
}

/// The verb-to-scope mapping is the only production path into the
/// scoped snapshot: pin it,
/// and pin that a list prefix and its scope agree at segment boundaries.
#[test]
fn read_scope_names_each_snapshot_verbs_subtree() {
    let list = |prefix: &str| read_scope("comp.props.list", &json!({ "prefix": prefix }));
    assert_eq!(list("windows"), Some("windows".to_string()));
    assert_eq!(list("windows.s3"), Some("windows.s3".to_string()));
    assert_eq!(list("windows.s"), Some("windows.s".to_string()));
    assert_eq!(
        read_scope("comp.props.list", &json!({})),
        None,
        "no prefix: whole tree"
    );
    assert_eq!(
        read_scope("comp.props.get", &json!({ "path": "windows.s3.visible" })),
        Some("windows.s3.visible".to_string())
    );
    assert_eq!(
        read_scope("comp.props.describe", &json!({ "path": "windows.s3" })),
        Some("windows.s3".to_string())
    );
    assert_eq!(
        read_scope("comp.info", &json!({})),
        Some("info".to_string())
    );
    assert_eq!(read_scope("comp.windows.list", &json!({})), None);
    // A scope reaches its own subtree and no sibling; a mid-segment
    // prefix scopes nothing, exactly as the list itself matches nothing.
    let mut scopes = ReadScopes::Paths(Vec::new());
    scopes.add(list("windows.s3").as_deref());
    assert!(scopes.wants("windows.s3.presentation"));
    assert!(!scopes.wants("windows.s30.presentation"));
    let mut partial = ReadScopes::Paths(Vec::new());
    partial.add(list("windows.s").as_deref());
    assert!(!partial.wants("windows.s3.presentation"));
}

#[test]
fn service_name_validation_matches_abp_grammar() {
    for valid in ["comp", "comp-nested", "a0"] {
        assert!(validate_service_name(valid).is_ok(), "{valid}");
    }
    for invalid in ["c", "Comp", "comp_nested", "-comp", "comp-é"] {
        assert!(validate_service_name(invalid).is_err(), "{invalid}");
    }
}

#[test]
fn window_verb_arguments_parse_with_both_or_neither_target_fields() {
    assert_eq!(
        parse_window_op("comp.window.restore", &Value::Null),
        Ok(WindowOp::Restore { target: None })
    );
    assert_eq!(
        parse_window_op("comp.window.restore", &json!({})),
        Ok(WindowOp::Restore { target: None })
    );
    assert_eq!(
        parse_window_op("comp.window.restore", &json!({"id": 7, "generation": 3})),
        Ok(WindowOp::Restore {
            target: Some((7, 3))
        })
    );
    assert_eq!(
        parse_window_op("comp.window.minimize", &json!({"id": 7, "generation": 3})),
        Ok(WindowOp::Minimize {
            id: 7,
            generation: 3
        })
    );
    for (verb, args, field) in [
        ("comp.window.restore", json!({"id": 7}), "generation"),
        ("comp.window.restore", json!({"generation": 3}), "id"),
        (
            "comp.window.restore",
            json!({"id": "7", "generation": 3}),
            "id",
        ),
        (
            "comp.window.restore",
            json!({"id": 7, "generation": -1}),
            "generation",
        ),
        ("comp.window.restore", json!([7, 3]), "args"),
        ("comp.window.minimize", json!({}), "id"),
        ("comp.window.minimize", json!({"id": 7}), "generation"),
    ] {
        let Err(reply) = parse_window_op(verb, &args) else {
            panic!("{verb} {args} must be refused");
        };
        let (rc, body) = reply.into_wire();
        let body = serde_json::from_str::<Value>(&body).unwrap();
        assert_eq!(rc, 10, "{verb} {args}");
        assert_eq!(body["error"], "invalid_value", "{verb} {args}");
        assert_eq!(body["path"], field, "{verb} {args}");
    }
}

#[test]
fn stats_verb_arguments_name_a_window_or_a_source() {
    let window = StatsTarget::Window {
        id: 7,
        generation: 3,
    };
    assert_eq!(
        parse_stats_op("comp.window.stats", &json!({"id": 7, "generation": 3})),
        Ok(WindowOp::Stats {
            target: window.clone(),
            samples: STATS_RING,
        })
    );
    assert_eq!(
        parse_stats_op(
            "comp.window.stats",
            &json!({"source": "scene", "registration": 2, "samples": 0})
        ),
        Ok(WindowOp::Stats {
            target: StatsTarget::Source {
                id: "scene".into(),
                registration: Some(2),
            },
            samples: 0,
        })
    );
    assert_eq!(
        parse_stats_op("comp.window.stats.reset", &Value::Null),
        Ok(WindowOp::StatsReset { target: None })
    );
    assert_eq!(
        parse_stats_op(
            "comp.window.stats.reset",
            &json!({"id": 7, "generation": 3})
        ),
        Ok(WindowOp::StatsReset {
            target: Some(window)
        })
    );
    assert_eq!(
        parse_stats_op("comp.window.stats.reset", &json!({"source": "scene"})),
        Ok(WindowOp::StatsReset {
            target: Some(StatsTarget::Source {
                id: "scene".into(),
                registration: None,
            })
        })
    );
    for (verb, args, field) in [
        (
            "comp.window.stats",
            json!({"id": 7, "generation": 3, "source": "scene"}),
            "source",
        ),
        ("comp.window.stats", json!({}), "id"),
        ("comp.window.stats", json!({"id": 7}), "generation"),
        ("comp.window.stats", json!({"source": "Bad.Id"}), "source"),
        ("comp.window.stats", json!({"source": 7}), "source"),
        (
            "comp.window.stats",
            json!({"source": "scene", "samples": 513}),
            "samples",
        ),
        (
            "comp.window.stats",
            json!({"id": 7, "generation": 3, "registration": 1}),
            "registration",
        ),
        (
            "comp.window.stats.reset",
            json!({"registration": 1}),
            "source",
        ),
        ("comp.window.stats.reset", json!({"generation": 3}), "id"),
        ("comp.window.stats.reset", json!([1]), "args"),
    ] {
        let Err(reply) = parse_stats_op(verb, &args) else {
            panic!("{verb} {args} must be refused");
        };
        let (rc, body) = reply.into_wire();
        let body = serde_json::from_str::<Value>(&body).unwrap();
        assert_eq!(rc, 10, "{verb} {args}");
        assert_eq!(body["error"], "invalid_value", "{verb} {args}");
        assert_eq!(body["path"], field, "{verb} {args}");
    }
    let reply = parse_stats_op("comp.window.stats.reset", &json!({"samples": 1}))
        .expect_err("reset takes no samples");
    let body = serde_json::from_str::<Value>(&reply.into_wire().1).unwrap();
    assert_eq!(body["error"], "invalid_args");
    assert_eq!(body["field"], "samples");
}

#[test]
fn window_verb_unknown_fields_are_refused_by_name() {
    for verb in ["comp.window.restore", "comp.window.minimize"] {
        let reply = parse_window_op(verb, &json!({"id": 7, "gen": 3}))
            .expect_err("a typo must not be ignored");
        let (rc, body) = reply.into_wire();
        assert_eq!(rc, 10);
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap(),
            json!({"error": "invalid_args", "field": "gen", "allowed": ["id", "generation"]}),
            "{verb}"
        );
    }
}

#[test]
fn step_eight_window_verbs_parse_and_refuse_by_field() {
    assert_eq!(
        parse_window_verb("comp.window.focus", &json!({"id": 7, "generation": 3})),
        Ok(WindowVerb::Op(WindowOp::Focus {
            id: 7,
            generation: 3,
            raise: true
        }))
    );
    assert_eq!(
        parse_window_verb(
            "comp.window.focus",
            &json!({"id": 7, "generation": 3, "raise": false})
        ),
        Ok(WindowVerb::Op(WindowOp::Focus {
            id: 7,
            generation: 3,
            raise: false
        }))
    );
    assert_eq!(
        parse_window_verb("comp.window.raise", &json!({"id": 7, "generation": 3})),
        Ok(WindowVerb::Op(WindowOp::Raise {
            id: 7,
            generation: 3
        }))
    );
    assert_eq!(
        parse_window_verb("comp.window.close", &json!({"id": 7, "generation": 3})),
        Ok(WindowVerb::Op(WindowOp::Close {
            id: 7,
            generation: 3
        }))
    );
    assert_eq!(
        parse_window_verb(
            "comp.window.close",
            &json!({"id": 7, "generation": 3, "force": true})
        ),
        Ok(WindowVerb::Long(LongOp::ForceClose {
            id: 7,
            generation: 3,
            timeout: CLOSE_FORCE_DEFAULT
        }))
    );
    assert_eq!(
        parse_window_verb(
            "comp.window.close",
            &json!({"id": 7, "generation": 3, "force": true, "timeout_ms": 250})
        ),
        Ok(WindowVerb::Long(LongOp::ForceClose {
            id: 7,
            generation: 3,
            timeout: Duration::from_millis(250)
        }))
    );
    assert_eq!(
        parse_window_verb(
            "comp.window.place",
            &json!({"id": 7, "generation": 3, "output": "o_x", "x": 10, "height": 300})
        ),
        Ok(WindowVerb::Op(WindowOp::Place(PlaceSpec {
            id: 7,
            generation: 3,
            output: Some("o_x".into()),
            x: Some(10.0),
            y: None,
            width: None,
            height: Some(300),
        })))
    );
    assert_eq!(
        parse_window_verb(
            "comp.window.wait",
            &json!({"match": {"app_id": "a", "title_contains": "b"}, "until": "presented"})
        ),
        Ok(WindowVerb::Long(LongOp::Wait(WaitSpec {
            window: WindowMatch {
                app_id: Some("a".into()),
                title_contains: Some("b".into()),
                ..WindowMatch::default()
            },
            until: WaitUntil::Presented,
            timeout: WINDOW_WAIT_DEFAULT,
        })))
    );
    assert_eq!(
        parse_window_verb(
            "comp.window.wait",
            &json!({
                "match": {"id": 7},
                "until": "size",
                "width": 500,
                "height": 300,
                "timeout_ms": 60_000,
            })
        ),
        Ok(WindowVerb::Long(LongOp::Wait(WaitSpec {
            window: WindowMatch {
                id: Some(7),
                ..WindowMatch::default()
            },
            until: WaitUntil::Size {
                width: 500,
                height: 300
            },
            timeout: LONG_VERB_MAX,
        })))
    );

    for (verb, args, path) in [
        ("comp.window.focus", json!({"id": 7}), "generation"),
        (
            "comp.window.focus",
            json!({"id": 7, "generation": 3, "raise": 1}),
            "raise",
        ),
        ("comp.window.raise", json!({"generation": 3}), "id"),
        (
            "comp.window.close",
            json!({"id": 7, "generation": 3, "timeout_ms": 5}),
            "timeout_ms",
        ),
        (
            "comp.window.close",
            json!({"id": 7, "generation": 3, "force": true, "timeout_ms": 60_001}),
            "timeout_ms",
        ),
        ("comp.window.place", json!({"id": 7, "generation": 3}), "x"),
        (
            "comp.window.place",
            json!({"id": 7, "generation": 3, "width": 0}),
            "width",
        ),
        (
            "comp.window.place",
            json!({"id": 7, "generation": 3, "y": "1"}),
            "y",
        ),
        ("comp.window.wait", json!({"until": "mapped"}), "match"),
        (
            "comp.window.wait",
            json!({"match": {}, "until": "mapped"}),
            "match",
        ),
        (
            "comp.window.wait",
            json!({"match": {"generation": 3}, "until": "mapped"}),
            "match.id",
        ),
        ("comp.window.wait", json!({"match": {"id": 7}}), "until"),
        (
            "comp.window.wait",
            json!({"match": {"id": 7}, "until": "resized"}),
            "until",
        ),
        (
            "comp.window.wait",
            json!({"match": {"id": 7}, "until": "size", "width": 5}),
            "width",
        ),
        (
            "comp.window.wait",
            json!({"match": {"id": 7}, "until": "mapped", "height": 5}),
            "width",
        ),
        (
            "comp.window.wait",
            json!({"match": {"id": 7}, "until": "mapped", "timeout_ms": 0}),
            "timeout_ms",
        ),
    ] {
        let body = refusal(parse_window_verb(verb, &args).expect_err("refused"));
        assert_eq!(body["error"], "invalid_value", "{verb} {args}: {body}");
        assert_eq!(body["path"], path, "{verb} {args}: {body}");
    }
    for (verb, args, field) in [
        (
            "comp.window.focus",
            json!({"id": 7, "generation": 3, "rise": true}),
            "rise",
        ),
        (
            "comp.window.close",
            json!({"id": 7, "generation": 3, "kill": true}),
            "kill",
        ),
        (
            "comp.window.place",
            json!({"id": 7, "generation": 3, "w": 5}),
            "w",
        ),
        (
            "comp.window.wait",
            json!({"match": {"appid": "x"}, "until": "mapped"}),
            "match.appid",
        ),
        (
            "comp.window.wait",
            json!({"match": {"id": 1}, "until": "mapped", "for": 1}),
            "for",
        ),
    ] {
        let body = refusal(parse_window_verb(verb, &args).expect_err("typo refused"));
        assert_eq!(body["error"], "invalid_args", "{verb}");
        assert_eq!(body["field"], field, "{verb}");
    }
}

/// `comp.workspace.switch` / `comp.window.send_to_workspace`: `index`
/// is `>= 1`, `next` or `prev` (0 is `invalid_value` at the parser),
/// `wrap` defaults on, `follow` off, and a typo is refused by name.
#[test]
fn workspace_verbs_parse_and_refuse_by_field() {
    assert!(window_verb("comp.workspace.switch").is_some());
    assert!(window_verb("comp.window.send_to_workspace").is_some());
    assert_eq!(
        parse_window_verb("comp.workspace.switch", &json!({"index": "next"})),
        Ok(WindowVerb::Op(WindowOp::SwitchWorkspace {
            output: None,
            index: WorkspaceIndex::Next,
            wrap: true,
        }))
    );
    assert_eq!(
        parse_window_verb(
            "comp.workspace.switch",
            &json!({"index": 3, "output": "o_x", "wrap": false})
        ),
        Ok(WindowVerb::Op(WindowOp::SwitchWorkspace {
            output: Some("o_x".into()),
            index: WorkspaceIndex::Absolute(3),
            wrap: false,
        }))
    );
    assert_eq!(
        parse_window_verb("comp.workspace.switch", &json!({"index": "prev"})),
        Ok(WindowVerb::Op(WindowOp::SwitchWorkspace {
            output: None,
            index: WorkspaceIndex::Prev,
            wrap: true,
        }))
    );
    assert_eq!(
        parse_window_verb(
            "comp.window.send_to_workspace",
            &json!({"id": 7, "generation": 3, "index": 2})
        ),
        Ok(WindowVerb::Op(WindowOp::SendToWorkspace {
            id: 7,
            generation: 3,
            index: WorkspaceIndex::Absolute(2),
            follow: false,
        }))
    );
    assert_eq!(
        parse_window_verb(
            "comp.window.send_to_workspace",
            &json!({"id": 7, "generation": 3, "index": "prev", "follow": true})
        ),
        Ok(WindowVerb::Op(WindowOp::SendToWorkspace {
            id: 7,
            generation: 3,
            index: WorkspaceIndex::Prev,
            follow: true,
        }))
    );
    for (verb, args, path) in [
        ("comp.workspace.switch", json!({}), "index"),
        ("comp.workspace.switch", json!({"index": 0}), "index"),
        ("comp.workspace.switch", json!({"index": -1}), "index"),
        ("comp.workspace.switch", json!({"index": 1.5}), "index"),
        (
            "comp.workspace.switch",
            json!({"index": "sideways"}),
            "index",
        ),
        (
            "comp.workspace.switch",
            json!({"index": 1, "wrap": "no"}),
            "wrap",
        ),
        (
            "comp.workspace.switch",
            json!({"index": 1, "output": ""}),
            "output",
        ),
        (
            "comp.workspace.switch",
            json!({"index": 1, "output": 3}),
            "output",
        ),
        (
            "comp.window.send_to_workspace",
            json!({"id": 7, "index": 2}),
            "generation",
        ),
        (
            "comp.window.send_to_workspace",
            json!({"generation": 3, "index": 2}),
            "id",
        ),
        (
            "comp.window.send_to_workspace",
            json!({"id": 7, "generation": 3}),
            "index",
        ),
        (
            "comp.window.send_to_workspace",
            json!({"id": 7, "generation": 3, "index": 0}),
            "index",
        ),
        (
            "comp.window.send_to_workspace",
            json!({"id": 7, "generation": 3, "index": 2, "follow": 1}),
            "follow",
        ),
    ] {
        let body = refusal(parse_window_verb(verb, &args).expect_err("refused"));
        assert_eq!(body["error"], "invalid_value", "{verb} {args}: {body}");
        assert_eq!(body["path"], path, "{verb} {args}: {body}");
    }
    let body = refusal(
        parse_window_verb(
            "comp.window.send_to_workspace",
            &json!({"id": 7, "index": 2}),
        )
        .expect_err("refused"),
    );
    assert_eq!(body["range"], "required (read windows.s<id>.generation)");
    for (verb, args, field) in [
        (
            "comp.workspace.switch",
            json!({"index": 1, "wrap_around": true}),
            "wrap_around",
        ),
        (
            "comp.window.send_to_workspace",
            json!({"id": 7, "generation": 3, "index": 2, "output": "o_x"}),
            "output",
        ),
    ] {
        let body = refusal(parse_window_verb(verb, &args).expect_err("typo refused"));
        assert_eq!(body["error"], "invalid_args", "{verb}");
        assert_eq!(body["field"], field, "{verb}");
    }
}

#[test]
fn set_generation_is_accepted_only_on_window_leaves() {
    let (path, value, generation) =
        parse_set(&json!({"path": "windows.s7.minimized", "value": true, "generation": 3}))
            .expect("fenced window write parses");
    assert_eq!(
        (path.as_str(), value, generation),
        ("windows.s7.minimized", json!(true), Some(3))
    );
    let (_, _, generation) =
        parse_set(&json!({"path": "windows.s7.band", "value": "bottom"})).unwrap();
    assert_eq!(generation, None, "the fence is optional");
    for args in [
        json!({"path": "input.corners.enabled", "value": true, "generation": 3}),
        json!({"path": "windows.s7.minimized", "value": true, "generation": "3"}),
    ] {
        let Err((rc, body)) = parse_set(&args) else {
            panic!("{args} must be refused");
        };
        let body = serde_json::from_str::<Value>(&body).unwrap();
        assert_eq!(rc, 10);
        assert_eq!(body["error"], "invalid_value");
        assert_eq!(body["path"], "generation");
    }
}

fn move_op(target: PointerMoveTarget) -> InputOp {
    InputOp::PointerMove {
        target,
        corners: true,
    }
}

fn refusal(reply: ControlReply) -> Value {
    let (rc, body) = reply.into_wire();
    assert_eq!(rc, 10, "{body}");
    serde_json::from_str(&body).unwrap()
}

#[test]
fn input_verbs_parse_every_documented_form() {
    assert_eq!(
        parse_input_op("comp.input.pointer.move", &json!({"seat": "human", "x": 40, "y": 30.5})),
        Ok(move_op(PointerMoveTarget::Output {
            output: None,
            x: 40.0,
            y: 30.5
        }))
    );
    assert_eq!(
        parse_input_op(
            "comp.input.pointer.move",
            &json!({"seat": "human", "output": "o_nested", "x": 1, "y": 2})
        ),
        Ok(move_op(PointerMoveTarget::Output {
            output: Some("o_nested".into()),
            x: 1.0,
            y: 2.0
        }))
    );
    assert_eq!(
        parse_input_op("comp.input.pointer.move", &json!({"seat": "human", "dx": -3})),
        Ok(move_op(PointerMoveTarget::Relative { dx: -3.0, dy: 0.0 }))
    );
    assert_eq!(
        parse_input_op(
            "comp.input.pointer.move",
            &json!({"seat": "human", "window": {"id": 7, "generation": 3}, "x": 4, "y": 5, "require_hit": true})
        ),
        Ok(move_op(PointerMoveTarget::Window {
            id: 7,
            generation: 3,
            x: 4.0,
            y: 5.0,
            require_hit: true
        }))
    );
    assert_eq!(
        parse_input_op("comp.input.pointer.button", &json!({"seat": "human"})),
        Ok(InputOp::PointerButton {
            button: BTN_LEFT,
            action: PressAction::Both
        })
    );
    assert_eq!(
        parse_input_op(
            "comp.input.pointer.button",
            &json!({"seat": "human", "button": "right", "action": "press"})
        ),
        Ok(InputOp::PointerButton {
            button: BTN_RIGHT,
            action: PressAction::Press
        })
    );
    assert_eq!(
        parse_input_op(
            "comp.input.pointer.button",
            &json!({"seat": "human", "button": 0x113, "action": "release"})
        ),
        Ok(InputOp::PointerButton {
            button: 0x113,
            action: PressAction::Release
        })
    );
    // A wheel derives detents (15 units = 120); a finger has none and a
    // missing axis stays missing.
    assert_eq!(
        parse_input_op("comp.input.pointer.scroll", &json!({"seat": "human", "dy": 15})),
        Ok(InputOp::PointerScroll {
            dx: None,
            dy: Some(15.0),
            source: ScrollSource::Wheel,
            v120: (None, Some(120))
        })
    );
    assert_eq!(
        parse_input_op(
            "comp.input.pointer.scroll",
            &json!({"seat": "human", "dx": 0, "source": "finger"})
        ),
        Ok(InputOp::PointerScroll {
            dx: Some(0.0),
            dy: None,
            source: ScrollSource::Finger,
            v120: (None, None)
        })
    );
    assert_eq!(
        parse_input_op(
            "comp.input.pointer.scroll",
            &json!({"seat": "human", "dy": 10, "v120": {"dy": -240}})
        ),
        Ok(InputOp::PointerScroll {
            dx: None,
            dy: Some(10.0),
            source: ScrollSource::Wheel,
            v120: (None, Some(-240))
        })
    );
    assert_eq!(
        parse_input_op(
            "comp.input.key",
            &json!({"seat": "human", "key": "q", "modifiers": ["super", "shift"]})
        ),
        Ok(InputOp::Key {
            key: KeySpec::Name("q".into()),
            action: PressAction::Both,
            modifiers: vec![
                KeySpec::Name("Super_L".into()),
                KeySpec::Name("Shift_L".into())
            ],
        })
    );
    assert_eq!(
        parse_input_op("comp.input.key", &json!({"seat": "human", "key": 28, "action": "press"})),
        Ok(InputOp::Key {
            key: KeySpec::Evdev(28),
            action: PressAction::Press,
            modifiers: Vec::new(),
        })
    );
    assert_eq!(
        parse_input_op("comp.input.key", &json!({"seat": "human", "text": "ok\n"})),
        Ok(InputOp::Text("ok\n".into()))
    );
    assert_eq!(
        parse_input_op("comp.input.release_all", &json!({})),
        Ok(InputOp::ReleaseAll)
    );
}

#[test]
fn input_verbs_refuse_ambiguous_and_out_of_range_arguments() {
    for (verb, args, path) in [
        ("comp.input.pointer.move", json!({"x": 1}), "y"),
        (
            "comp.input.pointer.move",
            json!({"x": 1, "y": 2, "dx": 1}),
            "dx",
        ),
        (
            "comp.input.pointer.move",
            json!({"x": 1, "y": 2, "require_hit": true}),
            "require_hit",
        ),
        (
            "comp.input.pointer.move",
            json!({"window": {"id": 7}, "x": 1, "y": 2}),
            "window.generation",
        ),
        (
            "comp.input.pointer.move",
            json!({"window": {"id": 7, "generation": 1}, "dx": 1, "x": 1, "y": 2}),
            "window",
        ),
        ("comp.input.pointer.move", json!({"x": "1", "y": 2}), "x"),
        ("comp.input.pointer.move", json!({"x": 1e9, "y": 2}), "x"),
        (
            "comp.input.pointer.button",
            json!({"button": "back"}),
            "button",
        ),
        ("comp.input.pointer.button", json!({"button": 30}), "button"),
        (
            "comp.input.pointer.button",
            json!({"action": "tap"}),
            "action",
        ),
        ("comp.input.pointer.scroll", json!({}), "dy"),
        (
            "comp.input.pointer.scroll",
            json!({"dy": 1, "source": "finger", "v120": {"dy": 120}}),
            "v120",
        ),
        (
            "comp.input.pointer.scroll",
            json!({"dy": 1, "v120": {"dx": 120}}),
            "v120.dx",
        ),
        ("comp.input.key", json!({}), "key"),
        ("comp.input.key", json!({"key": ""}), "key"),
        ("comp.input.key", json!({"key": 0}), "key"),
        (
            "comp.input.key",
            json!({"key": "a", "action": "click"}),
            "action",
        ),
        (
            "comp.input.key",
            json!({"key": "a", "modifiers": ["hyper"]}),
            "modifiers",
        ),
        ("comp.input.key", json!({"text": "a", "key": "b"}), "text"),
        ("comp.input.key", json!({"text": ""}), "text"),
        ("comp.input.key", json!({"text": "x".repeat(4097)}), "text"),
        ("comp.input.release_all", json!([1]), "args"),
    ] {
        let Err(reply) = parse_input_op(verb, &args) else {
            panic!("{verb} {args} must be refused");
        };
        let body = refusal(reply);
        assert_eq!(body["error"], "invalid_value", "{verb} {args}: {body}");
        assert_eq!(body["path"], path, "{verb} {args}: {body}");
    }
    for (verb, args, field) in [
        (
            "comp.input.pointer.move",
            json!({"x": 1, "y": 2, "screen": 0}),
            "screen",
        ),
        (
            "comp.input.pointer.move",
            json!({"window": {"id": 7, "generation": 1, "gen": 1}, "x": 1, "y": 2}),
            "window.gen",
        ),
        ("comp.input.pointer.button", json!({"btn": "left"}), "btn"),
        (
            "comp.input.pointer.scroll",
            json!({"dy": 1, "discrete": 1}),
            "discrete",
        ),
        ("comp.input.key", json!({"key": "a", "mods": []}), "mods"),
        ("comp.input.release_all", json!({"all": true}), "all"),
    ] {
        let body = refusal(parse_input_op(verb, &args).expect_err("typo refused"));
        assert_eq!(body["error"], "invalid_args", "{verb}");
        assert_eq!(body["field"], field, "{verb}");
    }
}

#[test]
fn sequence_parses_delays_and_names_the_failing_step() {
    let Ok(LongOp::Sequence(steps)) = parse_sequence(&json!({
        "interval_ms": 10,
        "steps": [
            {"verb": "comp.input.pointer.button", "args": {"action": "press"}, "delay_ms": 0},
            {"verb": "comp.input.pointer.move", "args": {"dx": 5}},
            {"verb": "comp.input.release_all"},
        ],
    })) else {
        panic!("sequence parses");
    };
    assert_eq!(
        steps
            .iter()
            .map(|step| (step.verb, step.delay))
            .collect::<Vec<_>>(),
        [
            ("comp.input.pointer.button", Duration::ZERO),
            ("comp.input.pointer.move", Duration::from_millis(10)),
            ("comp.input.release_all", Duration::from_millis(10)),
        ]
    );
    assert_eq!(LongOp::Sequence(steps).budget(), Duration::from_millis(20));

    let body = refusal(
        parse_sequence(&json!({"steps": [
            {"verb": "comp.input.key", "args": {"key": "a"}},
            {"verb": "comp.input.key", "args": {"key": "a", "action": "hold"}},
        ]}))
        .expect_err("bad step refused"),
    );
    assert_eq!(body["path"], "steps[1].args.action");
    let body = refusal(
        parse_sequence(&json!({"steps": [{"verb": "comp.input.key", "args": {"k": 1}}]}))
            .expect_err("bad step field refused"),
    );
    assert_eq!(body["field"], "steps[0].args.k");
    for (args, path) in [
        (json!({"steps": []}), "steps"),
        (
            json!({"steps": [{"verb": "comp.window.focus"}]}),
            "steps.verb",
        ),
        (
            json!({"steps": [{"verb": "comp.input.sequence"}]}),
            "steps.verb",
        ),
        (
            json!({"steps": [
                {"verb": "comp.input.release_all", "delay_ms": 40_000},
                {"verb": "comp.input.release_all", "delay_ms": 30_000},
            ]}),
            "steps",
        ),
        (
            json!({"steps": [{"verb": "comp.input.release_all"}], "interval_ms": -1}),
            "interval_ms",
        ),
    ] {
        let body = refusal(parse_sequence(&args).expect_err("refused"));
        assert_eq!(body["path"], path, "{args}");
    }
    let too_many = vec![json!({"verb": "comp.input.release_all"}); SEQUENCE_MAX_STEPS + 1];
    let body = refusal(parse_sequence(&json!({"steps": too_many})).expect_err("capped"));
    assert_eq!(body["path"], "steps");
    let body = refusal(
        parse_sequence(&json!({"steps": [{"verb": "comp.input.release_all", "wait": 1}]}))
            .expect_err("unknown step field"),
    );
    assert_eq!(body["field"], "steps[0].wait");
}

#[test]
fn one_verb_is_capped_at_4096_injected_events() {
    let text = "a".repeat(TEXT_MAX_CHARS);
    let step = json!({"verb": "comp.input.key", "args": {"text": text}});
    // 4 x 256 x 4 = 4096 fits exactly; one more event does not.
    let Ok(LongOp::Sequence(steps)) = parse_sequence(&json!({"steps": vec![step.clone(); 4]}))
    else {
        panic!("exactly at the cap parses");
    };
    assert_eq!(
        steps
            .iter()
            .map(|step| step.op.event_bound())
            .sum::<usize>(),
        MAX_EVENTS_PER_VERB
    );
    let mut over = vec![step; 4];
    over.push(json!({"verb": "comp.input.pointer.move", "args": {"dx": 1}}));
    let body = refusal(parse_sequence(&json!({"steps": over})).expect_err("over the cap"));
    assert_eq!(body["path"], "steps");
    assert!(body["range"].as_str().unwrap().contains("4096"), "{body}");
    let body = refusal(
        parse_input_op(
            "comp.input.key",
            &json!({"text": "a".repeat(TEXT_MAX_CHARS + 1)}),
        )
        .expect_err("text over the cap"),
    );
    assert_eq!(body["path"], "text");
    assert_eq!(
        parse_input_op(
            "comp.input.pointer.move",
            &json!({"seat": "human", "dx": 1, "corners": false})
        ),
        Ok(InputOp::PointerMove {
            target: PointerMoveTarget::Relative { dx: 1.0, dy: 0.0 },
            corners: false
        })
    );
    let body = refusal(
        parse_window_verb(
            "comp.window.wait",
            &json!({"match": {"id": 7, "app_id": "a"}, "until": "mapped"}),
        )
        .expect_err("id with names"),
    );
    assert_eq!(body["path"], "match");
}

#[test]
fn long_admission_budget_is_the_verb_deadline_plus_slack() {
    let op = LongOp::Sequence(vec![SequenceStep {
        verb: "comp.input.release_all",
        op: InputOp::ReleaseAll,
        delay: Duration::from_millis(1500),
    }]);
    assert_eq!(
        op.admission_timeout(),
        Duration::from_millis(1500) + LONG_VERB_SLACK
    );
    // The cap applies before the slack.
    let long = LongOp::Wait(WaitSpec {
        window: WindowMatch {
            id: Some(1),
            ..WindowMatch::default()
        },
        until: WaitUntil::Mapped,
        timeout: Duration::from_secs(600),
    });
    assert_eq!(long.admission_timeout(), LONG_VERB_MAX + LONG_VERB_SLACK);
}

#[test]
fn refused_reply_carries_code_and_detail() {
    assert_eq!(
        ControlReply::refused("occluded", json!({"id": 7, "error": "ignored"})).into_wire(),
        (10, Arc::from(r#"{"error":"occluded","id":7}"#))
    );
    assert_eq!(
        ControlReply::refused("busy", Value::Null).into_wire(),
        (10, Arc::from(r#"{"error":"busy"}"#))
    );
}

// ---- The dispatcher's routing, lifted into `classify`. ----

fn refused_body((rc, body): (u8, Arc<str>)) -> Value {
    assert_eq!(rc, 10, "{body}");
    serde_json::from_str(&body).unwrap()
}

/// On the routing alone: ping answers whatever the body, and each family
/// refuses a malformed body its own way.
#[test]
fn classify_refuses_a_malformed_body_per_family_and_ping_ignores_it() {
    assert!(body_is_malformed("{"));
    assert!(!body_is_malformed(""));
    assert!(!body_is_malformed("{}"));
    assert!(matches!(classify("comp.ping", &Value::Null, true), Ok(Request::Ping)));
    assert_eq!(PING_BODY, r#"{"pong":true}"#);
    for verb in ["comp.props.watch", "comp.pointer.watch", "comp.props.get", "comp.windows.list"] {
        let body = refused_body(classify(verb, &Value::Null, true).unwrap_err());
        assert_eq!(body, json!({"error": "unknown_path"}), "{verb}");
    }
    let body = refused_body(classify("comp.props.set", &Value::Null, true).unwrap_err());
    assert_eq!(
        body,
        json!({"error": "invalid_value", "path": null, "expected": "JSON property value", "range": "descriptor"})
    );
    for (verb, range) in [
        ("comp.window.focus", "{id, generation}"),
        ("comp.region.select", "{output?, timeout_ms?}"),
        ("comp.input.sequence", "{steps, interval_ms?}"),
        ("comp.input.key", "verb arguments"),
        ("comp.panel.hold", "{output, edge, surface, ...}"),
    ] {
        let body = refused_body(classify(verb, &Value::Null, true).unwrap_err());
        assert_eq!(body["error"], "invalid_value", "{verb}");
        assert_eq!(body["path"], "args", "{verb}");
        assert_eq!(body["range"], range, "{verb}");
    }
}

#[test]
fn classify_routes_every_family_and_keeps_verbs_literal() {
    assert!(matches!(
        classify("comp.window.focus", &json!({"id": 7, "generation": 3}), false),
        Ok(Request::Window(WindowOp::Focus { id: 7, generation: 3, raise: true }))
    ));
    assert!(matches!(
        classify("comp.window.wait", &json!({"match": {"id": 7}, "until": "mapped"}), false),
        Ok(Request::Long(LongOp::Wait(_)))
    ));
    assert!(matches!(
        classify("comp.region.select", &json!({}), false),
        Ok(Request::Long(LongOp::RegionSelect { .. }))
    ));
    assert!(matches!(
        classify("comp.input.sequence", &json!({"steps": [{"verb": "comp.input.release_all"}]}), false),
        Ok(Request::Long(LongOp::Sequence(_)))
    ));
    assert!(matches!(
        classify("comp.input.key", &json!({"key": "a"}), false),
        Ok(Request::Input(InputOp::OnSeat { seat: SeatKind::Agent, .. }))
    ));
    let Ok(Request::Panel(panel)) = classify(
        "comp.panel.mode",
        &json!({"output": "DP-1", "edge": "left", "surface": "quoin", "mode": "hidden"}),
        false,
    ) else {
        panic!("panel mode routes");
    };
    assert_eq!(panel.sender, "", "the transport stamps the sender");
    let Ok(Request::Set { path, generation, .. }) = classify(
        "comp.props.set",
        &json!({"path": "windows.s7.minimized", "value": true, "generation": 3}),
        false,
    ) else {
        panic!("a fenced set routes");
    };
    assert_eq!((path.as_str(), generation), ("windows.s7.minimized", Some(3)));
    // The ingress gate runs before admission.
    let body = refused_body(
        classify("comp.props.set", &json!({"path": "windows.s7.title", "value": "x"}), false)
            .unwrap_err(),
    );
    assert_eq!(body, json!({"error": "read_only"}));
    let Ok(Request::Read { verb, scope }) =
        classify("comp.props.get", &json!({"path": "windows.s3.visible"}), false)
    else {
        panic!("reads route");
    };
    assert_eq!((verb, scope.as_deref()), ("comp.props.get", Some("windows.s3.visible")));
    for unknown in ["comp-nested.panel.hold", "comp.nope", "noded.props.get", "comp.input.sequence.x"] {
        assert_eq!(
            refused_body(classify(unknown, &Value::Null, false).unwrap_err()),
            json!({"error": "unknown_verb"}),
            "{unknown}"
        );
    }
}

#[test]
fn every_refusal_gains_error_code_and_success_is_untouched() {
    let (rc, body) = with_error_code(10, Arc::from(r#"{"error":"stale_target","id":7}"#));
    assert_eq!(rc, 10);
    assert_eq!(body.as_ref(), r#"{"error":"stale_target","error_code":"stale_target","id":7}"#);
    let ok: Arc<str> = Arc::from(r#"{"error":"not a refusal"}"#);
    assert!(Arc::ptr_eq(&with_error_code(0, Arc::clone(&ok)).1, &ok));
    let already: Arc<str> = Arc::from(r#"{"error":"a","error_code":"b"}"#);
    assert_eq!(with_error_code(10, Arc::clone(&already)).1, already);
    let not_json: Arc<str> = Arc::from("busy");
    assert_eq!(with_error_code(10, Arc::clone(&not_json)).1, not_json);
    assert_eq!(
        ControlReply::WindowTarget {
            id: 7,
            error: WindowTargetError::StaleTarget { requested: 3, current: 4 },
        }
        .wire_json(),
        json!({"error": "stale_target", "error_code": "stale_target", "id": 7, "generation": 3, "current": 4})
    );
    for (error, code) in [
        (WindowTargetError::UnknownWindow, "unknown_window"),
        (WindowTargetError::NotManaged, "not_managed"),
        (WindowTargetError::NotMapped, "not_mapped"),
    ] {
        assert_eq!(
            ControlReply::WindowTarget { id: 9, error }.into_wire(),
            (10, Arc::from(format!(r#"{{"error":"{code}","id":9}}"#)))
        );
        assert_eq!(error.code(), code);
    }
}

/// Byte-identity guard: the frozen wire relies on serde_json's sorted maps.
/// A dependency that turns on `preserve_order` anywhere in the graph would
/// reorder every reply body; this test fails first.
#[test]
fn wire_object_keys_stay_sorted() {
    let (_, body) = ControlReply::Window {
        id: 7,
        generation: 3,
        title: None,
        app_id: Some(Arc::from("org.example.App")),
        minimized: false,
        changed: true,
    }
    .into_wire();
    assert_eq!(
        body.as_ref(),
        r#"{"app_id":"org.example.App","changed":true,"generation":3,"id":7,"minimized":false,"title":null}"#
    );
    let (_, body) = ControlReply::InvalidArgs {
        field: "gen".into(),
        allowed: &["id", "generation"],
    }
    .into_wire();
    assert_eq!(body.as_ref(), r#"{"allowed":["id","generation"],"error":"invalid_args","field":"gen"}"#);
}

/// The wire-limit rule alone (the byte measurement itself needs the Bus
/// framing and stays with the transport).
#[test]
fn an_oversized_reply_becomes_too_large_with_its_code() {
    let body: Arc<str> = Arc::from("x");
    assert_eq!(
        crate::reply::enforce_wire_limit(crate::reply::MAX_REPLY_WIRE_BYTES, 0, Arc::clone(&body)),
        (0, body)
    );
    let (rc, body) = crate::reply::enforce_wire_limit(crate::reply::MAX_REPLY_WIRE_BYTES + 1, 0, Arc::from("x"));
    assert_eq!(rc, 10);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap(),
        json!({
            "error": "too_large",
            "error_code": "too_large",
            "limit_bytes": crate::reply::MAX_REPLY_BODY_BYTES,
            "hint": "read a subtree",
        })
    );
    assert_eq!(crate::reply::MAX_REPLY_WIRE_BYTES, 8 * 1024 * 1024);
}

#[test]
fn agent_admissions_are_recognised_for_the_epoch_fence() {
    assert!(parse_input_op("comp.input.key", &json!({"key": "a"})).unwrap().uses_agent());
    assert!(!parse_input_op("comp.input.key", &json!({"key": "a", "seat": "human"})).unwrap().uses_agent());
    assert!(!parse_input_op("comp.input.release_all", &Value::Null).unwrap().uses_agent());
    assert!(parse_sequence(&json!({"steps": [{"verb": "comp.input.key", "args": {"text": "a"}}]}))
        .unwrap()
        .uses_agent());
    assert!(!parse_sequence(&json!({"steps": [{"verb": "comp.input.release_all"}]}))
        .unwrap()
        .uses_agent());
    assert_eq!(
        input_cleared_reply().into_wire(),
        (10, Arc::from(r#"{"error":"input_cleared","released":true,"seat":"agent"}"#))
    );
}

// Tests. Each test pins one rule engine-free. The harness halves (real
// devices, keymaps, popups, timers) stay with the engine.

use super::*;
use comp_model::request::{KeySpec, parse_input_op};
use surfaces::SurfaceRole;

fn bound() -> AgentTarget {
    AgentTarget {
        x11: false,
        root_mapped: true,
        tree_mapped: true,
        input_presentable: true,
        has_client: true,
        keyboard_bound: true,
        pointer_bound: true,
    }
}

fn error(reply: ControlReply) -> serde_json::Value {
    reply.wire_json()
}

fn key(action: PressAction) -> InputOp {
    InputOp::Key {
        key: KeySpec::Evdev(30),
        action,
        modifiers: Vec::new(),
    }
}

fn button(action: PressAction) -> InputOp {
    InputOp::PointerButton {
        button: comp_model::request::BTN_LEFT,
        action,
    }
}

/// agent_seat.rs `agent_refusal`: the hint and message ride along.
#[test]
fn agent_refusals_carry_the_human_seat_hint() {
    let body = error(agent_refusal("agent_seat_unbound"));
    assert_eq!(body["error"], "agent_seat_unbound");
    assert_eq!(body["error_code"], "agent_seat_unbound");
    assert_eq!(body["hint"]["seat"], "human");
    assert_eq!(body["message"], "target client is currently unbound on the agent seat");
    for reason in ["x11_unsupported", "chrome_target"] {
        assert_eq!(error(agent_refusal(reason))["hint"]["seat"], "human");
    }
    assert_eq!(error(agent_refusal("session_lock")), json!({"error": "session_lock", "error_code": "session_lock"}));
}

/// An unbound client is refused with the hint, and the lock wins first.
#[test]
fn unbound_clients_and_the_lock_refuse_before_anything_is_injected() {
    let mut facts = AgentSeatFacts {
        keyboard_target: Some(Some(AgentTarget {
            keyboard_bound: false,
            ..bound()
        })),
        pointer_target: Some(Some(AgentTarget {
            pointer_bound: false,
            ..bound()
        })),
        ..AgentSeatFacts::default()
    };
    for op in [key(PressAction::Both), button(PressAction::Both)] {
        assert_eq!(error(agent_preflight(&op, &facts).unwrap_err())["error"], "agent_seat_unbound");
    }
    facts.session_lock = true;
    assert_eq!(error(agent_preflight(&key(PressAction::Both), &facts).unwrap_err())["error"], "session_lock");
    // Even cleanup is refused under the lock (the lock is checked first).
    assert!(agent_preflight(&InputOp::ReleaseAll, &facts).is_err());
}

/// agent_seat.rs `agent_preflight` / `validate_agent_surface`: the order of
/// the refusals, and that releases always pass.
#[test]
fn preflight_refuses_in_comps_order_and_releases_always_pass() {
    let open = AgentSeatFacts {
        keyboard_target: Some(Some(bound())),
        pointer_target: Some(Some(bound())),
        ..AgentSeatFacts::default()
    };
    assert!(agent_preflight(&key(PressAction::Both), &open).is_ok());
    assert!(agent_preflight(&InputOp::Text("a".into()), &open).is_ok());
    let grabbed = AgentSeatFacts {
        keyboard_grab: true,
        pointer_grab: true,
        keyboard_target: None,
        pointer_target: None,
        ..open
    };
    for op in [key(PressAction::Release), button(PressAction::Release), InputOp::ReleaseAll] {
        assert!(agent_preflight(&op, &grabbed).is_ok(), "{op:?}");
    }
    assert_eq!(error(agent_preflight(&key(PressAction::Press), &grabbed).unwrap_err())["error"], "keyboard_grab");
    assert_eq!(error(agent_preflight(&button(PressAction::Press), &grabbed).unwrap_err())["error"], "pointer_grab");
    let targetless = AgentSeatFacts {
        keyboard_target: None,
        pointer_target: None,
        ..open
    };
    assert_eq!(error(agent_preflight(&key(PressAction::Both), &targetless).unwrap_err())["error"], "no_keyboard_target");
    assert_eq!(
        error(agent_preflight(&InputOp::PointerScroll {
            dx: None,
            dy: Some(1.0),
            source: comp_model::request::ScrollSource::Wheel,
            v120: (None, Some(8)),
        }, &targetless)
        .unwrap_err())["error"],
        "no_pointer_target"
    );
    // A press outside a popup is delivered to its grab to dismiss it.
    let popup = AgentSeatFacts {
        popup_pointer_grab: true,
        ..targetless
    };
    assert!(agent_preflight(&button(PressAction::Both), &popup).is_ok());
    // Moves are never refused here (their own path resolves the target).
    let moved = InputOp::PointerMove {
        target: comp_model::request::PointerMoveTarget::Relative { dx: 1.0, dy: 0.0 },
        corners: true,
    };
    assert!(agent_preflight(&moved, &targetless).is_ok());
    // The surface ladder: unrecorded root, X11, unmapped, not presentable,
    // clientless, unbound.
    for (target, reason) in [
        (None, "unmapped"),
        (Some(AgentTarget { x11: true, ..bound() }), "x11_unsupported"),
        (Some(AgentTarget { root_mapped: false, ..bound() }), "unmapped"),
        (Some(AgentTarget { tree_mapped: false, ..bound() }), "unmapped"),
        (Some(AgentTarget { input_presentable: false, ..bound() }), "not_presentable"),
        (Some(AgentTarget { has_client: false, ..bound() }), "unmapped"),
        (Some(AgentTarget { keyboard_bound: false, ..bound() }), "agent_seat_unbound"),
    ] {
        assert_eq!(error(validate_agent_surface(target, true, false).unwrap_err())["error"], reason);
    }
    assert!(validate_agent_surface(Some(AgentTarget { keyboard_bound: false, ..bound() }), false, false).is_ok());
}

fn mapped_window(registry: &mut Registry<u32>) -> (u64, u64) {
    let (id, generation) = registry.take_role(1, SurfaceRole::Toplevel, None).unwrap();
    registry.set_mapped(id, true).unwrap();
    registry.set_workspace(id, 1).unwrap();
    (id.0, generation)
}

/// agent_seat.rs `service_agent_targeted_input`: raise is refused, the lock
/// next, releases skip the target, then the fence, grabs and chrome.
#[test]
fn targeted_agent_input_is_fenced_and_never_raises() {
    let mut registry = Registry::<u32>::new();
    let (id, generation) = mapped_window(&mut registry);
    let open = AgentTargetedFacts {
        target: Some(bound()),
        hit: Some(bound()),
        ..AgentTargetedFacts::default()
    };
    let plan = |op: &InputOp, raise, facts: &AgentTargetedFacts| {
        agent_targeted_preflight(&registry, id, generation, raise, op, facts)
    };
    assert_eq!(error(plan(&key(PressAction::Both), true, &open).unwrap_err())["error"], "invalid_argument");
    let locked = AgentTargetedFacts { session_lock: true, ..open };
    assert_eq!(error(plan(&key(PressAction::Both), false, &locked).unwrap_err())["error"], "session_lock");
    // A release retires the hold even if the window is gone.
    assert_eq!(
        agent_targeted_preflight(&registry, 999, 1, false, &button(PressAction::Release), &open),
        Ok(AgentTargetedPlan::Release)
    );
    assert_eq!(
        error(agent_targeted_preflight(&registry, id, generation + 1, false, &key(PressAction::Both), &open).unwrap_err())["error"],
        "stale_target"
    );
    assert_eq!(
        plan(&key(PressAction::Both), false, &open),
        Ok(AgentTargetedPlan::Deliver { focus_keyboard: true, move_pointer: false })
    );
    assert_eq!(
        plan(&button(PressAction::Both), false, &open),
        Ok(AgentTargetedPlan::Deliver { focus_keyboard: true, move_pointer: true })
    );
    let grabbed = AgentTargetedFacts { keyboard_grabbed: true, ..open };
    assert_eq!(error(plan(&key(PressAction::Both), false, &grabbed).unwrap_err())["error"], "keyboard_grab");
    let popup = AgentTargetedFacts { matching_popup: true, ..grabbed };
    assert_eq!(
        plan(&key(PressAction::Both), false, &popup),
        Ok(AgentTargetedPlan::Deliver { focus_keyboard: false, move_pointer: false }),
        "a popup of the named window keeps its grab"
    );
    let elsewhere = AgentTargetedFacts { pointer_grabbed: true, ..open };
    assert_eq!(error(plan(&button(PressAction::Both), false, &elsewhere).unwrap_err())["error"], "pointer_grab");
    let on_target = AgentTargetedFacts { pointer_on_target: true, ..elsewhere };
    assert!(plan(&button(PressAction::Both), false, &on_target).is_ok());
    let chrome = AgentTargetedFacts { hit: None, ..open };
    let body = error(plan(&button(PressAction::Both), false, &chrome).unwrap_err());
    assert_eq!((body["error"].clone(), body["hint"]["seat"].clone()), (json!("chrome_target"), json!("human")));
    assert!(plan(&key(PressAction::Both), false, &chrome).is_ok(), "keys never hit-test");
}

/// Targeted-input refusals inject nothing and do not switch workspaces:
/// the human ladder names its reason, never switches.
#[test]
fn targeted_human_refusals_name_their_reason() {
    let mut registry = Registry::<u32>::new();
    let (id, generation) = mapped_window(&mut registry);
    let open = HumanTargetedFacts {
        current_workspace: 1,
        input_presentable: true,
        visible: true,
        ..HumanTargetedFacts::default()
    };
    let text = InputOp::Text("a".into());
    let reason = |registry: &Registry<u32>, op: &InputOp, facts: &HumanTargetedFacts| {
        human_targeted_preflight(registry, id, generation, op, facts)
            .unwrap_err()
            .wire_json()
    };
    assert_eq!(human_targeted_preflight(&registry, id, generation, &text, &open), Ok(HumanTargetedPlan::FocusThenDeliver));
    assert_eq!(
        human_targeted_preflight(&registry, id, generation + 1, &text, &open).unwrap_err().wire_json()["error"],
        "stale_target"
    );
    for (facts, expected) in [
        (HumanTargetedFacts { region_select: true, session_lock: true, ..open }, "region_select"),
        (HumanTargetedFacts { session_lock: true, exclusive_layer: true, ..open }, "session_lock"),
        (HumanTargetedFacts { exclusive_layer: true, ..open }, "exclusive_layer"),
        (HumanTargetedFacts { current_workspace: 2, ..open }, "other_workspace"),
        (HumanTargetedFacts { input_presentable: false, visible: false, ..open }, "not_presentable"),
        (HumanTargetedFacts { visible: false, ..open }, "not_visible"),
        (HumanTargetedFacts { keyboard_grab: true, ..open }, "keyboard_grab"),
    ] {
        let body = reason(&registry, &text, &facts);
        assert_eq!(body["error"], "target_unfocusable");
        assert_eq!(body["reason"], expected);
        assert_eq!((body["id"].clone(), body["generation"].clone()), (json!(id), json!(generation)));
    }
    let grabbed = HumanTargetedFacts { pointer_grab: true, ..open };
    assert!(human_targeted_preflight(&registry, id, generation, &text, &grabbed).is_ok(), "only buttons");
    assert_eq!(reason(&registry, &button(PressAction::Both), &grabbed)["reason"], "pointer_grab");
    // A key release skips the ladder; minimised and unmapped are named.
    assert_eq!(
        human_targeted_preflight(&registry, id, generation, &key(PressAction::Release), &HumanTargetedFacts { session_lock: true, ..open }),
        Ok(HumanTargetedPlan::Release)
    );
    registry.set_minimized(surfaces::SurfaceId(id), true).unwrap();
    assert_eq!(reason(&registry, &text, &open)["reason"], "minimized");
    registry.set_mapped(surfaces::SurfaceId(id), false).unwrap();
    assert_eq!(reason(&registry, &text, &open)["reason"], "unmapped");
}

/// A bare `release_all` cleans both seats; a scoped cleanup preserves the
/// other seat.
#[test]
fn release_all_scope_follows_the_seat_argument() {
    for (args, scope, reply) in [
        (json!({}), ReleaseScope::Both, "both"),
        (json!({"seat": "human"}), ReleaseScope::Seat(SeatKind::Human), "human"),
        (json!({"seat": "agent"}), ReleaseScope::Seat(SeatKind::Agent), "agent"),
    ] {
        let op = parse_input_op("comp.input.release_all", &args).unwrap();
        let parsed = ReleaseScope::of(&op).expect("a release_all");
        assert_eq!(parsed, scope);
        assert_eq!(parsed.reply_seat(), reply);
        assert_eq!(parsed.includes(SeatKind::Human), reply != "agent");
        assert_eq!(parsed.includes(SeatKind::Agent), reply != "human");
    }
    assert_eq!(ReleaseScope::of(&InputOp::Text("a".into())), None);
}

/// Shared holds are released by their last owner, and a failed sequence
/// releases only its own holds.
#[test]
fn shared_holds_are_released_by_their_last_owner() {
    let key_a = Hold::Key(38);
    let mut holds = Holds::default();
    holds.note(Some(1), key_a, true);
    holds.note(Some(2), key_a, true);
    assert_eq!(holds.owners_of(key_a), 2);
    assert_eq!(holds.drop_owner(Some(2)), Vec::<Hold>::new(), "the keeper still holds A");
    assert_eq!(holds.owners_of(key_a), 1);
    assert_eq!(holds.drop_owner(Some(1)), [key_a]);
    assert!(holds.is_empty());
    // A verb's release clears the run's claim; the verb's re-press is its own.
    holds.note(Some(3), key_a, true);
    holds.note(None, key_a, false);
    holds.note(None, key_a, true);
    assert_eq!(holds.owners_of(key_a), 1);
    assert_eq!(holds.drop_owner(Some(3)), Vec::<Hold>::new(), "the verb's hold survives");
    assert_eq!(holds.take_all(), [key_a]);
    assert!(holds.is_empty());
}

/// input_injection.rs `release_holds` and `release_agent_device_holds`.
#[test]
fn releases_go_newest_key_first_then_buttons_and_spare_physical_holds() {
    let holds = [Hold::Key(30), Hold::Button(0x110), Hold::Key(42), Hold::Key(31)];
    assert_eq!(
        release_order(&holds, |_| false),
        [Hold::Key(31), Hold::Key(42), Hold::Key(30), Hold::Button(0x110)]
    );
    assert_eq!(
        release_order(&holds, |hold| hold == Hold::Key(42)),
        [Hold::Key(31), Hold::Key(30), Hold::Button(0x110)]
    );
    let mut seat = Holds::default();
    for hold in holds {
        seat.note(None, hold, true);
    }
    assert_eq!(seat.take_kind(false), [Hold::Button(0x110)]);
    assert_eq!(seat.take_kind(true).len(), 3);
    assert!(seat.is_empty());
}

fn step(verb: &'static str, op: InputOp, delay_ms: u64) -> SequenceStep {
    SequenceStep {
        verb,
        op,
        delay: Duration::from_millis(delay_ms),
    }
}

fn on(seat: SeatKind, op: InputOp) -> InputOp {
    InputOp::OnSeat {
        seat,
        op: Box::new(op),
    }
}

/// Drive a run with every step answered by `answer`.
fn drive(run: &mut SequenceRun, answer: impl Fn(&InputOp) -> ControlReply) -> Option<ControlReply> {
    loop {
        match run.next_action() {
            SequenceNext::Done => return None,
            SequenceNext::Wait(_) => run.delay_elapsed(),
            SequenceNext::Run { index, step } => {
                let seat = match &step.op {
                    InputOp::OnSeat { seat, .. } => seat.name(),
                    _ => "human",
                };
                let reply = match answer(&step.op) {
                    ControlReply::Body(mut body) => {
                        body["seat"] = json!(seat);
                        ControlReply::Body(body)
                    }
                    ControlReply::Refused { error, mut detail } => {
                        detail["seat"] = json!(seat);
                        ControlReply::Refused { error, detail }
                    }
                    other => other,
                };
                if let Some(failed) = run.record(index, step.verb, reply, 1) {
                    return Some(failed);
                }
            }
        }
    }
}

fn ok(_: &InputOp) -> ControlReply {
    ControlReply::Body(json!({"injected": 1}))
}

/// The run waits each step's delay once, then runs it.
#[test]
fn a_run_waits_each_delay_once_then_runs_the_step() {
    let mut run = SequenceRun::new(LongOp::Sequence(vec![
        step("comp.input.key", key(PressAction::Press), 0),
        step("comp.input.key", key(PressAction::Release), 80),
    ]))
    .unwrap();
    assert!(matches!(run.next_action(), SequenceNext::Run { index: 0, .. }));
    assert_eq!(run.next_action(), SequenceNext::Wait(Duration::from_millis(80)));
    assert_eq!(run.next_action(), SequenceNext::Wait(Duration::from_millis(80)), "until it elapses");
    run.delay_elapsed();
    assert!(matches!(run.next_action(), SequenceNext::Run { index: 1, .. }));
    assert_eq!(run.next_action(), SequenceNext::Done);
    assert!(SequenceRun::new(LongOp::RegionSelect { output: None, timeout: Duration::from_secs(1), selection: None }).is_none());
}

/// An unseated sequence reports human or mixed, on success and on failure.
#[test]
fn an_unseated_run_reports_human_or_mixed() {
    for mixed in [false, true] {
        for fail in [false, true] {
            let mut steps = vec![step("comp.input.key", key(PressAction::Both), 0)];
            if mixed {
                steps.push(step("comp.input.key", on(SeatKind::Agent, key(PressAction::Both)), 0));
            }
            if fail {
                steps.push(step("comp.input.key", InputOp::Key {
                    key: KeySpec::Name("not_a_real_keysym".into()),
                    action: PressAction::Both,
                    modifiers: Vec::new(),
                }, 0));
            }
            let mut run = SequenceRun::new(LongOp::Sequence(steps)).unwrap();
            assert_eq!(run.uses_agent(), mixed);
            let failed = drive(&mut run, |op| match op {
                InputOp::Key { key: KeySpec::Name(_), .. } => {
                    ControlReply::refused("unknown_key", json!({"key": "not_a_real_keysym"}))
                }
                op => ok(op),
            });
            let body = failed.map_or_else(|| run.clone().finish(5).wire_json(), ControlReply::wire_json);
            assert_eq!(body.get("error").is_some(), fail, "{body}");
            assert_eq!(body["seat"], if mixed { "mixed" } else { "human" });
            if fail {
                assert_eq!(body["error"], "step_failed");
                assert_eq!(body["released"], true);
                assert_eq!(body["step"]["error"], "unknown_key");
                assert_eq!(body["completed"].as_array().unwrap().len(), if mixed { 2 } else { 1 });
            } else {
                assert_eq!(body["elapsed_ms"], 5);
            }
        }
    }
}

/// An unseated agent sequence counts a refusing human step.
#[test]
fn an_agent_run_counts_a_refusing_human_step_as_mixed() {
    let mut run = SequenceRun::new(LongOp::Sequence(vec![
        step("comp.input.key", on(SeatKind::Agent, key(PressAction::Both)), 0),
        step("comp.input.key", key(PressAction::Both), 0),
    ]))
    .unwrap();
    let failed = drive(&mut run, |op| match op {
        InputOp::OnSeat { .. } => ok(op),
        _ => ControlReply::refused("target_unfocusable", json!({"reason": "minimized"})),
    })
    .expect("the human step fails");
    let body = failed.wire_json();
    assert_eq!(body["seat"], "mixed");
    assert_eq!(body["index"], 1);
    assert_eq!(body["verb"], "comp.input.key");
    assert_eq!(body["completed"][0]["seat"], "agent");
    assert_eq!(body["step"]["seat"], "human");
}

/// Mixed-seat sequence replies name the default seat and each driven seat.
#[test]
fn a_seated_run_names_its_seat_and_each_step_its_own() {
    let op = comp_model::request::parse_sequence(&json!({"seat": "agent", "steps": [
        {"verb": "comp.input.key", "args": {"key": "b", "seat": "human"}},
        {"verb": "comp.input.key", "args": {"key": "a"}},
    ]}))
    .unwrap();
    let mut run = SequenceRun::new(op).unwrap();
    assert!(drive(&mut run, ok).is_none());
    let body = run.finish(0).wire_json();
    assert_eq!(body["seat"], "agent");
    assert_eq!(body["steps"][0]["seat"], "human");
    assert_eq!(body["steps"][1]["seat"], "agent");
}

/// input_injection.rs `cancel_agent_sequences`: human input clears an agent
/// run with what it completed; coalesced replies are repeated and marked.
#[test]
fn a_cleared_run_reports_what_it_completed() {
    let mut run = SequenceRun::new(LongOp::Sequence(vec![
        step("comp.input.pointer.move", on(SeatKind::Agent, InputOp::ReleaseAll), 0),
        step("comp.input.key", on(SeatKind::Agent, key(PressAction::Both)), 50),
    ]))
    .unwrap();
    let SequenceNext::Run { index, step } = run.next_action() else {
        panic!("first step runs");
    };
    assert!(run.record(index, step.verb, ok(&step.op), 2).is_none());
    assert_eq!(run.next_action(), SequenceNext::Wait(Duration::from_millis(50)));
    let body = run.cleared().wire_json();
    assert_eq!(body["error"], "input_cleared");
    assert_eq!(body["seat"], "agent");
    assert_eq!(body["released"], true);
    assert_eq!(body["completed"], json!([{"injected": 1, "coalesced": 2}, {"injected": 1, "coalesced": 2}]));
}

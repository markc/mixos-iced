// Tests. Each pins one rule engine-free; assertions are on the refusal
// bodies and on the effect list where the scene matters. The pixel-grid,
// configure round-trip and Wayland-traffic tests stay with the engine.

use super::*;
use comp_model::snapshot::RectSnapshot;

const OUT: &str = "o_dp_1";

fn default_output() -> DefaultOutput {
    DefaultOutput {
        key: OUT.into(),
        name: "DP-1".into(),
    }
}

fn outputs() -> BTreeMap<String, OutputSnapshot> {
    let row = |name: &str, x: i32| OutputSnapshot {
        name: name.into(),
        default: x == 0,
        x,
        y: 0,
        width: 1920,
        height: 1080,
        scale: 1.0,
        refresh_mhz: 60_000,
        usable: RectSnapshot {
            x: x as f32,
            y: 0.0,
            width: 1920.0,
            height: 1080.0,
        },
        presentation: None,
    };
    BTreeMap::from([
        (OUT.to_string(), row("DP-1", 0)),
        ("o_hdmi_a_1".to_string(), row("HDMI-A-1", 1920)),
    ])
}

/// Two mapped toplevels on workspace 1, beta focused.
fn two_windows() -> (Registry<u32>, (u64, u64), (u64, u64)) {
    let mut registry = Registry::new();
    let mut map = |handle: u32, title: &str| {
        let (id, generation) = registry.take_role(handle, SurfaceRole::Toplevel, None).unwrap();
        registry.set_mapped(id, true).unwrap();
        registry.set_workspace(id, 1).unwrap();
        registry.set_title(id, Some(title.into())).unwrap();
        registry.set_app_id(id, Some(format!("dev.mixos.{title}").into())).unwrap();
        (id.0, generation)
    };
    let alpha = map(1, "Alpha");
    let beta = map(2, "Beta");
    registry.set_focused(SurfaceId(beta.0), true).unwrap();
    (registry, alpha, beta)
}

fn facts() -> WindowFacts {
    WindowFacts {
        visible: true,
        input_presentable: true,
        window_origin: (100.0, 100.0),
        geometry_size: (640, 480),
        ..WindowFacts::default()
    }
}

fn body(reply: ControlReply) -> (u8, Value) {
    let (rc, body) = reply.into_wire();
    (rc, serde_json::from_str(&body).unwrap())
}

/// Every verb is fenced by `{id, generation}` with the exact body, and every one that
/// names a window (or switches) is refused under the lock.
#[test]
fn window_verbs_refuse_stale_targets_and_the_lock() {
    let (mut registry, (id, generation), _) = two_windows();
    let stale = generation + 5;
    let mut state = WorkspaceState::default();
    let output = default_output();
    let expected = (10, json!({"error": "stale_target", "id": id, "generation": stale, "current": generation}));
    let refusals = [
        set_minimized(&mut registry, id, stale, true).unwrap_err(),
        set_minimized(&mut registry, id, stale, false).unwrap_err(),
        focus(&registry, &mut state, Some(&output), SceneFacts::default(), &facts(), id, stale, true).unwrap_err(),
        raise(&registry, id, stale).unwrap_err(),
        close(&registry, id, stale).unwrap_err(),
        place(&registry, &PlaceSpec { id, generation: stale, output: None, x: Some(1.0), y: None, width: None, height: None },
            &facts(), &outputs(), Some(OUT), Some(OUT), |size| size, |origin| origin).unwrap_err(),
        send_to_workspace(&mut registry, &mut state, Some(&output), SwitchGates::default(), id, stale,
            WorkspaceIndex::Absolute(2), true).unwrap_err(),
        set_state(&registry, id, stale, WindowState::Maximized, true, None, &facts(), &outputs()).unwrap_err(),
        start_force_close(&registry, id, stale, false).unwrap_err().0,
    ];
    for refusal in refusals {
        assert_eq!(body(refusal), expected);
    }
    assert!(!registry.get(SurfaceId(id)).unwrap().minimized());
    assert_eq!(state.current(Some(&output)), 1);
    assert_eq!(registry.get(SurfaceId(id)).unwrap().workspace(), Some(1));

    for op in [
        WindowOp::Minimize { id, generation },
        WindowOp::Restore { target: None },
        WindowOp::Focus { id, generation, raise: true },
        WindowOp::Raise { id, generation },
        WindowOp::Close { id, generation },
        WindowOp::SwitchWorkspace { output: None, index: WorkspaceIndex::Next, wrap: true },
        WindowOp::Stats { target: StatsTarget::Window { id, generation }, samples: 1 },
        WindowOp::StatsReset { target: Some(StatsTarget::Window { id, generation }) },
    ] {
        assert_eq!(locked_refusal(&op, true).map(body), Some((10, json!({"error": "locked"}))), "{op:?}");
        assert_eq!(locked_refusal(&op, false), None);
    }
    for op in [
        WindowOp::Stats { target: StatsTarget::Source { id: "scene".into(), registration: None }, samples: 1 },
        WindowOp::StatsReset { target: None },
    ] {
        assert_eq!(locked_refusal(&op, true), None, "{op:?} names no window");
    }
    assert_eq!(
        start_force_close(&registry, id, generation, true).unwrap_err().0,
        ControlReply::Locked
    );
}

/// Focus, raise and close act on the named window.
#[test]
fn focus_raise_and_close_act_on_the_named_window() {
    let (mut registry, (alpha, alpha_generation), _) = two_windows();
    let mut state = WorkspaceState::default();
    let output = default_output();
    let a = SurfaceId(alpha);
    // Focus without raise: keyboard only.
    let decision = focus(&registry, &mut state, Some(&output), SceneFacts::default(), &facts(), alpha, alpha_generation, false).unwrap();
    assert_eq!(decision.reason, None);
    assert_eq!(decision.effects, [Effect::MarkDirty { id: a, cause: "comp.window" }, Effect::Focus(a)]);
    assert_eq!(
        body(focus_reply(alpha, alpha_generation, true, decision.reason)),
        (0, json!({"id": alpha, "generation": alpha_generation, "focused": true}))
    );
    // Raise without focus.
    assert_eq!(
        raise(&registry, alpha, alpha_generation).unwrap(),
        [Effect::MarkDirty { id: a, cause: "comp.window" }, Effect::Raise(a), Effect::RetargetPointer]
    );
    assert_eq!(body(raise_reply(alpha, alpha_generation, true)).1["raised"], true);
    // Focus with raise (the default) activates.
    let decision = focus(&registry, &mut state, Some(&output), SceneFacts::default(), &facts(), alpha, alpha_generation, true).unwrap();
    assert_eq!(decision.effects.last(), Some(&Effect::Activate(a)));
    // A minimised window cannot take focus, says why, and changes nothing.
    registry.set_minimized(a, true).unwrap();
    let decision = focus(&registry, &mut state, Some(&output), SceneFacts::default(), &facts(), alpha, alpha_generation, true).unwrap();
    assert_eq!(decision, FocusDecision { effects: Vec::new(), reason: Some("minimized") });
    let (rc, reply) = body(focus_reply(alpha, alpha_generation, false, decision.reason));
    assert_eq!((rc, reply["focused"].clone(), reply["reason"].clone()), (0, json!(false), json!("minimized")));
    registry.set_minimized(a, false).unwrap();
    // A focus that did not take says `refused`.
    assert_eq!(body(focus_reply(alpha, alpha_generation, false, None)).1["reason"], "refused");
    // Polite close is the close event, nothing more.
    let (reply, effects) = close(&registry, alpha, alpha_generation).unwrap();
    assert_eq!(body(reply).1["closed"], "polite");
    assert_eq!(effects, [Effect::ClosePolite(a)]);
}

/// Focus under an exclusive layer is refused with a reason.
#[test]
fn focus_under_an_exclusive_layer_is_refused_with_a_reason() {
    let (registry, (id, generation), _) = two_windows();
    let mut state = WorkspaceState::default();
    let scene = SceneFacts { exclusive_layer: true, ..SceneFacts::default() };
    let decision = focus(&registry, &mut state, Some(&default_output()), scene, &facts(), id, generation, true).unwrap();
    assert!(decision.effects.is_empty());
    assert_eq!(
        body(focus_reply(id, generation, false, decision.reason)),
        (0, json!({"id": id, "generation": generation, "focused": false, "reason": "exclusive_layer"}))
    );
}

/// Focus on an off-workspace window switches and focuses; focus on a
/// minimised off-workspace window is refused without switching.
#[test]
fn focus_on_an_off_workspace_window_switches_unless_a_gate_holds() {
    let (mut registry, (id, generation), _) = two_windows();
    let a = SurfaceId(id);
    registry.set_workspace(a, 3).unwrap();
    let output = default_output();
    let off_screen = WindowFacts { visible: false, ..facts() };
    let mut state = WorkspaceState::default();
    let decision = focus(&registry, &mut state, Some(&output), SceneFacts::default(), &off_screen, id, generation, true).unwrap();
    assert_eq!(decision.reason, None, "seen as on-current after the switch");
    assert_eq!(state.current(Some(&output)), 3);
    assert_eq!(decision.effects.first(), Some(&Effect::MarkDirty { id: a, cause: "comp.window" }));
    assert!(decision.effects.contains(&Effect::Settle { prefer: Some(a) }));
    assert_eq!(decision.effects.last(), Some(&Effect::Activate(a)));
    // Minimised on another workspace: refused, no switch.
    let mut state = WorkspaceState::default();
    registry.set_minimized(a, true).unwrap();
    let decision = focus(&registry, &mut state, Some(&output), SceneFacts::default(), &off_screen, id, generation, true).unwrap();
    assert_eq!(decision, FocusDecision { effects: Vec::new(), reason: Some("minimized") });
    assert_eq!(state.current(Some(&output)), 1);
    // No default output: the switch cannot run, so `not_visible`, and the
    // planted mark goes.
    registry.set_minimized(a, false).unwrap();
    let decision = focus(&registry, &mut state, None, SceneFacts::default(), &off_screen, id, generation, true).unwrap();
    assert_eq!(decision, FocusDecision { effects: Vec::new(), reason: Some("not_visible") });
    // Not presentable (the KMS input gate) wins over visibility.
    let gated = WindowFacts { input_presentable: false, ..off_screen };
    let decision = focus(&registry, &mut state, Some(&output), SceneFacts::default(), &gated, id, generation, true).unwrap();
    assert_eq!(decision.reason, Some("not_presentable"));
    assert_eq!(state.current(Some(&output)), 1);
}

/// Raise on an off-workspace or minimised window never switches or unminimises.
#[test]
fn raise_never_switches_or_unminimises() {
    let (mut registry, (id, generation), _) = two_windows();
    let a = SurfaceId(id);
    registry.set_workspace(a, 2).unwrap();
    registry.set_minimized(a, true).unwrap();
    let effects = raise(&registry, id, generation).unwrap();
    assert!(!effects.iter().any(|effect| matches!(effect, Effect::Settle { .. } | Effect::Restore(_))));
    assert!(registry.get(a).unwrap().minimized());
}

/// The `minimize`/`restore` replies.
#[test]
fn minimise_and_restore_report_changed_and_the_record() {
    let (mut registry, (id, generation), _) = two_windows();
    let (reply, effects) = set_minimized(&mut registry, id, generation, true).unwrap();
    assert_eq!(effects, [Effect::MarkDirty { id: SurfaceId(id), cause: "comp.window" }, Effect::Minimize(SurfaceId(id))]);
    assert_eq!(
        body(reply),
        (0, json!({"id": id, "generation": generation, "title": "Alpha", "app_id": "dev.mixos.Alpha", "minimized": true, "changed": true}))
    );
    let (reply, effects) = set_minimized(&mut registry, id, generation, true).unwrap();
    assert_eq!(body(reply).1["changed"], false);
    assert_eq!(effects.len(), 1, "a no-op only marks");
    assert_eq!(
        body(nothing_to_restore(&registry)),
        (10, json!({"error": "not_found", "minimized_count": 1}))
    );
    let (reply, _) = set_minimized(&mut registry, id, generation, false).unwrap();
    assert_eq!(body(reply).1["minimized"], false);
    assert_eq!(body(nothing_to_restore(&registry)).1["minimized_count"], 0);
}

/// State props fence at service and reject fixed-size windows; an
/// unsupported state does not install a fullscreen output selection.
#[test]
fn state_verbs_refuse_fixed_size_and_unknown_outputs_before_any_configure() {
    let (registry, (id, generation), _) = two_windows();
    let a = SurfaceId(id);
    let fixed = WindowFacts { min_size: (300, 200), max_size: (300, 200), ..facts() };
    assert_eq!(
        body(set_state(&registry, id, generation, WindowState::Maximized, true, None, &fixed, &outputs()).unwrap_err()),
        (10, json!({"error": "unsupported_state", "id": id, "reason": "fixed_size"}))
    );
    // Leaving a state is never refused for size.
    assert!(set_state(&registry, id, generation, WindowState::Maximized, false, None, &fixed, &outputs()).is_ok());
    assert_eq!(
        body(set_state(&registry, id, generation, WindowState::Fullscreen, true, Some("nope"), &facts(), &outputs()).unwrap_err()),
        (10, json!({"error": "unknown_output", "output": "nope"}))
    );
    assert_eq!(
        set_state(&registry, id, generation, WindowState::Fullscreen, true, Some("HDMI-A-1"), &facts(), &outputs()).unwrap(),
        [
            Effect::MarkDirty { id: a, cause: "comp.window" },
            Effect::SetFullscreenOutput { id: a, output: Some("o_hdmi_a_1".into()) },
            Effect::RequestWindowState { id: a, state: WindowState::Fullscreen, enabled: true },
        ]
    );
    // Unfullscreen clears the selection; maximise never touches it.
    assert!(set_state(&registry, id, generation, WindowState::Fullscreen, false, None, &facts(), &outputs())
        .unwrap()
        .contains(&Effect::SetFullscreenOutput { id: a, output: None }));
    assert_eq!(
        set_state(&registry, id, generation, WindowState::Maximized, true, None, &facts(), &outputs()).unwrap(),
        [
            Effect::MarkDirty { id: a, cause: "comp.window" },
            Effect::RequestWindowState { id: a, state: WindowState::Maximized, enabled: true },
        ]
    );
    let committed = WindowFacts { committed_maximized: true, configure_pending: true, ..facts() };
    let reply = body(state_reply(registry.get(a).unwrap(), true, &committed)).1;
    assert_eq!((reply["maximized"].clone(), reply["configure_pending"].clone()), (json!(true), json!(true)));
}

fn spec(id: u64, generation: u64) -> PlaceSpec {
    PlaceSpec { id, generation, output: None, x: None, y: None, width: None, height: None }
}

/// Place moves the window geometry origin output-locally, keeps the real
/// size, refuses off-output, sends a clamped configure on resize and
/// refuses maximized windows.
#[test]
fn place_moves_output_locally_keeps_the_real_size_and_refuses_off_output() {
    let (registry, (id, generation), _) = two_windows();
    let a = SurfaceId(id);
    let place_with = |spec: &PlaceSpec, facts: &WindowFacts| {
        place(&registry, spec, facts, &outputs(), Some(OUT), Some(OUT), |(w, h)| (w.min(1000), h.min(1000)), |origin| origin)
    };
    // To the second output: an absent y keeps the offset within the output.
    let placement = place_with(&PlaceSpec { output: Some("HDMI-A-1".into()), x: Some(10.0), ..spec(id, generation) }, &facts()).unwrap();
    assert_eq!(placement.output, "o_hdmi_a_1");
    assert_eq!(placement.effects, [Effect::MoveTo { id: a, x: 1930.0, y: 100.0 }, Effect::RetargetPointer]);
    assert_eq!(
        body(place_reply(&spec(id, generation), &placement, (1930.0, 100.0), false)),
        (0, json!({"id": id, "generation": generation, "output": "o_hdmi_a_1", "window_x": 10.0, "window_y": 100.0, "requested": null, "configure_pending": false}))
    );
    // A height alone keeps the real width; the size is clamped.
    let placement = place_with(&PlaceSpec { height: Some(5000), ..spec(id, generation) }, &facts()).unwrap();
    assert_eq!(placement.requested, Some((640, 1000)));
    assert_eq!(placement.effects[0], Effect::Resize { id: a, x: 100.0, y: 100.0, width: 640, height: 1000 });
    // A client move/resize in progress is ended first.
    let dragging = WindowFacts { interactive: true, ..facts() };
    assert_eq!(place_with(&spec(id, generation), &dragging).unwrap().effects[0], Effect::FinishInteractive(a));
    // Wholly off every output.
    assert_eq!(
        body(place_with(&PlaceSpec { x: Some(100_000.0), ..spec(id, generation) }, &facts()).unwrap_err()),
        (10, json!({"error": "off_output", "id": id, "x": 100_000.0, "y": 100.0, "width": 640, "height": 480}))
    );
    // Maximised or fullscreen (requested or committed) is refused.
    for facts in [
        WindowFacts { requested_maximized: true, ..facts() },
        WindowFacts { committed_fullscreen: true, ..facts() },
    ] {
        let (rc, refusal) = body(place_with(&PlaceSpec { x: Some(1.0), ..spec(id, generation) }, &facts).unwrap_err());
        assert_eq!((rc, refusal["error"].clone()), (10, json!("invalid_state")));
    }
    assert_eq!(
        body(place_with(&PlaceSpec { output: Some("o_nope".into()), ..spec(id, generation) }, &facts()).unwrap_err()),
        (10, json!({"error": "unknown_output", "output": "o_nope"}))
    );
    assert_eq!(
        body(place(&registry, &spec(id, generation), &facts(), &outputs(), None, None, |size| size, |origin| origin).unwrap_err()),
        (10, json!({"error": "unknown_output", "output": null}))
    );
    // The snapped origin is what is validated and placed.
    let placement = place(&registry, &PlaceSpec { x: Some(10.4), ..spec(id, generation) }, &facts(), &outputs(), Some(OUT), Some(OUT),
        |size| size, |(x, y)| (x.round(), y.round())).unwrap();
    assert_eq!(placement.effects[0], Effect::MoveTo { id: a, x: 10.0, y: 100.0 });
}

/// Force-close kills only a window still alive at the deadline, kills a
/// window that only unmapped, and refuses X11.
#[test]
fn force_close_kills_only_the_same_window_still_alive_at_the_deadline() {
    let (mut registry, (id, generation), _) = two_windows();
    let a = SurfaceId(id);
    assert_eq!(start_force_close(&registry, id, generation, false), Ok(vec![Effect::ClosePolite(a)]));
    assert_eq!(force_close_at_deadline(&registry, id, generation, false), ForceCloseOutcome::Kill { id: a, mapped: true });
    registry.set_mapped(a, false).unwrap();
    assert_eq!(force_close_at_deadline(&registry, id, generation, false), ForceCloseOutcome::Kill { id: a, mapped: false });
    assert_eq!(force_close_at_deadline(&registry, id, generation, true), ForceCloseOutcome::Locked);
    // A new role on the same surface is a different window.
    registry.take_role(1, SurfaceRole::Toplevel, None).unwrap();
    assert_eq!(force_close_at_deadline(&registry, id, generation, false), ForceCloseOutcome::Gone);
    registry.destroy(a);
    assert_eq!(force_close_at_deadline(&registry, id, generation, true), ForceCloseOutcome::Gone, "gone wins over the lock");
    let reply = body(kill_reply(id, generation, 40, false, Some(4242), vec![9, 3])).1;
    assert_eq!(
        reply,
        json!({"id": id, "generation": generation, "closed": "killed", "waited_ms": 40, "window": "unmapped", "scope": "client", "pid": 4242, "windows": [3, 9]})
    );
    assert_eq!(body(close_reply(id, generation, "gone", 5)).1["closed"], "gone");
    // X11: polite close sent, kill refused at once.
    let (x11, x11_generation) = registry.take_role(7, SurfaceRole::X11 { override_redirect: false }, None).unwrap();
    registry.set_mapped(x11, true).unwrap();
    let (refusal, effects) = start_force_close(&registry, x11.0, x11_generation, false).unwrap_err();
    assert_eq!(effects, [Effect::ClosePolite(x11)]);
    assert_eq!(
        body(refusal),
        (10, json!({"error": "still_open", "id": x11.0, "generation": x11_generation, "reason": "x11_kill_unsupported", "polite_close_sent": true}))
    );
}

fn wait_spec(window: WindowMatch, until: WaitUntil) -> WaitSpec {
    WaitSpec { window, until, timeout: std::time::Duration::from_secs(1) }
}

fn by_id(id: u64, generation: u64) -> WindowMatch {
    WindowMatch { id: Some(id), generation: Some(generation), ..WindowMatch::default() }
}

/// Wait respects the lock, refuses unissued ids, resolves now on an edge
/// or times out.
#[test]
fn waits_resolve_on_their_condition_respect_the_lock_and_refuse_unissued_ids() {
    let (mut registry, (alpha, alpha_generation), (beta, beta_generation)) = two_windows();
    assert_eq!(
        body(start_wait(&registry, &wait_spec(by_id(999_999, 1), WaitUntil::Gone)).unwrap_err()).1["error"],
        "unknown_window"
    );
    assert!(start_wait(&registry, &wait_spec(by_id(alpha, alpha_generation), WaitUntil::Gone)).is_ok());
    let all = |_: SurfaceId| facts();
    let outcome = |registry: &Registry<u32>, spec: &WaitSpec, lock: bool| wait_outcome(registry, spec, lock, 1, all);
    let by_app = WindowMatch { app_id: Some("dev.mixos.Beta".into()), ..WindowMatch::default() };
    assert_eq!(outcome(&registry, &wait_spec(by_app.clone(), WaitUntil::Mapped), false), Some(WaitResolution::Window(SurfaceId(beta))));
    assert_eq!(outcome(&registry, &wait_spec(by_app.clone(), WaitUntil::Mapped), true), None, "hidden while locked");
    assert_eq!(outcome(&registry, &wait_spec(by_id(beta, beta_generation), WaitUntil::Visible), true), None);
    assert_eq!(outcome(&registry, &wait_spec(by_id(beta, beta_generation), WaitUntil::Focused), false), Some(WaitResolution::Window(SurfaceId(beta))));
    assert_eq!(outcome(&registry, &wait_spec(by_id(alpha, alpha_generation), WaitUntil::Focused), false), None);
    let title = WindowMatch { title_contains: Some("lph".into()), ..WindowMatch::default() };
    assert_eq!(outcome(&registry, &wait_spec(title, WaitUntil::Size { width: 640, height: 480 }), false), Some(WaitResolution::Window(SurfaceId(alpha))));
    // Gone by id resolves under the lock; a stale generation reads as gone.
    assert_eq!(outcome(&registry, &wait_spec(by_id(alpha, alpha_generation), WaitUntil::Gone), true), None);
    registry.destroy(SurfaceId(alpha));
    assert_eq!(outcome(&registry, &wait_spec(by_id(alpha, alpha_generation), WaitUntil::Gone), true), Some(WaitResolution::Null));
    assert_eq!(outcome(&registry, &wait_spec(by_id(beta, beta_generation + 1), WaitUntil::Gone), false), Some(WaitResolution::Null));
    // Unmapped by filter: when no live match remains.
    assert_eq!(outcome(&registry, &wait_spec(by_app.clone(), WaitUntil::Unmapped), false), None);
    registry.set_mapped(SurfaceId(beta), false).unwrap();
    assert_eq!(outcome(&registry, &wait_spec(by_app, WaitUntil::Unmapped), false), Some(WaitResolution::Null));
    let reply = body(wait_reply(&wait_spec(by_id(beta, beta_generation), WaitUntil::Unmapped), Value::Null, 12)).1;
    assert_eq!(reply, json!({"window": null, "until": "unmapped", "waited_ms": 12}));
}

/// Wait-until-visible times out off workspace, and the presented rules:
/// presented needs a shown frame of this mapping, on workspace, not
/// minimised; committed states wait for their configure ack.
#[test]
fn presented_and_state_waits_read_the_engine_facts() {
    let (mut registry, (id, generation), _) = two_windows();
    let shown = |presented: bool, maximized: bool, pending: bool| {
        move |_: SurfaceId| WindowFacts {
            presented_since_map: presented,
            committed_maximized: maximized,
            configure_pending: pending,
            ..facts()
        }
    };
    let presented = wait_spec(by_id(id, generation), WaitUntil::Presented);
    assert_eq!(wait_outcome(&registry, &presented, false, 1, shown(false, false, false)), None);
    assert!(wait_outcome(&registry, &presented, false, 1, shown(true, false, false)).is_some());
    assert_eq!(wait_outcome(&registry, &presented, false, 2, shown(true, false, false)), None, "off workspace");
    let maximized = wait_spec(by_id(id, generation), WaitUntil::Maximized);
    assert_eq!(wait_outcome(&registry, &maximized, false, 1, shown(false, true, true)), None, "ack pending");
    assert!(wait_outcome(&registry, &maximized, false, 1, shown(false, true, false)).is_some());
    for (until, committed) in [(WaitUntil::Fullscreen, true), (WaitUntil::Unfullscreen, false)] {
        let spec = wait_spec(by_id(id, generation), until);
        let fullscreen = |pending| move |_: SurfaceId| WindowFacts {
            committed_fullscreen: committed,
            configure_pending: pending,
            ..facts()
        };
        assert_eq!(wait_outcome(&registry, &spec, false, 1, fullscreen(true)), None, "fullscreen transition pending");
        assert!(wait_outcome(&registry, &spec, false, 1, fullscreen(false)).is_some());
    }
    registry.set_minimized(SurfaceId(id), true).unwrap();
    assert_eq!(wait_outcome(&registry, &presented, false, 1, shown(true, false, false)), None, "minimised");
}

/// Send-to-workspace moves and follows; follow is inert under an exclusive
/// layer.
#[test]
fn send_to_workspace_moves_and_follows() {
    let (mut registry, (id, generation), _) = two_windows();
    let a = SurfaceId(id);
    let output = default_output();
    let mut state = WorkspaceState::default();
    let open = SwitchGates { input_presentable: true, ..SwitchGates::default() };
    let (reply, effects) = send_to_workspace(&mut registry, &mut state, Some(&output), open, id, generation, WorkspaceIndex::Absolute(3), false).unwrap();
    assert_eq!(body(reply), (0, json!({"id": id, "generation": generation, "index": 3})));
    assert_eq!(state.current(Some(&output)), 1, "no follow, no switch");
    assert!(!effects.contains(&Effect::Activate(a)));
    // Next is relative to the window's own workspace.
    let (reply, effects) = send_to_workspace(&mut registry, &mut state, Some(&output), open, id, generation, WorkspaceIndex::Next, true).unwrap();
    assert_eq!(body(reply), (0, json!({"id": id, "generation": generation, "index": 4, "followed": true})));
    assert_eq!(state.current(Some(&output)), 4);
    assert_eq!(effects.last(), Some(&Effect::Activate(a)));
    // Under an exclusive layer the follow is inert: moved, not followed.
    let layer = SwitchGates { exclusive_layer: true, ..open };
    let (reply, effects) = send_to_workspace(&mut registry, &mut state, Some(&output), layer, id, generation, WorkspaceIndex::Absolute(2), true).unwrap();
    assert_eq!(body(reply).1["followed"], false);
    assert_eq!(state.current(Some(&output)), 4);
    assert!(!effects.contains(&Effect::Activate(a)));
    // An index above the count is refused before anything changes.
    let (rc, refusal) = body(send_to_workspace(&mut registry, &mut state, Some(&output), open, id, generation, WorkspaceIndex::Absolute(9), true).unwrap_err());
    assert_eq!((rc, refusal["path"].clone(), refusal["range"].clone()), (10, json!("index"), json!("1..=4")));
    assert_eq!(registry.get(a).unwrap().workspace(), Some(2));
}

/// Every refusal gains its alias on the
/// wire (the transport applies `with_error_code`).
#[test]
fn refusals_carry_error_code() {
    let (registry, (id, generation), _) = two_windows();
    let reply = raise(&registry, id, generation + 1).unwrap_err().wire_json();
    assert_eq!((reply["error"].clone(), reply["error_code"].clone(), reply["current"].clone()), (json!("stale_target"), json!("stale_target"), json!(generation)));
    let reply = crate::workspaces::workspace_refusal(crate::workspaces::WorkspaceRefusal::AtEnd { from: 1, count: 4 }, Some(OUT), 0).wire_json();
    assert_eq!((reply["error_code"].clone(), reply["from"].clone(), reply["count"].clone()), (json!("at_end"), json!(1), json!(4)));
}

/// window_control.rs `source_target`: the content-source fence.
#[test]
fn source_stats_are_fenced_by_registration() {
    assert_eq!(source_target("scene", None, Some(3)), Ok(3));
    assert_eq!(source_target("scene", Some(3), Some(3)), Ok(3));
    assert_eq!(
        body(source_target("scene", Some(2), Some(3)).unwrap_err()),
        (10, json!({"error": "stale_target", "source": "scene", "registration": 2, "current": 3}))
    );
    assert_eq!(
        body(source_target("gone", None, None).unwrap_err()),
        (10, json!({"error": "unknown_source", "source": "gone"}))
    );
}

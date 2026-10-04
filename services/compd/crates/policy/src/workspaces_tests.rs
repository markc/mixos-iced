// Tests. Each pins one workspace rule over the registry; `on_workspace`
// stands in for the engine's `layout.visible` (the engine's recompute
// derives one from the other).

use super::*;

fn output() -> DefaultOutput {
    DefaultOutput {
        key: "o_dp_1".into(),
        name: "DP-1".into(),
    }
}

/// A mapped toplevel stamped with workspace `ws`.
fn window(registry: &mut Registry<u32>, handle: u32, ws: u32) -> SurfaceId {
    let (id, _) = registry.take_role(handle, SurfaceRole::Toplevel, None).unwrap();
    registry.set_mapped(id, true).unwrap();
    assert!(stamp_workspace_at_map(registry, id, false, ws));
    id
}

#[test]
fn workspace_switch_refusals() {
    let registry = Registry::<u32>::new();
    let output = output();
    let mut state = WorkspaceState::default();
    let out = Some(&output);
    assert_eq!(state.count, 4);
    assert_eq!(state.current(out), 1);
    let switch = |state: &mut WorkspaceState, key: Option<&str>, target, wrap| {
        state
            .switch(&registry, out, key, target, wrap, None)
            .map(|(switched, _)| switched)
    };
    assert_eq!(
        switch(&mut state, None, WorkspaceTarget::Index(0), true),
        Err(WorkspaceRefusal::InvalidIndex { count: 4 })
    );
    assert_eq!(
        switch(&mut state, None, WorkspaceTarget::Index(5), true),
        Err(WorkspaceRefusal::InvalidIndex { count: 4 })
    );
    assert_eq!(
        switch(&mut state, None, WorkspaceTarget::Prev, false),
        Err(WorkspaceRefusal::AtEnd { from: 1, count: 4 })
    );
    assert_eq!(state.current(out), 1);
    let wrapped = switch(&mut state, None, WorkspaceTarget::Prev, true).expect("prev wraps");
    assert_eq!((wrapped.from, wrapped.to), (1, 4));
    assert_eq!(state.current(out), 4);
    assert_eq!(
        switch(&mut state, None, WorkspaceTarget::Next, false),
        Err(WorkspaceRefusal::AtEnd { from: 4, count: 4 })
    );
    let wrapped = switch(&mut state, None, WorkspaceTarget::Next, true).expect("next wraps");
    assert_eq!((wrapped.from, wrapped.to), (4, 1));
    // Same workspace: Ok, nothing to do.
    let same = switch(&mut state, None, WorkspaceTarget::Index(1), true).expect("no-op switch");
    assert_eq!((same.from, same.to), (1, 1));
    assert_eq!(
        switch(&mut state, Some("o_no_such_output"), WorkspaceTarget::Index(2), true),
        Err(WorkspaceRefusal::UnknownOutput)
    );
    // The default output is addressable by its key and by its name.
    let by_key = switch(&mut state, Some("o_dp_1"), WorkspaceTarget::Index(2), true).expect("by key");
    assert_eq!((by_key.output.as_str(), by_key.to), ("o_dp_1", 2));
    let by_name = switch(&mut state, Some("DP-1"), WorkspaceTarget::Index(3), true).expect("by name");
    assert_eq!((by_name.output.as_str(), by_name.from, by_name.to), ("o_dp_1", 2, 3));
    assert_eq!(state.current(out), 3);
    // No output at all: nothing is switchable.
    assert_eq!(
        state.switch(&registry, None, None, WorkspaceTarget::Index(2), true, None).map(|_| ()),
        Err(WorkspaceRefusal::UnknownOutput)
    );
    // Counts outside 1..=16 are refused.
    let mut registry = Registry::<u32>::new();
    assert_eq!(
        state.set_count(&mut registry, 0).map(|_| ()),
        Err(WorkspaceRefusal::InvalidCount { max: 16 })
    );
    assert_eq!(
        state.set_count(&mut registry, 17).map(|_| ()),
        Err(WorkspaceRefusal::InvalidCount { max: 16 })
    );
    assert_eq!(state.count, 4);
}

#[test]
fn shrinking_count_strands_windows_and_clamps_current() {
    let mut registry = Registry::<u32>::new();
    let output = output();
    let out = Some(&output);
    let mut state = WorkspaceState::default();
    let first = window(&mut registry, 1, 1);
    state.switch(&registry, out, None, WorkspaceTarget::Index(4), true, None).unwrap();
    let fourth = window(&mut registry, 2, state.current(out));
    assert_eq!(registry.get(fourth).unwrap().workspace(), Some(4));
    assert!(!on_workspace(registry.get(first).unwrap(), state.current(out)));

    let ((old, new), effects) = state.set_count(&mut registry, 2).unwrap();
    assert_eq!((old, new), (4, 2));
    assert_eq!(state.count, 2);
    assert_eq!(state.current(out), 2);
    assert_eq!(registry.get(fourth).unwrap().workspace(), Some(2));
    assert!(on_workspace(registry.get(fourth).unwrap(), 2));
    assert_eq!(registry.get(first).unwrap().workspace(), Some(1));
    assert!(!on_workspace(registry.get(first).unwrap(), 2));
    assert_eq!(
        effects,
        [
            Effect::WorkspacesDirty("workspace.count"),
            Effect::Relabelled { id: fourth, to: 2, cause: "workspace.count" },
            Effect::ResyncAllX11,
            Effect::PublishDesktops,
            Effect::Settle { prefer: None },
        ]
    );
    // Growing again changes nothing about placement.
    let ((old, new), effects) = state.set_count(&mut registry, 4).unwrap();
    assert_eq!((old, new), (2, 4));
    assert_eq!(effects, [Effect::WorkspacesDirty("workspace.count"), Effect::PublishDesktops]);
    assert_eq!(registry.get(fourth).unwrap().workspace(), Some(2));
    assert_eq!(state.current(out), 2);

    // A move is relative to the window, wraps, and never bumps generation.
    let generation = registry.get(fourth).unwrap().generation();
    let mut moved = |target| {
        state
            .move_window(&mut registry, out, fourth, target, None)
            .map(|(moved, _)| moved)
    };
    assert_eq!(moved(WorkspaceTarget::Prev), Ok((2, 1)));
    assert_eq!(moved(WorkspaceTarget::Prev), Ok((1, 4)));
    assert_eq!(moved(WorkspaceTarget::Index(2)), Ok((4, 2)));
    let record = registry.get(fourth).unwrap();
    assert!(on_workspace(record, 2));
    assert!(!record.minimized());
    assert_eq!(record.generation(), generation);
}

/// New windows join the current workspace at map: only the
/// mapped edge stamps, only carriers are stamped, a remap restamps.
#[test]
fn records_join_the_current_workspace_at_their_map_edge() {
    let mut registry = Registry::<u32>::new();
    let (id, _) = registry.take_role(1, SurfaceRole::Toplevel, None).unwrap();
    assert!(!stamp_workspace_at_map(&mut registry, id, false, 3), "not mapped yet");
    registry.set_mapped(id, true).unwrap();
    assert!(!stamp_workspace_at_map(&mut registry, id, true, 3), "not an edge");
    assert!(stamp_workspace_at_map(&mut registry, id, false, 3));
    assert_eq!(registry.get(id).unwrap().workspace(), Some(3));
    registry.set_mapped(id, false).unwrap();
    registry.set_mapped(id, true).unwrap();
    assert!(stamp_workspace_at_map(&mut registry, id, false, 2), "a remap rejoins current");
    assert_eq!(registry.get(id).unwrap().workspace(), Some(2));
    for (handle, role, stamps) in [
        (2, SurfaceRole::Layer, false),
        (3, SurfaceRole::Popup, false),
        (4, SurfaceRole::X11 { override_redirect: true }, true),
        (5, SurfaceRole::X11 { override_redirect: false }, true),
    ] {
        let (id, _) = registry.take_role(handle, role, None).unwrap();
        registry.set_mapped(id, true).unwrap();
        assert_eq!(stamp_workspace_at_map(&mut registry, id, false, 1), stamps, "{role:?}");
    }
}

/// The suppression term: a carrier off the current workspace is
/// suppressed (no visibility, callbacks or presentation); bands, layers
/// and popups never are; an unstamped carrier is on no workspace.
#[test]
fn suppression_is_the_on_workspace_term() {
    let mut registry = Registry::<u32>::new();
    let a = window(&mut registry, 1, 1);
    let b = window(&mut registry, 2, 2);
    let (layer, _) = registry.take_role(3, SurfaceRole::Layer, None).unwrap();
    registry.set_mapped(layer, true).unwrap();
    let (unstamped, _) = registry.take_role(4, SurfaceRole::Toplevel, None).unwrap();
    let (menu, _) = registry
        .take_role(5, SurfaceRole::X11 { override_redirect: true }, None)
        .unwrap();
    registry.set_mapped(menu, true).unwrap();
    stamp_workspace_at_map(&mut registry, menu, false, 2);
    let at = |id| registry.get(id).unwrap();
    assert!(!suppressed(at(a), 1) && suppressed(at(b), 1));
    assert!(!suppressed(at(layer), 1) && !suppressed(at(layer), 2));
    assert!(suppressed(at(unstamped), 1));
    assert!(suppressed(at(menu), 1) && !suppressed(at(menu), 2), "OR carries its workspace");
    assert!(presentable(at(a), 1) && !presentable(at(b), 1));
    registry.set_minimized(a, true).unwrap();
    assert!(!presentable(registry.get(a).unwrap(), 1), "minimised is never presented");
    assert!(x11_suspended(registry.get(a).unwrap(), 1));
    assert!(!workspace_movable(registry.get(menu).unwrap()), "OR is never movable");
    assert!(!workspace_movable(registry.get(unstamped).unwrap()));
}

#[test]
fn a_switch_withdraws_leavers_presents_arrivals_and_settles_once() {
    let mut registry = Registry::<u32>::new();
    let output = output();
    let out = Some(&output);
    let mut state = WorkspaceState::default();
    let a = window(&mut registry, 1, 1);
    let b = window(&mut registry, 2, 2);
    let _c = window(&mut registry, 3, 3);
    let (_, effects) = state.switch(&registry, out, None, WorkspaceTarget::Index(2), true, None).unwrap();
    assert_eq!(
        effects,
        [
            Effect::Withdraw { id: a, cause: "workspace.switch" },
            Effect::Present { id: b, cause: "workspace.switch" },
            Effect::WorkspacesDirty("workspace.switch"),
            Effect::PublishDesktops,
            Effect::Settle { prefer: None },
        ]
    );
    // A minimised window is still withdrawn and stays minimised.
    registry.set_minimized(b, true).unwrap();
    let (_, effects) = state.switch(&registry, out, None, WorkspaceTarget::Index(1), true, None).unwrap();
    assert!(effects.contains(&Effect::Withdraw { id: b, cause: "workspace.switch" }));
    assert!(registry.get(b).unwrap().minimized());
}

#[test]
fn a_move_withdraws_or_presents_only_across_the_current_workspace() {
    let mut registry = Registry::<u32>::new();
    let output = output();
    let out = Some(&output);
    let mut state = WorkspaceState::default();
    let a = window(&mut registry, 1, 1);
    let ((from, to), effects) = state
        .move_window(&mut registry, out, a, WorkspaceTarget::Index(3), None)
        .unwrap();
    assert_eq!((from, to), (1, 3));
    assert_eq!(
        effects,
        [
            Effect::Relabelled { id: a, to: 3, cause: "workspace.move" },
            Effect::Withdraw { id: a, cause: "workspace.move" },
            Effect::WorkspacesDirty("workspace.move"),
            Effect::Settle { prefer: None },
        ]
    );
    // Between two off-screen workspaces: no settle.
    let (_, effects) = state
        .move_window(&mut registry, out, a, WorkspaceTarget::Index(2), None)
        .unwrap();
    assert_eq!(
        effects,
        [
            Effect::Relabelled { id: a, to: 2, cause: "workspace.move" },
            Effect::WorkspacesDirty("workspace.move"),
        ]
    );
    let (_, effects) = state
        .move_window(&mut registry, out, a, WorkspaceTarget::Index(1), Some(a))
        .unwrap();
    assert!(effects.contains(&Effect::Present { id: a, cause: "workspace.move" }));
    assert_eq!(effects.last(), Some(&Effect::Settle { prefer: Some(a) }));
    // The same workspace changes nothing; a non-window is refused.
    assert_eq!(
        state.move_window(&mut registry, out, a, WorkspaceTarget::Index(1), None),
        Ok(((1, 1), Vec::new()))
    );
    let (layer, _) = registry.take_role(9, SurfaceRole::Layer, None).unwrap();
    assert_eq!(
        state.move_window(&mut registry, out, layer, WorkspaceTarget::Next, None).map(|_| ()),
        Err(WorkspaceRefusal::NotAWindow)
    );
}

/// A move takes the owner's override-redirect descendants
/// along (menus, submenus), and a client-made transient-for cycle ends.
#[test]
fn a_move_relabels_override_redirect_children_and_survives_a_cycle() {
    let mut registry = Registry::<u32>::new();
    let output = output();
    let mut state = WorkspaceState::default();
    let (owner, _) = registry
        .take_role(1, SurfaceRole::X11 { override_redirect: false }, None)
        .unwrap();
    registry.set_mapped(owner, true).unwrap();
    stamp_workspace_at_map(&mut registry, owner, false, 1);
    let or = SurfaceRole::X11 { override_redirect: true };
    // The X handles: owner 1, menu 2 (transient for 1), submenu 3 (for 2).
    // No `parent`: an X11 record gets none.
    let (menu, _) = registry.take_role(2, or, None).unwrap();
    let (submenu, _) = registry.take_role(3, or, None).unwrap();
    let (stranger, _) = registry.take_role(4, or, None).unwrap();
    // A managed dialog transient for the owner is a window of its own and
    // does not follow (the walk takes override-redirect children only).
    let (dialog, _) = registry
        .take_role(5, SurfaceRole::X11 { override_redirect: false }, None)
        .unwrap();
    assert!(registry.set_transient_for(menu, Some(1)).unwrap());
    assert!(registry.set_transient_for(submenu, Some(2)).unwrap());
    assert!(registry.set_transient_for(dialog, Some(1)).unwrap());
    assert!(!registry.set_transient_for(dialog, Some(1)).unwrap(), "unchanged");
    for id in [menu, submenu, stranger, dialog] {
        registry.set_mapped(id, true).unwrap();
        stamp_workspace_at_map(&mut registry, id, false, 1);
    }
    // A malformed client makes the owner name its own submenu as owner:
    // the walk still ends.
    registry.set_transient_for(owner, Some(3)).unwrap();
    let (_, effects) = state
        .move_window(&mut registry, Some(&output), owner, WorkspaceTarget::Index(2), None)
        .unwrap();
    for id in [owner, menu, submenu] {
        assert_eq!(registry.get(id).unwrap().workspace(), Some(2));
        assert!(effects.contains(&Effect::Relabelled { id, to: 2, cause: "workspace.move" }));
    }
    assert_eq!(registry.get(stranger).unwrap().workspace(), Some(1), "no transient-for, stays");
    assert_eq!(registry.get(dialog).unwrap().workspace(), Some(1), "a managed transient stays");
    // A role take clears the relation.
    registry.take_role(2, or, None).unwrap();
    assert_eq!(registry.get(menu).unwrap().transient_for(), None);
}

#[test]
fn move_and_follow_relabels_then_switches_in_one_settle() {
    let mut registry = Registry::<u32>::new();
    let output = output();
    let out = Some(&output);
    let mut state = WorkspaceState::default();
    let a = window(&mut registry, 1, 1);
    let b = window(&mut registry, 2, 1);
    let ((from, to), effects) = state
        .move_and_follow(&mut registry, out, a, WorkspaceTarget::Index(3), true)
        .unwrap();
    assert_eq!((from, to), (1, 3));
    assert_eq!(state.current(out), 3);
    assert_eq!(
        effects,
        [
            Effect::Raise(a),
            Effect::Relabelled { id: a, to: 3, cause: "workspace.move" },
            // The window already reads as on 3: the switch withdraws only
            // the bystander and presents the follower.
            Effect::Withdraw { id: b, cause: "workspace.switch" },
            Effect::Present { id: a, cause: "workspace.switch" },
            Effect::WorkspacesDirty("workspace.switch"),
            Effect::PublishDesktops,
            Effect::Settle { prefer: Some(a) },
        ]
    );
    // Not allowed (minimised, locked, ...): moved and raised, not preferred.
    let (_, effects) = state
        .move_and_follow(&mut registry, out, a, WorkspaceTarget::Index(2), false)
        .unwrap();
    assert_eq!(effects.last(), Some(&Effect::Settle { prefer: None }));
    // No default output: the plain move.
    let (_, effects) = state
        .move_and_follow(&mut registry, None, a, WorkspaceTarget::Index(4), true)
        .unwrap();
    assert!(!effects.contains(&Effect::Raise(a)));
}

/// Ensure-workspace-shown is inert under a session lock and an exclusive
/// layer, on the gate: no switch for a lock, an exclusive
/// layer, a minimised or a non-presentable window.
#[test]
fn ensure_shown_is_inert_unless_the_gate_admits_the_window() {
    let mut registry = Registry::<u32>::new();
    let output = output();
    let out = Some(&output);
    let mut state = WorkspaceState::default();
    let a = window(&mut registry, 1, 3);
    let open = SwitchGates {
        input_presentable: true,
        ..SwitchGates::default()
    };
    for gates in [
        SwitchGates { session_lock: true, ..open },
        SwitchGates { exclusive_layer: true, ..open },
        SwitchGates { input_presentable: false, ..open },
    ] {
        let allowed = switch_allowed_for(registry.get(a).unwrap(), gates);
        assert_eq!(allowed, None, "{gates:?}");
        assert_eq!(state.ensure_shown(&registry, out, a, allowed), None);
        assert_eq!(state.current(out), 1);
    }
    registry.set_minimized(a, true).unwrap();
    assert_eq!(switch_allowed_for(registry.get(a).unwrap(), open), None);
    registry.set_minimized(a, false).unwrap();
    let allowed = switch_allowed_for(registry.get(a).unwrap(), open);
    assert_eq!(allowed, Some(3));
    let effects = state.ensure_shown(&registry, out, a, allowed).expect("switches");
    assert_eq!(effects.last(), Some(&Effect::Settle { prefer: Some(a) }));
    assert_eq!(state.current(out), 3);
    assert_eq!(state.ensure_shown(&registry, out, a, allowed), None, "already shown");
    assert!(!state.window_off_current(out, registry.get(a).unwrap()));
}

/// A replaced default output keeps its workspace, and a changed one settles.
#[test]
fn a_replaced_default_output_keeps_its_workspace() {
    let mut state = WorkspaceState::default();
    let old = output();
    state.current.insert(old.key.clone(), 3);
    let new = DefaultOutput {
        key: "o_hdmi_a_1".into(),
        name: "HDMI-A-1".into(),
    };
    let effects = state.reconcile_after_topology_change(Some(&new), Some(&old.key), 3);
    assert_eq!(state.current(Some(&new)), 3, "carried to the replacement");
    assert_eq!(effects, [Effect::PublishDesktops]);
    // The only output went away: current reads 1, so re-derive and settle.
    let effects = state.reconcile_after_topology_change(None, Some(&new.key), 3);
    assert_eq!(effects, [Effect::PublishDesktops, Effect::ResyncAllX11, Effect::Settle { prefer: None }]);
}

/// The workspace switch verb wraps and refuses at the end, on the reply
/// bodies.
#[test]
fn workspace_switch_verb_wraps_and_refuses_at_end() {
    let registry = Registry::<u32>::new();
    let output = output();
    let out = Some(&output);
    let mut state = WorkspaceState::default();
    let reply = |state: &mut WorkspaceState, key: Option<&str>, index, wrap| {
        let (reply, _) = service_switch(state, &registry, out, key, index, wrap);
        reply.into_wire()
    };
    let body = |(rc, body): (u8, std::sync::Arc<str>)| {
        (rc, serde_json::from_str::<serde_json::Value>(&body).unwrap())
    };
    for (from, to) in [(1, 2), (2, 3), (3, 4), (4, 1)] {
        assert_eq!(
            body(reply(&mut state, None, WorkspaceIndex::Next, true)),
            (0, json!({"output": "o_dp_1", "from": from, "to": to}))
        );
    }
    assert_eq!(
        body(reply(&mut state, None, WorkspaceIndex::Prev, true)),
        (0, json!({"output": "o_dp_1", "from": 1, "to": 4}))
    );
    assert_eq!(
        body(reply(&mut state, None, WorkspaceIndex::Next, false)),
        (10, json!({"error": "at_end", "output": "o_dp_1", "from": 4, "count": 4}))
    );
    assert_eq!(
        body(reply(&mut state, Some("DP-1"), WorkspaceIndex::Next, false)),
        (10, json!({"error": "at_end", "output": "o_dp_1", "from": 4, "count": 4})),
        "addressed by name, refused by key"
    );
    for (index, key, path) in [
        (WorkspaceIndex::Absolute(5), None, "index"),
        (WorkspaceIndex::Absolute(0), None, "index"),
        (WorkspaceIndex::Absolute(1), Some("o_nope"), "output"),
    ] {
        let (rc, refusal) = body(reply(&mut state, key, index, true));
        assert_eq!(rc, 10);
        assert_eq!(refusal["error"], "invalid_value");
        assert_eq!(refusal["path"], path);
        assert_eq!(state.current(out), 4);
    }
    assert_eq!(body(reply(&mut state, None, WorkspaceIndex::Absolute(5), true)).1["range"], "1..=4");
    assert_eq!(workspace_index_range(16), "1..=16");
    // The table: a count of 0 saturates to the first entry; only a
    // count past the 16-entry table falls back to the symbolic range.
    assert_eq!(workspace_index_range(0), "1..=1");
    assert_eq!(workspace_index_range(17), "1..=workspaces.count");
}

// Registry tests, including every resolve_window_target refusal.

use std::sync::Arc;

use uuid::Uuid;

use crate::{
    BindOutcome, Registry, RegistryError, StackBand, SurfaceId, SurfaceRole, WindowTargetError,
};

/// Test handles are plain integers standing in for `WlSurface`.
fn registry() -> Registry<u32> {
    Registry::new()
}

fn uuid(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

/// A mapped xdg toplevel on workspace 1.
fn mapped_toplevel(registry: &mut Registry<u32>, handle: u32) -> (SurfaceId, u64) {
    let (id, generation) = registry
        .take_role(handle, SurfaceRole::Toplevel, None)
        .unwrap();
    registry.set_mapped(id, true).unwrap();
    registry.set_workspace(id, 1).unwrap();
    (id, generation)
}

#[test]
fn ids_start_at_one_and_are_never_reused() {
    let mut registry = registry();
    let (a, _) = registry.take_role(10, SurfaceRole::Toplevel, None).unwrap();
    let (b, _) = registry.take_role(11, SurfaceRole::Layer, None).unwrap();
    assert_eq!((a, b), (SurfaceId(1), SurfaceId(2)));
    assert!(registry.destroy(a).is_some());
    assert!(registry.destroy(a).is_none(), "destroyed once");
    // The same engine handle reappearing is a new wl_surface: a new id.
    let (c, _) = registry.take_role(10, SurfaceRole::Toplevel, None).unwrap();
    assert_eq!(c, SurfaceId(3));
    let (d, _) = registry.take_role(12, SurfaceRole::Popup, Some(c)).unwrap();
    assert_eq!(d, SurfaceId(4));
    assert!(registry.get(a).is_none());
    assert_eq!(registry.id_for_handle(&10), Some(c));
}

#[test]
fn generation_bumps_on_every_role_take_and_is_global() {
    let mut registry = registry();
    let (a, g1) = registry.take_role(1, SurfaceRole::Toplevel, None).unwrap();
    let (_, g2) = registry.take_role(2, SurfaceRole::Toplevel, None).unwrap();
    assert_eq!((g1, g2), (1, 2), "one counter across surfaces");
    // Losing the role keeps the id and mints a dormant generation.
    assert_eq!(registry.go_dormant(a), Ok(Some(3)));
    assert_eq!(registry.go_dormant(a), Ok(None), "already dormant: no bump");
    assert_eq!(registry.get(a).unwrap().generation(), 3);
    // Re-taking a role keeps the id, never the generation.
    let (again, g4) = registry.take_role(1, SurfaceRole::Subsurface, Some(SurfaceId(2))).unwrap();
    assert_eq!((again, g4), (a, 4));
    assert_eq!(registry.get(a).unwrap().role(), SurfaceRole::Subsurface);
}

#[test]
fn map_and_unmap_never_bump_but_a_remap_by_role_retake_does() {
    let mut registry = registry();
    let (id, generation) = mapped_toplevel(&mut registry, 1);
    // A null-buffer unmap and the following map keep the role: same window.
    assert_eq!(registry.set_mapped(id, false), Ok(true));
    assert_eq!(registry.set_mapped(id, true), Ok(true));
    assert_eq!(registry.set_mapped(id, true), Ok(false), "no change");
    assert_eq!(registry.get(id).unwrap().generation(), generation);
    // An X11 unmap->remap re-associates: a role take, so a new generation.
    let (x11, x11_generation) = registry
        .take_role(2, SurfaceRole::X11 { override_redirect: false }, None)
        .unwrap();
    registry.set_mapped(x11, true).unwrap();
    let (remapped, remap_generation) = registry
        .take_role(2, SurfaceRole::X11 { override_redirect: false }, None)
        .unwrap();
    assert_eq!(remapped, x11, "the id survives the remap");
    assert!(remap_generation > x11_generation);
    let record = registry.get(x11).unwrap();
    assert!(!record.mapped(), "a role take starts unmapped");
}

#[test]
fn role_retake_resets_window_state_but_keeps_the_workspace_stamp() {
    let mut registry = registry();
    let (id, _) = mapped_toplevel(&mut registry, 1);
    registry.set_title(id, Some(Arc::from("Terminal"))).unwrap();
    registry.set_app_id(id, Some(Arc::from("org.example.Terminal"))).unwrap();
    registry.set_pid(id, Some(42)).unwrap();
    registry.set_focused(id, true).unwrap();
    registry.set_minimized(id, true).unwrap();
    registry.set_band(id, StackBand::Bottom).unwrap();
    registry.set_workspace(id, 3).unwrap();
    registry.take_role(1, SurfaceRole::Toplevel, None).unwrap();
    let record = registry.get(id).unwrap();
    assert!(!record.mapped() && !record.focused() && !record.minimized());
    assert_eq!((record.title(), record.app_id(), record.pid()), (None, None, None));
    assert_eq!(record.band(), StackBand::Normal);
    assert_eq!(record.workspace(), Some(3), "the next map restamps it");
    assert_eq!(record.workspace_leaf(), None, "unmapped: on no workspace");
}

#[test]
fn dormant_is_not_a_role_and_unknown_surfaces_are_refused() {
    let mut registry = registry();
    assert_eq!(
        registry.take_role(1, SurfaceRole::Dormant, None),
        Err(RegistryError::DormantIsNotARole)
    );
    assert!(registry.is_empty());
    let ghost = SurfaceId(99);
    assert_eq!(registry.go_dormant(ghost), Err(RegistryError::UnknownSurface(ghost)));
    assert_eq!(registry.set_mapped(ghost, true), Err(RegistryError::UnknownSurface(ghost)));
    let (id, _) = mapped_toplevel(&mut registry, 1);
    assert_eq!(registry.set_workspace(id, 0), Err(RegistryError::InvalidWorkspace(0)));
}

#[test]
fn window_targets_refuse_in_comps_order() {
    let mut registry = registry();
    let (window, generation) = mapped_toplevel(&mut registry, 1);
    assert_eq!(
        registry.resolve_window_target(window.0, Some(generation)).map(|record| record.id()),
        Ok(window)
    );
    assert_eq!(
        registry.resolve_window_target(window.0, None).map(|record| record.id()),
        Ok(window),
        "the fence is optional for id-only callers"
    );
    assert_eq!(
        registry.resolve_window_target(999, Some(1)).map(|_| ()),
        Err(WindowTargetError::UnknownWindow)
    );
    assert_eq!(
        registry.resolve_window_target(window.0, Some(generation + 7)).map(|_| ()),
        Err(WindowTargetError::StaleTarget {
            requested: generation + 7,
            current: generation,
        })
    );
    let (layer, layer_generation) = registry.take_role(2, SurfaceRole::Layer, None).unwrap();
    registry.set_mapped(layer, true).unwrap();
    assert_eq!(
        registry.resolve_window_target(layer.0, Some(layer_generation)).map(|_| ()),
        Err(WindowTargetError::NotManaged)
    );
    let (or_window, or_generation) = registry
        .take_role(3, SurfaceRole::X11 { override_redirect: true }, None)
        .unwrap();
    registry.set_mapped(or_window, true).unwrap();
    assert_eq!(
        registry.resolve_window_target(or_window.0, Some(or_generation)).map(|_| ()),
        Err(WindowTargetError::NotManaged),
        "override-redirect is not a managed toplevel"
    );
    registry.set_mapped(window, false).unwrap();
    assert_eq!(
        registry.resolve_window_target(window.0, Some(generation)).map(|_| ()),
        Err(WindowTargetError::NotMapped)
    );
    // Dormant reads as unknown, but only after the generation fence: a
    // stale fence on a dormant surface is still stale_target.
    let dormant_generation = registry.go_dormant(window).unwrap().unwrap();
    assert_eq!(
        registry.resolve_window_target(window.0, Some(generation)).map(|_| ()),
        Err(WindowTargetError::StaleTarget {
            requested: generation,
            current: dormant_generation,
        })
    );
    assert_eq!(
        registry.resolve_window_target(window.0, Some(dormant_generation)).map(|_| ()),
        Err(WindowTargetError::UnknownWindow)
    );
    assert_eq!(
        registry.resolve_window_target(window.0, None).map(|_| ()),
        Err(WindowTargetError::UnknownWindow)
    );
    assert_eq!(WindowTargetError::NotMapped.code(), "not_mapped");
}

#[test]
fn a_stale_target_never_reaches_the_successor_role() {
    let mut registry = registry();
    let (id, old_generation) = mapped_toplevel(&mut registry, 1);
    // The client destroys its xdg_toplevel and makes a new one on the same
    // wl_surface: same id, new window.
    let (same_id, new_generation) = registry.take_role(1, SurfaceRole::Toplevel, None).unwrap();
    registry.set_mapped(same_id, true).unwrap();
    assert_eq!(same_id, id);
    assert_eq!(
        registry.resolve_window_target(id.0, Some(old_generation)).map(|_| ()),
        Err(WindowTargetError::StaleTarget {
            requested: old_generation,
            current: new_generation,
        })
    );
}

#[test]
fn uuids_bind_only_to_toplevels_and_x11_and_index_both_ways() {
    let mut registry = registry();
    let (window, generation) = mapped_toplevel(&mut registry, 1);
    assert_eq!(registry.bind_uuid(window, uuid(7)), Ok(BindOutcome::Bound));
    assert_eq!(registry.bind_uuid(window, uuid(7)), Ok(BindOutcome::Unchanged));
    assert_eq!(registry.get(window).unwrap().generation(), generation, "a first bind mints nothing");
    assert_eq!(registry.id_for_uuid(uuid(7)), Some(window));
    assert_eq!(registry.uuid_for(window), Some(uuid(7)));
    let (x11, _) = registry
        .take_role(2, SurfaceRole::X11 { override_redirect: false }, None)
        .unwrap();
    assert_eq!(registry.bind_uuid(x11, uuid(8)), Ok(BindOutcome::Bound));
    // Every other role gets an id and no uuid.
    for (handle, role) in [
        (3, SurfaceRole::Layer),
        (4, SurfaceRole::Popup),
        (5, SurfaceRole::Subsurface),
        (6, SurfaceRole::Lock),
        (7, SurfaceRole::DragIcon),
        (8, SurfaceRole::ImePopup),
    ] {
        let (id, _) = registry.take_role(handle, role, None).unwrap();
        assert_eq!(
            registry.bind_uuid(id, uuid(100 + u128::from(handle))),
            Err(RegistryError::RoleCarriesNoUuid { id, role })
        );
        assert_eq!(registry.uuid_for(id), None);
    }
    assert_eq!(
        registry.bind_uuid(x11, uuid(7)),
        Err(RegistryError::UuidInUse {
            uuid: uuid(7),
            owner: window,
        })
    );
    assert_eq!(registry.id_for_uuid(uuid(99)), None);
}

#[test]
fn a_uuid_replacement_mints_a_new_generation() {
    let mut registry = registry();
    let (window, generation) = mapped_toplevel(&mut registry, 1);
    registry.bind_uuid(window, uuid(1)).unwrap();
    // The engine recreated the Window without recreating the surface.
    let outcome = registry.bind_uuid(window, uuid(2)).unwrap();
    let BindOutcome::Replaced {
        previous,
        generation: replaced,
    } = outcome
    else {
        panic!("a different uuid is a replacement: {outcome:?}");
    };
    assert_eq!(previous, uuid(1));
    assert!(replaced > generation);
    assert_eq!(registry.get(window).unwrap().generation(), replaced);
    assert_eq!(registry.id_for_uuid(uuid(1)), None, "the old uuid is gone");
    assert_eq!(registry.id_for_uuid(uuid(2)), Some(window));
    assert_eq!(
        registry.resolve_uuid_target(uuid(2), Some(generation)).map(|_| ()),
        Err(WindowTargetError::StaleTarget {
            requested: generation,
            current: replaced,
        })
    );
    assert_eq!(
        registry.resolve_uuid_target(uuid(2), Some(replaced)).map(|record| record.id()),
        Ok(window)
    );
    assert_eq!(
        registry.resolve_uuid_target(uuid(1), None).map(|_| ()),
        Err(WindowTargetError::UnknownWindow)
    );
}

#[test]
fn role_loss_and_destroy_drop_the_uuid_and_retake_rebinds_without_a_second_bump() {
    let mut registry = registry();
    let (x11, _) = registry
        .take_role(1, SurfaceRole::X11 { override_redirect: false }, None)
        .unwrap();
    registry.bind_uuid(x11, uuid(5)).unwrap();
    // An X11 readmit keeps the uuid; the generation still bumps, once, at
    // the role take.
    let (_, readmitted) = registry
        .take_role(1, SurfaceRole::X11 { override_redirect: false }, None)
        .unwrap();
    assert_eq!(registry.id_for_uuid(uuid(5)), None, "a role take unbinds");
    assert_eq!(registry.bind_uuid(x11, uuid(5)), Ok(BindOutcome::Bound));
    assert_eq!(registry.get(x11).unwrap().generation(), readmitted);
    registry.go_dormant(x11).unwrap();
    assert_eq!(registry.id_for_uuid(uuid(5)), None, "dormant carries no uuid");
    let (window, _) = mapped_toplevel(&mut registry, 2);
    registry.bind_uuid(window, uuid(6)).unwrap();
    let record = registry.destroy(window).unwrap();
    // The returned record still names the window; only the index forgets it.
    assert_eq!(record.uuid(), Some(uuid(6)));
    assert_eq!(registry.id_for_uuid(uuid(6)), None);
    assert_eq!(registry.id_for_handle(&2), None);
}

#[test]
fn windows_projection_is_mapped_xdg_toplevels_in_id_order() {
    let mut registry = registry();
    let (b, _) = mapped_toplevel(&mut registry, 1);
    let (unmapped, _) = registry.take_role(2, SurfaceRole::Toplevel, None).unwrap();
    let (x11, _) = registry
        .take_role(3, SurfaceRole::X11 { override_redirect: false }, None)
        .unwrap();
    registry.set_mapped(x11, true).unwrap();
    let (layer, _) = registry.take_role(4, SurfaceRole::Layer, None).unwrap();
    registry.set_mapped(layer, true).unwrap();
    let (c, _) = mapped_toplevel(&mut registry, 5);
    let (dormant, _) = mapped_toplevel(&mut registry, 6);
    registry.go_dormant(dormant).unwrap();
    let windows = registry.windows().map(|record| record.id()).collect::<Vec<_>>();
    assert_eq!(windows, [b, c], "no X11, no layer, no unmapped, no dormant");
    let rows = registry.surface_rows().map(|record| record.id()).collect::<Vec<_>>();
    assert_eq!(rows, [b, unmapped, x11, layer, c], "dormant is not a row");
}

#[test]
fn focus_projection_is_the_lowest_focused_mapped_managed_toplevel() {
    let mut registry = registry();
    assert_eq!(registry.focus_window(), None);
    let (a, a_generation) = mapped_toplevel(&mut registry, 1);
    let (b, b_generation) = mapped_toplevel(&mut registry, 2);
    let (layer, _) = registry.take_role(3, SurfaceRole::Layer, None).unwrap();
    registry.set_mapped(layer, true).unwrap();
    registry.set_focused(layer, true).unwrap();
    assert_eq!(registry.focus_window(), None, "a layer is not a window");
    registry.set_focused(b, true).unwrap();
    assert_eq!(registry.focus_window(), Some((b.0, b_generation)));
    registry.set_focused(a, true).unwrap();
    assert_eq!(registry.focus_window(), Some((a.0, a_generation)));
    registry.set_mapped(a, false).unwrap();
    assert_eq!(registry.focus_window(), Some((b.0, b_generation)), "unmapped is not focus");
    let (x11, x11_generation) = registry
        .take_role(4, SurfaceRole::X11 { override_redirect: false }, None)
        .unwrap();
    registry.set_mapped(x11, true).unwrap();
    registry.set_focused(x11, true).unwrap();
    registry.set_focused(b, false).unwrap();
    assert_eq!(
        registry.focus_window(),
        Some((x11.0, x11_generation)),
        "a managed X11 window is a focus window"
    );
    registry.go_dormant(x11).unwrap();
    assert_eq!(registry.focus_window(), None, "losing the role drops focus");
}

#[test]
fn workspace_leaf_and_counts_follow_managed_mapped_rows() {
    let mut registry = registry();
    let (a, _) = mapped_toplevel(&mut registry, 1);
    let (b, _) = mapped_toplevel(&mut registry, 2);
    registry.set_workspace(b, 2).unwrap();
    let (x11, _) = registry
        .take_role(3, SurfaceRole::X11 { override_redirect: false }, None)
        .unwrap();
    registry.set_mapped(x11, true).unwrap();
    registry.set_workspace(x11, 2).unwrap();
    let (popup, _) = registry.take_role(4, SurfaceRole::Popup, Some(a)).unwrap();
    registry.set_mapped(popup, true).unwrap();
    registry.set_workspace(popup, 1).unwrap();
    let (unstamped, _) = registry.take_role(5, SurfaceRole::Toplevel, None).unwrap();
    registry.set_mapped(unstamped, true).unwrap();
    let (far, _) = mapped_toplevel(&mut registry, 6);
    registry.set_workspace(far, 9).unwrap();
    assert_eq!(registry.get(popup).unwrap().workspace_leaf(), None, "popups follow their toplevel");
    assert_eq!(registry.get(unstamped).unwrap().workspace_leaf(), None);
    assert_eq!(registry.get(x11).unwrap().workspace_leaf(), Some(2), "X11 is legible here (D11)");
    assert_eq!(registry.workspace_window_counts(3), [1, 2, 0], "out-of-range stamps are dropped");
    assert_eq!(registry.workspace_window_counts(0), Vec::<u32>::new());
}

#[test]
fn stats_window_walks_subsurfaces_to_their_root() {
    let mut registry = registry();
    let (root, root_generation) = mapped_toplevel(&mut registry, 1);
    let (child, _) = registry.take_role(2, SurfaceRole::Subsurface, Some(root)).unwrap();
    let (grandchild, _) = registry.take_role(3, SurfaceRole::Subsurface, Some(child)).unwrap();
    let (popup, _) = registry.take_role(4, SurfaceRole::Popup, Some(root)).unwrap();
    let (popup_child, _) = registry.take_role(5, SurfaceRole::Subsurface, Some(popup)).unwrap();
    let (orphan, _) = registry.take_role(6, SurfaceRole::Subsurface, None).unwrap();
    assert_eq!(registry.stats_window(root), Some((root.0, root_generation)));
    assert_eq!(registry.stats_window(grandchild), Some((root.0, root_generation)));
    assert_eq!(registry.stats_window(popup), None);
    assert_eq!(registry.stats_window(popup_child), None);
    assert_eq!(registry.stats_window(orphan), None);
    // A parent cycle terminates.
    registry.set_parent(child, Some(grandchild)).unwrap();
    assert_eq!(registry.stats_window(grandchild), None);
}

#[test]
fn role_kind_strings_and_band_names_are_the_wire_strings() {
    for (role, kind, managed, carries_workspace) in [
        (SurfaceRole::Toplevel, "toplevel", true, true),
        (SurfaceRole::Popup, "popup", false, false),
        (SurfaceRole::ImePopup, "ime-popup", false, false),
        (SurfaceRole::Layer, "layer", false, false),
        (SurfaceRole::Lock, "lock", false, false),
        (SurfaceRole::Subsurface, "subsurface", false, false),
        (SurfaceRole::DragIcon, "drag-icon", false, false),
        (SurfaceRole::Dormant, "dormant", false, false),
        (SurfaceRole::X11 { override_redirect: false }, "x11-toplevel", true, true),
        (SurfaceRole::X11 { override_redirect: true }, "x11-override-redirect", false, true),
    ] {
        assert_eq!(role.kind(), kind);
        assert_eq!(role.managed_toplevel(), managed, "{kind}");
        assert_eq!(role.carries_workspace(), carries_workspace, "{kind}");
    }
    let names = [
        StackBand::Background,
        StackBand::Bottom,
        StackBand::Normal,
        StackBand::Top,
        StackBand::Overlay,
        StackBand::DragIcon,
        StackBand::Lock,
    ]
    .map(StackBand::name);
    assert_eq!(names, ["background", "bottom", "normal", "top", "overlay", "drag-icon", "lock"]);
    assert_eq!(crate::SeatKind::Human.seat_name(), "seat0");
    assert_eq!(crate::SeatKind::Agent.seat_name(), "agent");
}

#[test]
fn issued_ids_include_destroyed_ones_but_never_future_ones() {
    let mut registry = registry();
    assert!(!registry.issued(0) && !registry.issued(1));
    let (id, _) = registry.take_role(1, SurfaceRole::Toplevel, None).unwrap();
    assert!(registry.issued(id.0));
    registry.destroy(id);
    assert!(registry.issued(id.0), "a destroyed id was handed out");
    assert!(!registry.issued(id.0 + 1));
}

/// A reserved id (a compositor-drawn scene surface) comes from the same
/// counter: issued, never handed to a handle, no record; generations too.
#[test]
fn reserved_ids_share_the_counter_and_have_no_record() {
    let mut registry = registry();
    let (first, g1) = mapped_toplevel(&mut registry, 1);
    let reserved = registry.reserve_id();
    let g2 = registry.reserve_generation();
    let (next, g3) = mapped_toplevel(&mut registry, 2);
    assert_eq!((first.0, reserved.0, next.0), (1, 2, 3));
    assert!(g1 < g2 && g2 < g3, "one generation counter");
    assert!(registry.issued(reserved.0));
    assert!(registry.get(reserved).is_none(), "no record behind a reserved id");
    assert_eq!(registry.id_for_handle(&2), Some(next));
}

// SPDX-License-Identifier: MIT OR Apache-2.0
//! Real protocol/registry lifetime and shared geometry executor guards.
//! These prove protocol geometry and ACK/commit fences, not rendered pixels.

use dispatcher::wire::trait_::surface_event::SurfaceHandle;
use policy::tiling::{Allocation, Facts, Group, LayoutError, Member, Plan, Rect, Target};
use protocols::window::ident::ident;
use testkit::Harness;
use wayland_client::protocol::wl_surface::WlSurface;
use world::camera::transform::translate::slot;
use world::window::interface::record::window::LoopWindow;

fn group() -> Group {
    Group {
        output: "testkit-0".into(),
        workspace: 1,
    }
}
fn area(width: i32) -> Rect {
    Rect {
        x: 0,
        y: 0,
        width,
        height: 1000,
    }
}
fn target(h: &Harness, surface: &WlSurface) -> Target {
    let handle = h.handle_of(surface);
    let record = h.record(&handle).unwrap();
    Target {
        id: record.id(),
        generation: record.generation(),
    }
}
fn request(h: &Harness, target: Target) -> Member {
    let handle = h.comp().registry.get(target.id).unwrap().handle();
    let window = h
        .wire
        .inner
        .space
        .state
        .elements()
        .find(|window| SurfaceHandle::of_window(window).as_ref() == Some(handle))
        .unwrap();
    let location = h.wire.inner.space.state.element_location(window).unwrap();
    let size = slot::size_of(window).unwrap_or(window.geometry().size);
    Member {
        target,
        group: group(),
        normal: Rect {
            x: location.x,
            y: location.y,
            width: size.w,
            height: size.h,
        },
    }
}
fn admit(h: &mut Harness, target: Target) {
    let request = request(h, target);
    let comp = &mut h.wire.inner.comp;
    comp.tiles
        .admit(&comp.registry, request, Some(area(1001)), |_| {
            Facts::default()
        })
        .unwrap();
}
fn plan(h: &Harness, width: Option<i32>) -> Plan {
    h.comp()
        .tiles
        .plan(&h.comp().registry, &group(), width.map(area), |target| {
            let handle = h.comp().registry.get(target.id).unwrap().handle();
            let window = h
                .wire
                .inner
                .space
                .state
                .elements()
                .find(|window| SurfaceHandle::of_window(window).as_ref() == Some(handle))
                .unwrap();
            Facts {
                overlay: window.is_fullscreen()
                    || ident::states(window).fullscreen
                    || ident::committed_fullscreen(window)
                    || h.comp().maximize_restore(target.id).is_some(),
                ..Facts::default()
            }
        })
}
fn ready(plan: Plan) -> Vec<Allocation> {
    match plan {
        Plan::Ready(cells) => cells,
        other => panic!("{other:?}"),
    }
}

fn native_window(h: &Harness, target: Target) -> smithay::desktop::Window {
    let handle = h.comp().registry.get(target.id).unwrap().handle();
    h.wire
        .inner
        .space
        .state
        .elements()
        .find(|window| SurfaceHandle::of_window(window).as_ref() == Some(handle))
        .unwrap()
        .clone()
}
fn tile(h: &mut Harness, target: Target, enabled: bool) {
    let window = native_window(h, target);
    let host = &mut h.wire.inner;
    world::comp::geometry::set_tiled(
        &mut host.comp,
        &mut host.space.state,
        target,
        &window,
        enabled,
        None,
    )
    .unwrap();
}
fn refresh_native(h: &mut Harness) {
    let host = &mut h.wire.inner;
    world::comp::geometry::refresh_space(&mut host.comp, &mut host.space.state);
}

fn tiled_wait(
    h: &Harness,
    target: Target,
    until: comp_model::request::WaitUntil,
) -> Option<policy::window::WaitResolution> {
    let spec = comp_model::request::WaitSpec {
        window: comp_model::request::WindowMatch {
            id: Some(target.id.0),
            generation: Some(target.generation),
            ..Default::default()
        },
        until,
        timeout: std::time::Duration::from_secs(1),
    };
    policy::window::wait_outcome(
        &h.comp().registry,
        &spec,
        false,
        h.comp().current_workspace(),
        |id| {
            let window = native_window(
                h,
                Target {
                    id,
                    generation: h.comp().registry.get(id).unwrap().generation(),
                },
            );
            let tiles =
                world::comp::geometry::tile_facts(h.comp(), &h.wire.inner.space.state, id, &window);
            policy::window::WindowFacts {
                requested_tiled: tiles.membership,
                native_requested_tiled: tiles.requested,
                committed_tiled: tiles.committed,
                tile_pending: tiles.pending.is_some(),
                configure_pending: tiles.configure_pending,
                ..Default::default()
            }
        },
    )
}

#[test]
fn public_control_fence_removes_suspended_members_in_their_owning_space() {
    use protocols::window::shell::shell;
    let mut h = Harness::new();
    let (surface, _, _) = h.mapped_toplevel(320, 240);
    let member = target(&h, &surface);
    let normal = request(&h, member).normal;
    tile(&mut h, member, true);
    let window = native_window(&h, member);
    {
        let comp = &mut h.wire.inner.comp;
        policy::window::set_minimized(&mut comp.registry, member.id.0, member.generation, true)
            .unwrap();
        comp.note_minimized(member.id, true);
        comp.registry.set_workspace(member.id, 2).unwrap();
        comp.workspaces_changed();
    }
    policy::tiling::resolve_control_target(&h.comp().registry, member, false).unwrap();
    h.client.attach_null(&surface);
    h.roundtrip();
    assert!(!h.comp().registry.get(member.id).unwrap().mapped());
    assert!(policy::tiling::resolve_control_target(&h.comp().registry, member, true).is_err());
    policy::tiling::resolve_control_target(&h.comp().registry, member, false).unwrap();

    // The production route selects the stored owning Space, never host Space.
    let mut dormant = smithay::desktop::Space::default();
    let location = h.wire.inner.space.state.element_location(&window).unwrap();
    h.wire.inner.space.state.unmap_elem(&window);
    dormant.map_element(window.clone(), location, false);
    let host = &mut h.wire.inner;
    world::comp::geometry::set_tiled(&mut host.comp, &mut dormant, member, &window, false, None)
        .unwrap();
    assert!(host.comp.tiles.member(member).is_none());
    assert!(!shell::tile_input_owned(
        window.toplevel().unwrap().wl_surface()
    ));
    assert!(!shell::requested_tiled(&window));
    assert!(
        host.space.state.element_location(&window).is_none(),
        "untile must not migrate dormant windows"
    );
    assert_eq!(
        dormant.element_location(&window),
        Some((normal.x, normal.y).into())
    );
    assert_eq!(
        slot::decided_size(&window),
        Some((normal.width, normal.height).into())
    );
    let stale = Target {
        generation: member.generation + 1,
        ..member
    };
    assert!(matches!(
        policy::tiling::resolve_control_target(&host.comp.registry, stale, false),
        Err(policy::tiling::AdmissionError::Target(
            surfaces::WindowTargetError::StaleTarget { .. }
        ))
    ));
}

#[test]
fn actual_tiled_configures_reflow_two_to_three_without_committing_old_buffers() {
    use protocols::window::shell::shell;
    let mut h = Harness::new();
    let (first, _, _) = h.mapped_toplevel(320, 240);
    let (second, _, _) = h.mapped_toplevel(320, 240);
    let a = target(&h, &first);
    let b = target(&h, &second);
    let normal = request(&h, a).normal;
    tile(&mut h, a, true);
    tile(&mut h, b, true);
    h.roundtrip();
    assert!(
        h.comp().input_geometry_dirty(),
        "native decided placement reaches the existing seat reconciliation owner"
    );
    let wa = native_window(&h, a);
    let wb = native_window(&h, b);
    assert_eq!(slot::decided_size(&wa), Some((960, 1080).into()));
    assert_eq!(
        h.wire.inner.space.state.element_location(&wb),
        Some((960, 0).into())
    );
    assert!(shell::requested_tiled(&wa));
    assert!(
        !shell::committed_tiled(&wa),
        "ACK without a surface commit cannot commit tiled state"
    );
    assert!(tiled_wait(&h, a, comp_model::request::WaitUntil::Tiled).is_none());
    assert_eq!(wa.geometry().size, (320, 240).into());
    h.client.attach(&first, 960, 1080);
    h.client.attach(&second, 960, 1080);
    h.roundtrip();
    assert!(shell::committed_tiled(&wa));
    assert_eq!(
        tiled_wait(&h, a, comp_model::request::WaitUntil::Tiled),
        Some(policy::window::WaitResolution::Window(a.id))
    );
    let (third, _, _) = h.mapped_toplevel(320, 240);
    let c = target(&h, &third);
    tile(&mut h, c, true);
    h.roundtrip();
    assert_eq!(slot::decided_size(&wa), Some((640, 1080).into()));
    assert_eq!(
        wa.geometry().size,
        (960, 1080).into(),
        "old buffer stays native client geometry"
    );
    assert!(
        tiled_wait(&h, a, comp_model::request::WaitUntil::Tiled).is_none(),
        "same-state reflow still waits for current tile geometry"
    );
    h.wire.inner.comp.reserved.insert(
        "testkit-0".into(),
        world::comp::usable::Reserved {
            bottom: 80,
            ..Default::default()
        },
    );
    refresh_native(&mut h);
    h.roundtrip();
    assert_eq!(slot::decided_size(&wa), Some((640, 1000).into()));
    assert_eq!(
        h.wire
            .inner
            .space
            .state
            .element_location(&native_window(&h, c)),
        Some((1280, 0).into())
    );
    tile(&mut h, a, false);
    h.roundtrip();
    assert!(!shell::requested_tiled(&wa));
    assert!(
        shell::committed_tiled(&wa),
        "untile remains pending until the client commits"
    );
    assert!(tiled_wait(&h, a, comp_model::request::WaitUntil::Untiled).is_none());
    assert_eq!(
        slot::decided_size(&wa),
        Some((normal.width, normal.height).into())
    );
    assert_eq!(
        h.wire.inner.space.state.element_location(&wa),
        Some((normal.x, normal.y).into())
    );
    assert_eq!(slot::decided_size(&wb), Some((960, 1000).into()));
    h.client.commit(&first);
    h.roundtrip();
    assert!(
        !shell::committed_tiled(&wa),
        "the empty commit can clear native state but does not replace the old tile buffer"
    );
    assert!(
        tiled_wait(&h, a, comp_model::request::WaitUntil::Untiled).is_none(),
        "normal restore also waits for its actual decided geometry"
    );
    h.client.attach(&first, normal.width, normal.height);
    h.roundtrip();
    assert!(!shell::committed_tiled(&wa));
    assert_eq!(
        tiled_wait(&h, a, comp_model::request::WaitUntil::Untiled),
        Some(policy::window::WaitResolution::Window(a.id))
    );
}

#[test]
fn fullscreen_exit_targets_current_tile_before_old_fullscreen_buffer_commits() {
    use protocols::window::shell::shell;
    let mut h = Harness::new();
    let (first, _, top) = h.mapped_toplevel(320, 240);
    let (second, _, _) = h.mapped_toplevel(320, 240);
    let a = target(&h, &first);
    let b = target(&h, &second);
    let normal = request(&h, a).normal;
    tile(&mut h, a, true);
    tile(&mut h, b, true);
    h.roundtrip();
    h.client.attach(&first, 960, 1080);
    h.roundtrip();
    top.set_fullscreen(None);
    h.roundtrip();
    let wa = native_window(&h, a);
    assert!(!shell::requested_tiled(&wa));
    h.client.attach(&first, 1920, 1080);
    h.roundtrip();
    assert!(ident::committed_fullscreen(&wa));
    let (third, _, _) = h.mapped_toplevel(320, 240);
    let c = target(&h, &third);
    tile(&mut h, c, true);
    h.wire.inner.comp.reserved.insert(
        "testkit-0".into(),
        world::comp::usable::Reserved {
            bottom: 80,
            ..Default::default()
        },
    );
    refresh_native(&mut h);
    top.unset_fullscreen();
    h.roundtrip();
    assert!(shell::requested_tiled(&wa));
    assert!(
        ident::committed_fullscreen(&wa),
        "exit ACK alone retains actual fullscreen commit"
    );
    assert_eq!(slot::decided_size(&wa), Some((640, 1000).into()));
    assert_eq!(wa.geometry().size, (1920, 1080).into());
    refresh_native(&mut h);
    assert_eq!(
        slot::decided_size(&wa),
        Some((640, 1000).into()),
        "old overlay commit cannot regress current tile"
    );
    h.client.attach(&first, 640, 1000);
    h.roundtrip();
    assert!(shell::committed_tiled(&wa));
    assert!(!ident::committed_fullscreen(&wa));
    tile(&mut h, a, false);
    assert_eq!(
        slot::decided_size(&wa),
        Some((normal.width, normal.height).into())
    );
}

#[test]
fn maximise_overlay_keeps_original_normal_restore_and_returns_to_current_group() {
    use protocols::window::shell::shell;
    let mut h = Harness::new();
    let (first, _, _) = h.mapped_toplevel(320, 240);
    let (second, _, _) = h.mapped_toplevel(320, 240);
    let a = target(&h, &first);
    let b = target(&h, &second);
    let normal = request(&h, a).normal;
    tile(&mut h, a, true);
    tile(&mut h, b, true);
    h.roundtrip();
    let wa = native_window(&h, a);
    {
        let host = &mut h.wire.inner;
        world::comp::geometry::set_maximized(
            &mut host.comp,
            &mut host.space.state,
            a.id,
            &wa,
            true,
        );
    }
    refresh_native(&mut h);
    h.roundtrip();
    assert!(!shell::requested_tiled(&wa));
    let restore = h.comp().maximize_restore(a.id).unwrap();
    assert_eq!(restore.size, (normal.width, normal.height).into());
    h.client.attach(&first, 1920, 1080);
    h.roundtrip();
    let (third, _, _) = h.mapped_toplevel(320, 240);
    let c = target(&h, &third);
    tile(&mut h, c, true);
    {
        let host = &mut h.wire.inner;
        world::comp::geometry::set_maximized(
            &mut host.comp,
            &mut host.space.state,
            a.id,
            &wa,
            false,
        );
    }
    refresh_native(&mut h);
    h.roundtrip();
    assert!(shell::requested_tiled(&wa));
    assert_eq!(slot::decided_size(&wa), Some((640, 1080).into()));
    assert_eq!(wa.geometry().size, (1920, 1080).into());
    assert!(h.comp().maximize_restore(a.id).is_none());
    h.client.attach(&first, 640, 1080);
    h.roundtrip();
    assert!(shell::committed_tiled(&wa));
    tile(&mut h, a, false);
    assert_eq!(
        slot::decided_size(&wa),
        Some((normal.width, normal.height).into())
    );
}

#[test]
fn infeasible_overlay_return_restores_normal_and_rejoins_only_after_real_hint_recovery() {
    use protocols::window::shell::shell;
    let mut h = Harness::new();
    let (first, _, top) = h.mapped_toplevel(320, 240);
    let (second, _, _) = h.mapped_toplevel(320, 240);
    let a = target(&h, &first);
    let b = target(&h, &second);
    let normal = request(&h, a).normal;
    tile(&mut h, a, true);
    tile(&mut h, b, true);
    h.roundtrip();
    h.client.attach(&first, 960, 1080);
    h.roundtrip();
    top.set_fullscreen(None);
    h.roundtrip();
    h.client.attach(&first, 1920, 1080);
    h.roundtrip();
    top.set_min_size(1200, 1000);
    h.client.commit(&first);
    h.roundtrip();
    h.wire.inner.comp.reserved.insert(
        "testkit-0".into(),
        world::comp::usable::Reserved {
            bottom: 200,
            ..Default::default()
        },
    );
    refresh_native(&mut h);
    top.unset_fullscreen();
    h.roundtrip();
    let wa = native_window(&h, a);
    assert!(
        h.comp().tiles.member(a).is_some(),
        "pending never abandons membership"
    );
    assert!(
        !shell::requested_tiled(&wa),
        "no successful tile target exists"
    );
    assert_eq!(
        world::comp::geometry::tile_pending(h.comp(), &h.wire.inner.space.state, a.id),
        Some(LayoutError::InsufficientArea { index: 0 })
    );
    assert!(tiled_wait(&h, a, comp_model::request::WaitUntil::Tiled).is_none());
    assert!(
        tiled_wait(&h, a, comp_model::request::WaitUntil::Untiled).is_none(),
        "pending normal fallback retains membership"
    );
    assert_eq!(
        slot::decided_size(&wa),
        Some((normal.width, normal.height).into())
    );
    assert_eq!(
        h.wire.inner.space.state.element_location(&wa),
        Some((normal.x, normal.y).into())
    );
    h.client.attach(&first, normal.width, normal.height);
    h.roundtrip();
    refresh_native(&mut h);
    assert!(
        !shell::requested_tiled(&wa),
        "infeasible hints keep normal mode after overlay commit"
    );
    let serial = h.press(&first, 0x110);
    top._move(h.client.seat(0), serial);
    h.roundtrip();
    assert!(
        h.comp().interactive.is_none(),
        "pending normal geometry retains owner input fence"
    );
    h.release(&first, 0x110);
    top.set_min_size(0, 0);
    h.client.commit(&first);
    h.roundtrip();
    refresh_native(&mut h);
    h.roundtrip();
    assert!(shell::requested_tiled(&wa));
    assert!(
        !shell::committed_tiled(&wa),
        "a feasible plan still needs a real client commit"
    );
    assert_eq!(slot::decided_size(&wa), Some((960, 880).into()));
    h.client.attach(&first, 960, 880);
    h.roundtrip();
    assert!(shell::committed_tiled(&wa));
    assert_eq!(
        h.comp()
            .tiles
            .members()
            .iter()
            .map(|member| member.target)
            .collect::<Vec<_>>(),
        [a, b]
    );
}

#[test]
fn executable_group_tracks_minimise_workspace_unmap_and_actual_role_destruction() {
    let mut h = Harness::new();
    let (first, _, _) = h.mapped_toplevel(320, 240);
    let (second, _, _) = h.mapped_toplevel(320, 240);
    let (third, xdg, top) = h.mapped_toplevel(320, 240);
    let a = target(&h, &first);
    let b = target(&h, &second);
    let c = target(&h, &third);
    for member in [a, b, c] {
        tile(&mut h, member, true);
    }
    h.roundtrip();
    let wa = native_window(&h, a);
    let wb = native_window(&h, b);
    let saved = h.comp().tiles.members().to_vec();
    {
        let comp = &mut h.wire.inner.comp;
        policy::window::set_minimized(&mut comp.registry, b.id.0, b.generation, true).unwrap();
        comp.note_minimized(b.id, true);
    }
    refresh_native(&mut h);
    assert_eq!(slot::decided_size(&wa), Some((960, 1080).into()));
    assert_eq!(h.comp().tiles.members(), saved);
    assert!(tiled_wait(&h, b, comp_model::request::WaitUntil::Tiled).is_none());
    {
        let comp = &mut h.wire.inner.comp;
        policy::window::set_minimized(&mut comp.registry, b.id.0, b.generation, false).unwrap();
        comp.note_minimized(b.id, false);
        comp.registry.set_workspace(c.id, 2).unwrap();
        comp.workspaces_changed();
    }
    refresh_native(&mut h);
    assert_eq!(slot::decided_size(&wb), Some((960, 1080).into()));
    assert_eq!(h.comp().tiles.member(c).unwrap().group.workspace, 2);
    h.wire.inner.comp.registry.set_workspace(c.id, 1).unwrap();
    h.wire.inner.comp.workspaces_changed();
    refresh_native(&mut h);
    assert_eq!(slot::decided_size(&wa), Some((640, 1080).into()));
    top.destroy();
    xdg.destroy();
    h.roundtrip();
    refresh_native(&mut h);
    assert!(h.comp().tiles.member(c).is_none());
    assert_eq!(slot::decided_size(&wa), Some((960, 1080).into()));
    h.client.attach_null(&second);
    h.roundtrip();
    refresh_native(&mut h);
    assert_eq!(slot::decided_size(&wa), Some((1920, 1080).into()));
    assert!(h.comp().tiles.member(b).is_some());
    h.client.commit(&second);
    h.roundtrip();
    h.client.attach(&second, 320, 240);
    h.roundtrip();
    refresh_native(&mut h);
    assert_eq!(slot::decided_size(&wa), Some((960, 1080).into()));
    assert_eq!(
        h.wire.inner.space.state.element_location(&wb),
        Some((960, 0).into())
    );
    assert_eq!(
        h.comp()
            .tiles
            .members()
            .iter()
            .map(|member| member.target)
            .collect::<Vec<_>>(),
        [a, b]
    );
}

#[test]
fn stale_native_executor_requests_cannot_mutate_rebound_window_or_leave_input_lease() {
    let mut h = Harness::new();
    let (surface, _, _) = h.mapped_toplevel(320, 240);
    let old = target(&h, &surface);
    tile(&mut h, old, true);
    let window = native_window(&h, old);
    let native_surface = window.toplevel().unwrap().wl_surface().clone();
    assert!(protocols::window::shell::shell::tile_input_owned(
        &native_surface
    ));
    let handle = h.handle_of(&surface);
    h.wire
        .inner
        .comp
        .bind_uuid(&handle, uuid::Uuid::now_v7(), None);
    assert!(!protocols::window::shell::shell::tile_input_owned(
        &native_surface
    ));
    let slot_before = slot::decided_size(&window);
    let host = &mut h.wire.inner;
    for enabled in [true, false] {
        assert!(matches!(
            world::comp::geometry::set_tiled(
                &mut host.comp,
                &mut host.space.state,
                old,
                &window,
                enabled,
                None
            ),
            Err(policy::tiling::AdmissionError::Target(
                surfaces::WindowTargetError::StaleTarget { .. }
            ))
        ));
    }
    assert!(host.comp.tiles.members().is_empty());
    assert_eq!(slot::decided_size(&window), slot_before);
}

#[test]
fn real_ssd_negotiation_and_new_installed_metrics_fit_fullscreen_return_before_commit() {
    use wayland_protocols::xdg::decoration::zv1::client::zxdg_toplevel_decoration_v1::Mode;
    decor::window::install(decor::theme::ChromeTheme::from_source(
        decor::layout::ChromeStyle::Mac,
        None,
    ));
    let mut h = Harness::new();
    let (first, _, top) = h.mapped_toplevel(320, 240);
    let (second, _, other_top) = h.mapped_toplevel(320, 240);
    let first_decor = h.client.decoration(&top);
    let second_decor = h.client.decoration(&other_top);
    first_decor.set_mode(Mode::ServerSide);
    second_decor.set_mode(Mode::ServerSide);
    h.roundtrip();
    h.client.commit(&first);
    h.client.commit(&second);
    h.roundtrip();
    let a = target(&h, &first);
    let b = target(&h, &second);
    let wa = native_window(&h, a);
    assert!(
        decor::window::normal_extents(&wa).is_some(),
        "actual committed server-side negotiation"
    );
    tile(&mut h, a, true);
    tile(&mut h, b, true);
    h.roundtrip();
    let initial = slot::decided_size(&wa).unwrap();
    h.client.attach(&first, initial.w, initial.h);
    h.roundtrip();
    top.set_fullscreen(None);
    h.roundtrip();
    h.client.attach(&first, 1920, 1080);
    h.roundtrip();
    assert!(ident::committed_fullscreen(&wa));
    assert!(decor::window::extents(&wa).is_none());
    decor::window::install(decor::theme::ChromeTheme::from_source(
        decor::layout::ChromeStyle::Win11,
        None,
    ));
    h.wire.inner.comp.reserved.insert(
        "testkit-0".into(),
        world::comp::usable::Reserved {
            bottom: 80,
            ..Default::default()
        },
    );
    refresh_native(&mut h);
    let extents = decor::window::normal_extents(&wa).unwrap();
    let expected = decor::window::inset(
        smithay::utils::Rectangle::new((0, 0).into(), (960, 1000).into()),
        extents,
    );
    top.unset_fullscreen();
    h.roundtrip();
    assert!(
        ident::committed_fullscreen(&wa),
        "native exit is still held at the client commit fence"
    );
    assert_eq!(slot::decided_size(&wa), Some(expected.size));
    assert_eq!(
        h.wire.inner.space.state.element_location(&wa),
        Some(expected.loc)
    );
    assert!(
        decor::window::extents(&wa).is_none(),
        "render decoration still follows the old committed fullscreen state"
    );
    h.client.attach(&first, expected.size.w, expected.size.h);
    h.roundtrip();
    assert!(!ident::committed_fullscreen(&wa));
    assert!(decor::window::extents(&wa).is_some());
    assert_eq!(slot::decided_size(&wa), Some(expected.size));
}

#[test]
fn actual_output_loss_uses_mapped_fallback_then_keeps_positive_slots_while_pending() {
    use smithay::output::{Mode, Output, PhysicalProperties, Scale, Subpixel};
    use smithay::utils::Transform;
    let mut h = Harness::new();
    let secondary = Output::new(
        "secondary".into(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "testkit".into(),
            model: "headless".into(),
            serial_number: String::new(),
        },
    );
    secondary.change_current_state(
        Some(Mode {
            size: (1280, 800).into(),
            refresh: 60_000,
        }),
        Some(Transform::Normal),
        Some(Scale::Integer(1)),
        None,
    );
    h.wire.inner.space.state.map_output(&secondary, (1920, 0));
    let (first, _, _) = h.mapped_toplevel(320, 240);
    let (second, _, _) = h.mapped_toplevel(320, 240);
    let a = target(&h, &first);
    let b = target(&h, &second);
    tile(&mut h, a, true);
    tile(&mut h, b, true);
    let saved_normal = h.comp().tiles.member(a).unwrap().normal;
    let primary = h.wire.inner.output.clone();
    h.wire.inner.space.state.unmap_output(&primary);
    refresh_native(&mut h);
    let wa = native_window(&h, a);
    assert_eq!(h.comp().tiles.member(a).unwrap().group.output, "secondary");
    assert_eq!(slot::decided_size(&wa), Some((640, 800).into()));
    assert_eq!(
        h.wire.inner.space.state.element_location(&wa),
        Some((1920, 0).into())
    );
    assert_eq!(h.comp().tiles.member(a).unwrap().normal, saved_normal);
    h.wire.inner.space.state.unmap_output(&secondary);
    refresh_native(&mut h);
    assert_eq!(
        world::comp::geometry::tile_pending(h.comp(), &h.wire.inner.space.state, a.id),
        Some(LayoutError::NoOutput)
    );
    assert_eq!(
        slot::decided_size(&wa),
        Some((640, 800).into()),
        "no zero or invented fallback geometry"
    );
    h.wire.inner.space.state.map_output(&primary, (0, 0));
    refresh_native(&mut h);
    assert_eq!(h.comp().tiles.member(a).unwrap().group.output, "testkit-0");
    assert_eq!(slot::decided_size(&wa), Some((960, 1080).into()));
    assert_eq!(
        world::comp::geometry::tile_pending(h.comp(), &h.wire.inner.space.state, a.id),
        None
    );
}

#[test]
fn native_version_one_client_refuses_before_any_tile_owner_or_geometry_mutation() {
    let mut h = Harness::new();
    let surface = h.client.create_surface();
    let (_xdg, _top) = h.client.toplevel_version(&surface, 1);
    h.client.commit(&surface);
    h.roundtrip();
    h.client.attach(&surface, 320, 240);
    h.roundtrip();
    h.tick_frame();
    let member = target(&h, &surface);
    let window = native_window(&h, member);
    let before = slot::decided_size(&window);
    let location = h.wire.inner.space.state.element_location(&window);
    let host = &mut h.wire.inner;
    assert!(matches!(
        world::comp::geometry::set_tiled(
            &mut host.comp,
            &mut host.space.state,
            member,
            &window,
            true,
            None
        ),
        Err(policy::tiling::AdmissionError::UnsupportedProtocol)
    ));
    assert!(host.comp.tiles.members().is_empty());
    assert_eq!(slot::decided_size(&window), before);
    assert_eq!(host.space.state.element_location(&window), location);
    assert!(!protocols::window::shell::shell::tile_input_owned(
        window.toplevel().unwrap().wl_surface()
    ));
}

#[test]
fn untile_while_fullscreen_preserves_overlay_then_restores_original_normal_geometry() {
    let mut h = Harness::new();
    let (surface, _, top) = h.mapped_toplevel(320, 240);
    let member = target(&h, &surface);
    let normal = request(&h, member).normal;
    tile(&mut h, member, true);
    h.roundtrip();
    top.set_fullscreen(None);
    h.roundtrip();
    h.client.attach(&surface, 1920, 1080);
    h.roundtrip();
    let window = native_window(&h, member);
    tile(&mut h, member, false);
    assert!(window.is_fullscreen());
    assert_eq!(slot::decided_size(&window), Some((1920, 1080).into()));
    assert!(h.comp().tiles.member(member).is_none());
    top.unset_fullscreen();
    h.roundtrip();
    assert_eq!(
        slot::decided_size(&window),
        Some((normal.width, normal.height).into())
    );
    assert_eq!(
        h.wire.inner.space.state.element_location(&window),
        Some((normal.x, normal.y).into())
    );
    assert!(tiled_wait(&h, member, comp_model::request::WaitUntil::Untiled).is_none());
    h.client.attach(&surface, normal.width, normal.height);
    h.roundtrip();
    assert_eq!(
        tiled_wait(&h, member, comp_model::request::WaitUntil::Untiled),
        Some(policy::window::WaitResolution::Window(member.id))
    );
}

#[test]
fn two_then_three_native_clients_keep_explicit_order_through_unmap_and_destroy() {
    let mut h = Harness::new();
    let (first, _, _) = h.mapped_toplevel(320, 240);
    let (second, _, _) = h.mapped_toplevel(320, 240);
    let a = target(&h, &first);
    let b = target(&h, &second);
    assert!(
        h.comp().tiles.members().is_empty(),
        "ordinary map never auto-admits"
    );
    admit(&mut h, a);
    admit(&mut h, b);
    let cells = ready(plan(&h, Some(1001)));
    assert_eq!(
        cells.iter().map(|cell| cell.target).collect::<Vec<_>>(),
        [a, b]
    );
    assert_eq!(
        cells
            .iter()
            .map(|cell| cell.cell.outer.width)
            .collect::<Vec<_>>(),
        [500, 501]
    );
    let (third, xdg, top) = h.mapped_toplevel(320, 240);
    let c = target(&h, &third);
    let third_request = request(&h, c);
    assert_eq!(
        h.comp().tiles.members().len(),
        2,
        "third map remains ordinary until admission"
    );
    admit(&mut h, c);
    assert_eq!(
        ready(plan(&h, Some(1001)))
            .iter()
            .map(|cell| cell.cell.outer.width)
            .collect::<Vec<_>>(),
        [333, 334, 334]
    );
    let before = h.comp().tiles.members().to_vec();
    h.client.attach_null(&second);
    h.roundtrip();
    assert_eq!(h.comp().tiles.members(), before);
    assert_eq!(
        ready(plan(&h, Some(1001)))
            .iter()
            .map(|cell| cell.target)
            .collect::<Vec<_>>(),
        [a, c]
    );
    h.client.commit(&second);
    h.roundtrip();
    h.client.attach(&second, 320, 240);
    h.roundtrip();
    assert_eq!(target(&h, &second), b);
    assert_eq!(
        ready(plan(&h, Some(901)))
            .iter()
            .map(|cell| cell.target)
            .collect::<Vec<_>>(),
        [a, b, c]
    );
    assert_eq!(plan(&h, None), Plan::Pending(LayoutError::NoOutput));
    assert_eq!(
        plan(&h, Some(2)),
        Plan::Pending(LayoutError::InsufficientArea { index: 0 })
    );
    assert_eq!(
        h.comp().tiles.members(),
        before,
        "pending area cannot abandon normal restores"
    );
    top.destroy();
    xdg.destroy();
    h.roundtrip();
    assert!(
        h.comp().tiles.member(c).is_none(),
        "actual role destruction retires membership"
    );
    assert_eq!(
        ready(plan(&h, Some(1001)))
            .iter()
            .map(|cell| cell.target)
            .collect::<Vec<_>>(),
        [a, b]
    );
    let comp = &mut h.wire.inner.comp;
    assert!(
        comp.tiles
            .admit(&comp.registry, third_request, Some(area(1001)), |_| {
                Facts::default()
            })
            .is_err(),
        "dead role cannot re-admit"
    );
    third.destroy();
    h.roundtrip();
    assert!(h.comp().registry.get(c.id).is_none());
}

#[test]
fn native_fullscreen_overlay_retains_membership_until_real_exit_commit() {
    let mut h = Harness::new();
    let (first, _, top) = h.mapped_toplevel(320, 240);
    let (second, _, _) = h.mapped_toplevel(320, 240);
    let a = target(&h, &first);
    let b = target(&h, &second);
    admit(&mut h, a);
    admit(&mut h, b);
    let before = h.comp().tiles.members().to_vec();
    top.set_fullscreen(None);
    h.roundtrip();
    assert_eq!(
        ready(plan(&h, Some(901)))
            .iter()
            .map(|cell| cell.target)
            .collect::<Vec<_>>(),
        [b]
    );
    h.client.attach(&first, 1920, 1080);
    h.roundtrip();
    top.unset_fullscreen();
    h.roundtrip();
    assert_eq!(
        ready(plan(&h, Some(901)))
            .iter()
            .map(|cell| cell.target)
            .collect::<Vec<_>>(),
        [b],
        "committed fullscreen still excludes its member during exit"
    );
    h.client.attach(&first, 320, 240);
    h.roundtrip();
    assert_eq!(
        ready(plan(&h, Some(901)))
            .iter()
            .map(|cell| cell.target)
            .collect::<Vec<_>>(),
        [a, b]
    );
    assert_eq!(
        h.comp().tiles.members(),
        before,
        "overlay must not overwrite normal restores/order"
    );
}

#[test]
fn real_uuid_generation_rebinding_retires_membership_without_auto_admission() {
    let mut h = Harness::new();
    let (surface, _, _) = h.mapped_toplevel(320, 240);
    let old = target(&h, &surface);
    let old_request = request(&h, old);
    admit(&mut h, old);
    let handle = h.handle_of(&surface);
    h.wire
        .inner
        .comp
        .bind_uuid(&handle, uuid::Uuid::now_v7(), None);
    let current = target(&h, &surface);
    assert_ne!(current.generation, old.generation);
    assert!(h.comp().tiles.members().is_empty());
    let comp = &mut h.wire.inner.comp;
    assert!(
        comp.tiles
            .admit(&comp.registry, old_request, Some(area(1001)), |_| {
                Facts::default()
            })
            .is_err()
    );
    assert!(comp.tiles.member(current).is_none());
}

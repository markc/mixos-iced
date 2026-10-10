//! Window control on compd's real engine: the
//! client's own maximise / minimise requests reach the host's queue, the
//! minimise LIFO picks the restore rule's candidate, and a destroyed window
//! takes its maximise record with it. The verbs' replies and refusals are
//! policy's (its own window tests), and the engine half (configure,
//! placement, kill) is exercised by the nested smoke.

use dispatcher::wire::trait_::surface_event::WindowRequest;
use policy::workspaces::DefaultOutput;
use smithay::utils::{Point, Size};
use surfaces::SurfaceId;
use testkit::Harness;
use world::comp::MaximizeRestore;

fn harness() -> Harness {
    let mut h = Harness::new();
    h.wire.inner.comp.default_output = Some(DefaultOutput {
        key: "o_test".into(),
        name: "test".into(),
    });
    h
}

fn id_of(h: &Harness, surface: &wayland_client::protocol::wl_surface::WlSurface) -> SurfaceId {
    let handle = h.handle_of(surface);
    h.comp().registry.id_for_handle(&handle).expect("a record")
}

#[test]
fn foreign_activation_resolves_the_owning_space_before_liveness() {
    use dispatcher::wire::trait_::wire_trait::{ActivationOrigin, WireTrait};
    use protocols::window::ident::ident;
    use world::window::lifecycle::event::event::WindowLifecycleEvent;

    let mut h = Harness::new();
    h.wire.state.foreign.set_all_worlds(true);
    let (target_surface, _, target_top) = h.mapped_toplevel(64, 48);
    target_top.set_app_id("testkit.parked".into());
    h.roundtrip();
    let target = h.wire.inner.space.state.elements().next().unwrap().clone();
    let target_uuid = h
        .record(&h.handle_of(&target_surface))
        .unwrap()
        .uuid()
        .unwrap();
    h.wire.inner.switch_world();
    let (_, _, hosted_top) = h.mapped_toplevel(80, 60);
    hosted_top.set_app_id("testkit.hosted".into());
    h.roundtrip();
    let hosted = h.wire.inner.space.state.elements().next().unwrap().clone();
    hosted.set_activated(true);
    protocols::window::shell::shell::send_pending(&hosted);

    let dock = h.add_client();
    h.extra_clients[dock].bind_foreign_toplevels();
    h.roundtrip_client(dock);
    assert!(
        h.extra_clients[dock]
            .state
            .foreign_toplevels
            .iter()
            .any(|(_, id)| id == "testkit.parked")
    );
    assert!(
        h.extra_clients[dock]
            .state
            .foreign_toplevels
            .iter()
            .any(|(_, id)| id == "testkit.hosted")
    );
    assert!(!world::comp::window_is_live(
        &h.wire.inner.comp,
        &h.wire.inner.space.state,
        &target
    ));
    let owner = world::comp::live_window_space(
        &h.wire.inner.comp,
        h.wire.inner.all_world_spaces(),
        &target,
    )
    .expect("the target is live in the parked world");
    assert!(std::ptr::eq(owner, &h.wire.inner.other_space));

    h.extra_clients[dock].activate_foreign("testkit.parked");
    h.roundtrip_client(dock);
    assert!(
        matches!(h.wire.inner.lifecycle[1].as_slice(), [
        WindowLifecycleEvent::Activate(window, ActivationOrigin::Foreign),
    ] if window == &target),
        "the real dock request reaches the lifecycle queue"
    );
    assert_eq!(h.wire.inner.active_world, 1);
    assert_eq!(h.service_lifecycle(), 0);
    assert_eq!(
        h.wire.inner.active_world, 0,
        "activation switches to the target's world"
    );
    assert!(h.wire.inner.space.state.element_location(&target).is_some());
    assert!(ident::states(&target).activated);
    assert!(!ident::states(&hosted).activated);
    assert_eq!(
        h.wire.inner.serviced_lifecycle.last(),
        Some(&(target_uuid, "activated"))
    );
    let applied = h.wire.inner.serviced_lifecycle.len();
    assert_eq!(h.tick_frame(), 0);
    assert_eq!(
        h.wire.inner.serviced_lifecycle.len(),
        applied,
        "activation is consumed once"
    );
}

/// A placed window has no pending InitialMap to protect its ordered tail.
/// Fullscreen entry and exit must still reach it after activation changes worlds.
#[test]
fn placed_fullscreen_requests_survive_an_earlier_cross_world_activation() {
    use dispatcher::wire::trait_::wire_trait::{ActivationOrigin, WireTrait};
    use protocols::window::ident::ident;
    use world::camera::transform::translate::slot;
    use world::window::interface::record::window::LoopWindow;

    for fullscreen in [false, true] {
        let mut h = Harness::new();
        let (a_surface, _, a_top) = h.mapped_toplevel(64, 48);
        let a = h.wire.inner.space.state.elements().next().unwrap().clone();
        let a_uuid = a.uuid().unwrap();
        h.wire.inner.space.state.map_element(a.clone(), (17, 23), false);
        slot::set_expected_size(&a, (64, 48).into());
        let restore_at = h.wire.inner.space.state.element_location(&a).unwrap();
        if !fullscreen {
            h.wire.inner.fullscreen_request(a.clone(), true);
            h.service_lifecycle();
            assert!(a.is_fullscreen());
            h.roundtrip();
        }

        h.wire.inner.switch_world();
        let (_, _, _) = h.mapped_toplevel(80, 60);
        let b = h.wire.inner.space.state.elements().next().unwrap().clone();
        let output = h.wire.inner.output.clone();
        h.wire.inner.space.state.map_output(&output, (2000, 1000));
        let b_at = h.wire.inner.space.state.element_location(&b);
        h.wire.inner.switch_world();
        h.wire.inner.serviced_lifecycle.clear();
        assert_eq!(h.wire.inner.pending_placements(), 0);

        h.wire.inner.request_activation(b.clone(), ActivationOrigin::Foreign);
        // Real xdg request, ordered after Activate(B), on the placed A.
        if fullscreen { a_top.set_fullscreen(None); } else { a_top.unset_fullscreen(); }
        h.roundtrip();
        assert_eq!(h.wire.inner.lifecycle[0].len(), 2);
        h.service_lifecycle();
        assert_eq!(h.wire.inner.active_world, 1);
        assert_eq!(h.wire.inner.serviced_lifecycle, [
            (b.uuid().unwrap(), "activated"), (a_uuid, "fullscreen"),
        ]);
        assert_eq!(a.is_fullscreen(), fullscreen);
        assert_eq!(ident::states(&a).fullscreen, fullscreen);
        assert!(ident::states(&b).activated);
        assert!(!ident::states(&a).activated, "off-world fullscreen cannot steal activation");
        assert!(h.wire.inner.space.state.element_location(&a).is_none());
        assert_eq!(h.wire.inner.space.state.element_location(&b), b_at);
        assert!(h.record(&h.handle_of(&a_surface)).unwrap().mapped());

        h.wire.inner.switch_world();
        h.tick_frame();
        assert_eq!(a.is_fullscreen(), fullscreen);
        if !fullscreen {
            assert_eq!(h.wire.inner.space.state.element_location(&a), Some(restore_at));
            assert_eq!(slot::decided_size(&a), Some((64, 48).into()));
        } else {
            assert_eq!(h.wire.inner.space.state.element_location(&a), Some((0, 0).into()));
            assert_eq!(slot::decided_size(&a), Some(testkit::host::OUTPUT_SIZE.into()));
        }
        assert!(h.wire.inner.lifecycle.iter().all(Vec::is_empty));
        assert_eq!(h.wire.inner.serviced_lifecycle.len(), 2, "consumed exactly once");
    }
}

/// Two protocol dispatches with lifecycle service between them: the later
/// unfullscreen must join A's retained tail, rather than run as a no-op in B.
#[test]
fn parked_retained_fullscreen_cannot_overtake_a_later_unfullscreen() {
    use dispatcher::wire::trait_::wire_trait::{ActivationOrigin, WireTrait};
    use world::camera::transform::translate::slot;
    use world::window::interface::record::window::LoopWindow;
    use world::window::lifecycle::event::event::WindowLifecycleEvent;

    let mut h = Harness::new();
    h.wire.inner.switch_world();
    let (_, _, _) = h.mapped_toplevel(80, 60);
    let b = h.wire.inner.space.state.elements().next().unwrap().clone();
    h.wire.inner.switch_world();
    let surface = h.client.create_surface();
    let (_xdg, top) = h.client.toplevel(&surface);
    h.client.commit(&surface);
    h.roundtrip();
    h.client.attach(&surface, 64, 48);
    h.roundtrip();
    let a = h.wire.inner.space.state.elements().next().unwrap().clone();
    let uuid = a.uuid().unwrap();
    h.client.attach_null(&surface);
    top.set_fullscreen(None);
    h.roundtrip();
    h.wire.inner.request_activation(b, ActivationOrigin::Foreign);
    h.wire.inner.serviced_lifecycle.clear();
    assert_eq!(h.service_lifecycle(), 0);
    assert_eq!(h.wire.inner.active_world, 1);
    assert!(matches!(h.wire.inner.lifecycle[0].as_slice(), [
        WindowLifecycleEvent::InitialMap(map), WindowLifecycleEvent::Fullscreen(full, true),
    ] if map == &a && full == &a));
    assert!(h.wire.inner.lifecycle[1].is_empty());

    // This dispatch happens while B is hosted and A's older work is parked.
    top.unset_fullscreen();
    h.roundtrip();
    assert!(matches!(h.wire.inner.lifecycle[0].as_slice(), [
        WindowLifecycleEvent::InitialMap(map),
        WindowLifecycleEvent::Fullscreen(enter, true),
        WindowLifecycleEvent::Fullscreen(exit, false),
    ] if map == &a && enter == &a && exit == &a));
    assert!(h.wire.inner.lifecycle[1].is_empty(), "B cannot consume A's exit early");
    let serviced = h.wire.inner.serviced_lifecycle.clone();
    assert_eq!(h.service_lifecycle(), 0);
    assert_eq!(h.wire.inner.serviced_lifecycle, serviced);

    h.client.commit(&surface);
    h.roundtrip();
    h.client.attach(&surface, 64, 48);
    h.roundtrip();
    assert_eq!(h.service_lifecycle(), 0, "a buffer alone cannot place in B");
    h.wire.inner.switch_world();
    assert_eq!(h.service_lifecycle(), 1);
    assert_eq!(&h.wire.inner.serviced_lifecycle[1..], [
        (uuid, "placed"), (uuid, "fullscreen"), (uuid, "fullscreen"),
    ]);
    assert!(!a.is_fullscreen(), "the later exit wins when A returns");
    assert!(!protocols::window::ident::ident::states(&a).fullscreen);
    assert_eq!(slot::decided_size(&a), Some((64, 48).into()));
    assert!(h.wire.inner.lifecycle.iter().all(Vec::is_empty));
    assert_eq!(h.tick_frame(), 0);
    assert!(!a.is_fullscreen());
}

/// Routing covers every producer, including uuid-only teardown after unmap.
#[test]
fn every_later_request_joins_the_parked_windows_retained_tail() {
    use dispatcher::wire::trait_::wire_trait::{ActivationOrigin, WireTrait};
    use world::window::interface::record::window::LoopWindow;
    use world::window::lifecycle::event::event::WindowLifecycleEvent;

    for request in ["map", "fullscreen", "activate", "drag", "destroy", "destroy_x11"] {
        let mut h = Harness::new();
        h.wire.inner.switch_world();
        let (_, _, _) = h.mapped_toplevel(80, 60);
        let b = h.wire.inner.space.state.elements().next().unwrap().clone();
        h.wire.inner.switch_world();
        let surface = h.client.create_surface();
        let (xdg, top) = h.client.toplevel(&surface);
        h.client.commit(&surface);
        h.roundtrip();
        h.client.attach(&surface, 64, 48);
        h.roundtrip();
        let a = h.wire.inner.space.state.elements().next().unwrap().clone();
        let uuid = a.uuid().unwrap();
        let server = a.toplevel().unwrap().wl_surface().clone();
        h.client.attach_null(&surface);
        h.roundtrip();
        h.wire.inner.request_activation(b, ActivationOrigin::Foreign);
        h.service_lifecycle();
        assert_eq!(h.wire.inner.active_world, 1);
        assert_eq!(h.wire.inner.lifecycle[0].len(), 1);

        match request {
            "map" => h.wire.inner.place_window(a.clone(), a.geometry()),
            "fullscreen" => h.wire.inner.fullscreen_request(a.clone(), true),
            "activate" => h.wire.inner.request_activation(a.clone(), ActivationOrigin::Foreign),
            "drag" => h.wire.inner.settle_toplevel_drag(server),
            "destroy_x11" => h.wire.inner.destroy_x11_data(a.clone()),
            "destroy" => {
                top.destroy();
                xdg.destroy();
                h.roundtrip();
            }
            _ => unreachable!(),
        }
        assert_eq!(h.wire.inner.lifecycle[0].len(), 2, "{request} appends to A");
        assert!(h.wire.inner.lifecycle[1].is_empty(), "{request} cannot bypass A in B");
        let tail = &h.wire.inner.lifecycle[0][1];
        if request == "destroy" || request == "destroy_x11" {
            assert!(matches!(tail, WindowLifecycleEvent::Destroyed(id, _, _) if *id == uuid));
        } else {
            assert!(tail.targets(&a));
        }
        let applied = h.wire.inner.serviced_lifecycle.len();
        h.service_lifecycle();
        assert_eq!(h.wire.inner.serviced_lifecycle.len(), applied);
    }
}

/// Destruction must find an off-world stack even after Space membership is gone;
/// withdrawal removes it synchronously before another activation can run.
#[test]
fn teardown_after_cross_world_activation_cleans_the_owning_draw_order() {
    use dispatcher::wire::trait_::wire_trait::{ActivationOrigin, WireTrait};
    use world::order::track::base::ComponentId;
    use world::window::interface::record::window::LoopWindow;
    for withdrawn in [false, true] {
        for unmapped in [false, true] {
            let mut h = Harness::new();
            let (_, a_xdg, a_top) = h.mapped_toplevel(64, 48);
            let a = h.wire.inner.space.state.elements().next().unwrap().clone();
            let a_id = ComponentId(a.uuid().unwrap());
            h.wire.inner.switch_world();
            let (_, _, _) = h.mapped_toplevel(80, 60);
            let b = h.wire.inner.space.state.elements().next().unwrap().clone();
            let b_id = ComponentId(b.uuid().unwrap());
            h.wire.inner.switch_world();
            h.wire.inner.serviced_lifecycle.clear();
            let b_key = h.wire.inner.draw_orders[1].key(b_id);
            assert!(h.wire.inner.draw_orders[0].key(a_id).is_some());
            assert!(b_key.is_some());
            h.wire.inner.request_activation(b, ActivationOrigin::Foreign);
            if withdrawn {
                if unmapped { h.wire.inner.space.state.unmap_elem(&a); }
                let world = h.wire.inner.world_ids[0];
                h.wire.inner.withdraw_x11(world, a.clone());
                assert!(h.wire.inner.draw_orders[0].key(a_id).is_none());
            } else {
                a_top.destroy();
                a_xdg.destroy();
                h.roundtrip();
                if !unmapped {
                    // Production Space GC can lag role death.
                    h.wire.inner.space.state.map_element(a.clone(), (0, 0), false);
                }
            }
            h.service_lifecycle();
            assert_eq!(h.wire.inner.active_world, 1);
            assert!(h.wire.inner.draw_orders[0].key(a_id).is_none());
            assert_eq!(h.wire.inner.draw_orders[1].key(b_id), b_key);
            let expected = if withdrawn {
                [(a_id.0, "withdrawn"), (b_id.0, "activated")]
            } else {
                [(b_id.0, "activated"), (a_id.0, "destroyed")]
            };
            assert_eq!(h.wire.inner.serviced_lifecycle, expected);
            h.wire.inner.switch_world();
            h.tick_frame();
            assert!(h.wire.inner.draw_orders[0].key(a_id).is_none());
            assert_eq!(h.wire.inner.serviced_lifecycle.len(), 2);
        }
    }
}

#[test]
fn every_lifecycle_event_tracks_membership_and_waits_behind_its_own_initial_map() {
    use dispatcher::wire::trait_::wire_trait::{ActivationOrigin, WireTrait};
    use world::window::interface::record::window::LoopWindow;
    use world::window::lifecycle::event::event::WindowLifecycleEvent;

    let mut h = Harness::new();
    let (_, _, _) = h.mapped_toplevel(64, 48);
    let a = h.wire.inner.space.state.elements().next().unwrap().clone();
    let server = protocols::window::ident::ident::surface(&a).unwrap();
    let events = [
        WindowLifecycleEvent::InitialMap(a.clone()),
        WindowLifecycleEvent::Activate(a.clone(), ActivationOrigin::Foreign),
        WindowLifecycleEvent::Fullscreen(a.clone(), false),
        WindowLifecycleEvent::Destroyed(a.uuid().unwrap(), Vec::new(), false),
        WindowLifecycleEvent::DragSettled(server),
    ];
    for event in &events {
        for other in &events {
            assert!(event.same_window(other), "all event identities route to the same tail");
        }
    }
    let waiting = [WindowLifecycleEvent::InitialMap(a.clone())];
    h.wire.inner.switch_world();
    for event in &events {
        let owner = event.owning_space(h.wire.inner.all_world_spaces()).unwrap();
        assert!(std::ptr::eq(owner, &h.wire.inner.other_space));
        assert!(event.defer_for_placement(
            &h.wire.inner.comp, h.wire.inner.all_world_spaces(), &h.wire.inner.space, &waiting,
        ));
    }
    assert!(events[0].defer_for_placement(
        &h.wire.inner.comp, h.wire.inner.all_world_spaces(), &h.wire.inner.space, &[],
    ));
    // A world move changes membership, while every queued identity stays the same.
    h.wire.inner.other_space.state.unmap_elem(&a);
    h.wire.inner.space.state.map_element(a, (10, 20), false);
    for event in &events {
        let owner = event.owning_space(h.wire.inner.all_world_spaces()).unwrap();
        assert!(std::ptr::eq(owner, &h.wire.inner.space));
        assert!(!event.defer_for_placement(
            &h.wire.inner.comp, h.wire.inner.all_world_spaces(), &h.wire.inner.space, &[],
        ));
    }
}

/// xdg `set_maximized` / `set_minimized` / `unset_maximized` arrive, in order,
/// as the host's window requests.
#[test]
fn client_window_state_requests_reach_the_host_in_order() {
    let mut h = harness();
    let (surface, _xdg, toplevel) = h.mapped_toplevel(64, 48);
    let id = id_of(&h, &surface);
    toplevel.set_maximized();
    toplevel.set_minimized();
    toplevel.unset_maximized();
    h.roundtrip();
    assert_eq!(
        h.wire.inner.comp.take_requests(),
        [
            (id, WindowRequest::Maximize(true)),
            (id, WindowRequest::Minimize),
            (id, WindowRequest::Maximize(false)),
        ]
    );
    assert!(h.wire.inner.comp.take_requests().is_empty(), "taken once");
}

/// `restore {}` takes the most recently minimised window, and a minimised
/// window is hidden.
#[test]
fn the_minimise_lifo_restores_the_most_recent_first() {
    let mut h = harness();
    let (a, _, _) = h.mapped_toplevel(64, 48);
    let (b, _, _) = h.mapped_toplevel(64, 48);
    let (a, b) = (id_of(&h, &a), id_of(&h, &b));
    let comp = &mut h.wire.inner.comp;
    for id in [a, b] {
        comp.registry.set_minimized(id, true).unwrap();
        comp.note_minimized(id, true);
    }
    assert!(comp.hidden_id(a) && comp.hidden_id(b));
    assert_eq!(comp.lifo_restore_candidate(), Some(b));
    comp.registry.set_minimized(b, false).unwrap();
    comp.note_minimized(b, false);
    assert!(!comp.hidden_id(b));
    assert_eq!(comp.lifo_restore_candidate(), Some(a));
}

/// A maximised window's restore record goes with the window.
#[test]
fn a_destroyed_window_takes_its_maximise_record_with_it() {
    let mut h = harness();
    let (surface, xdg, toplevel) = h.mapped_toplevel(64, 48);
    let id = id_of(&h, &surface);
    let restore = MaximizeRestore {
        location: Point::from((10, 20)),
        size: Size::from((64, 48)),
        output: "test".into(),
    };
    h.wire
        .inner
        .comp
        .set_maximize_restore(id, Some(restore.clone()));
    assert_eq!(h.comp().maximize_restore(id), Some(restore));
    toplevel.destroy();
    xdg.destroy();
    surface.destroy();
    h.roundtrip();
    assert_eq!(h.comp().maximize_restore(id), None);
}

/// The interactive grab's events fold into one record: Begin opens it, each
/// Update keeps only the latest delta (until the host applies it), End marks it
/// ended, and destroying the window drops it. (Driving a real grab needs a
/// client `wl_pointer` button serial, which the testkit client does not bind.)
#[test]
fn interactive_grab_events_fold_into_one_record() {
    use dispatcher::wire::trait_::surface_event::{InteractiveOp, SurfaceEvent};
    let mut h = harness();
    let (surface, xdg, toplevel) = h.mapped_toplevel(64, 48);
    let handle = h.handle_of(&surface);
    let id = id_of(&h, &surface);
    let comp = &mut h.wire.inner.comp;
    comp.apply(SurfaceEvent::Interactive {
        handle: handle.clone(),
        op: InteractiveOp::Begin { edges: 10 },
    });
    for (dx, dy) in [(5.0, 1.0), (12.0, -3.0)] {
        comp.apply(SurfaceEvent::Interactive {
            handle: handle.clone(),
            op: InteractiveOp::Update { dx, dy },
        });
    }
    let grab = comp.interactive.expect("a grab in progress");
    assert_eq!(
        (grab.id, grab.edges, grab.delta, grab.updated, grab.ended),
        (id, 10, (12.0, -3.0), true, false)
    );
    comp.apply(SurfaceEvent::Interactive {
        handle: handle.clone(),
        op: InteractiveOp::End,
    });
    assert!(comp.interactive.unwrap().ended);
    toplevel.destroy();
    xdg.destroy();
    surface.destroy();
    h.roundtrip();
    assert!(
        h.comp().interactive.is_none(),
        "the window's grab goes with it"
    );
}

/// A corner configuration change moves both revisions, so the edge pass
/// publishes `props.changed input.corners.*` and `compd.truth` brackets it;
/// an unchanged set moves neither.
#[test]
fn a_corner_config_change_moves_both_revisions() {
    let mut h = harness();
    let comp = &mut h.wire.inner.comp;
    let (activity, content) = (comp.revision(), comp.content_revision());
    let mut config = comp.corners.config();
    comp.corners.set_config(config);
    assert_eq!(
        (comp.revision(), comp.content_revision()),
        (activity, content)
    );
    config.dwell_ms = 350;
    comp.corners.set_config(config);
    assert_eq!(
        (comp.revision(), comp.content_revision()),
        (activity.wrapping_add(1), content.wrapping_add(1))
    );
    assert_eq!(comp.corners.config().dwell_ms, 350);
}

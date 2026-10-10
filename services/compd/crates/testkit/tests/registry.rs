//! The comp registry follows real clients through compd's real engine.

use surfaces::SurfaceRole;
use testkit::Harness;
use world::comp::CURRENT_WORKSPACE;

#[test]
fn retired_surface_ids_are_not_walked_as_live_trees() {
    use protocols::window::ident::ident::tree_index;
    use smithay::reexports::wayland_server::{Resource, protocol::wl_surface::WlSurface};

    let mut h = Harness::new();
    let (surface, xdg, toplevel) = h.mapped_toplevel(64, 48);
    let server = h
        .wire
        .inner
        .space
        .state
        .elements()
        .next()
        .unwrap()
        .toplevel()
        .unwrap()
        .wl_surface()
        .clone();
    let id = server.id();
    let display = h.wire.state.output.display_handle.clone();
    assert_eq!(tree_index(&server), Some(0));
    toplevel.destroy();
    xdg.destroy();
    surface.destroy();
    h.roundtrip();
    assert!(!server.is_alive());
    assert_eq!(tree_index(&server), None);
    // Some server backends retain reconstructible IDs until their final
    // references go away. Such a proxy no longer has compositor userdata.
    if let Ok(retired) = WlSurface::from_id(&display, id) {
        assert!(!retired.is_alive());
        assert_eq!(tree_index(&retired), None);
        assert!(Some(retired).filter(Resource::is_alive).is_none());
    }
}

/// (a) An xdg toplevel takes its role, maps only at lifecycle service that places
/// it, goes dormant when its role object dies and leaves the registry with its
/// surface.
#[test]
fn xdg_toplevel_maps_without_frame_and_unmaps_on_destroy() {
    let mut h = Harness::new();
    let surface = h.client.create_surface();
    let (xdg, toplevel) = h.client.toplevel(&surface);
    toplevel.set_app_id("testkit.toplevel".to_string());
    h.client.commit(&surface);
    h.roundtrip();

    let handle = h.handle_of(&surface);
    let record = h.record(&handle).expect("the toplevel has a record");
    assert_eq!(record.role(), SurfaceRole::Toplevel);
    assert!(!record.mapped(), "no buffer yet");
    assert_eq!(h.wire.inner.pending_placements(), 0);
    assert!(
        record.uuid().is_some(),
        "the drain binds a uuid to a new window"
    );
    assert!(
        h.client.state.configures >= 1,
        "the initial configure arrived"
    );
    assert_eq!(h.service_lifecycle(), 0, "a bufferless configure needs no placement");

    h.client.attach(&surface, 64, 48);
    h.roundtrip();
    assert!(
        !h.record(&handle).unwrap().mapped(),
        "a toplevel's buffer does not map it: its placement does"
    );
    assert_eq!(h.wire.inner.pending_placements(), 1);

    assert_eq!(h.service_lifecycle(), 1);
    let record = h.record(&handle).unwrap();
    assert!(record.mapped());
    assert_eq!(record.workspace(), Some(CURRENT_WORKSPACE));
    assert!(record.is_window_row());
    assert_eq!(record.app_id().map(|id| &**id), Some("testkit.toplevel"));
    assert_eq!(h.comp().registry.windows().count(), 1);
    assert_eq!(h.service_lifecycle(), 0, "the queue was consumed");
    assert_eq!(h.tick_frame(), 0, "resume cannot place it again");

    toplevel.destroy();
    xdg.destroy();
    h.roundtrip();
    let record = h.record(&handle).expect("the surface lives on");
    assert_eq!(record.role(), SurfaceRole::Dormant);
    assert!(!record.mapped());
    assert!(record.uuid().is_none(), "dormancy unbinds the uuid");
    assert_eq!(h.comp().registry.windows().count(), 0);

    surface.destroy();
    h.roundtrip();
    assert!(
        h.record(&handle).is_none(),
        "the surface's destruction removes the record"
    );
    assert!(h.comp().registry.is_empty());
}

/// (b) An xdg popup records its parent and maps with its buffer (no frame step).
#[test]
fn xdg_popup_with_parent() {
    let mut h = Harness::new();
    let (parent, parent_xdg, _parent_toplevel) = h.mapped_toplevel(64, 48);
    let parent_id = h
        .comp()
        .registry
        .id_for_handle(&h.handle_of(&parent))
        .expect("the parent has a record");

    let surface = h.client.create_surface();
    let (xdg, popup) = h.client.popup(&surface, &parent_xdg);
    h.client.commit(&surface);
    h.roundtrip();

    let handle = h.handle_of(&surface);
    let record = h.record(&handle).expect("the popup has a record");
    assert_eq!(record.role(), SurfaceRole::Popup);
    assert_eq!(record.parent(), Some(parent_id));
    assert!(!record.mapped());
    assert!(record.uuid().is_none(), "only windows carry a uuid");

    h.client.attach(&surface, 20, 20);
    h.roundtrip();
    let record = h.record(&handle).unwrap();
    assert!(record.mapped(), "a popup maps with its buffer");
    assert_eq!(record.workspace(), None, "a popup carries no workspace");
    assert_eq!(
        h.tick_frame(),
        0,
        "a popup is never placed by the frame step"
    );

    popup.destroy();
    xdg.destroy();
    h.roundtrip();
    assert_eq!(h.record(&handle).unwrap().role(), SurfaceRole::Dormant);

    surface.destroy();
    h.roundtrip();
    assert!(h.record(&handle).is_none());
    assert!(
        h.record(&h.handle_of(&parent)).unwrap().mapped(),
        "the parent is untouched"
    );
}

/// (c) A layer surface that leaves its output to the compositor maps with its
/// buffer once configured.
#[test]
fn layer_surface() {
    let mut h = Harness::new();
    let surface = h.client.create_surface();
    let layer = h.client.layer(&surface, "testkit.layer");
    h.client.commit(&surface);
    h.roundtrip();

    let handle = h.handle_of(&surface);
    let record = h.record(&handle).expect("the layer surface has a record");
    assert_eq!(record.role(), SurfaceRole::Layer);
    assert!(!record.mapped());
    assert!(
        !h.comp().layer_explicit(&handle),
        "no output named: binding is the default"
    );
    assert!(
        h.client.state.configures >= 1,
        "the layer surface was configured"
    );

    h.client.attach(&surface, 100, 30);
    h.roundtrip();
    let record = h.record(&handle).unwrap();
    assert!(record.mapped(), "a layer surface maps with its buffer");
    assert_eq!(
        record.workspace(),
        None,
        "a layer surface carries no workspace"
    );

    layer.destroy();
    h.roundtrip();
    assert_eq!(h.record(&handle).unwrap().role(), SurfaceRole::Dormant);

    surface.destroy();
    h.roundtrip();
    assert!(h.record(&handle).is_none());
}

/// (d) A null buffer unmaps a placed toplevel without retiring it; a buffer
/// after the re-configure maps it again with no second placement.
#[test]
fn null_buffer_unmap() {
    let mut h = Harness::new();
    let (surface, _xdg, _toplevel) = h.mapped_toplevel(64, 48);
    let handle = h.handle_of(&surface);
    let before = h.record(&handle).unwrap().clone();
    let window = h.wire.inner.space.state.elements().next().unwrap().clone();
    let at = h.wire.inner.space.state.element_location(&window);
    assert!(before.mapped());

    h.client.attach_null(&surface);
    h.roundtrip();
    let record = h.record(&handle).unwrap();
    assert!(!record.mapped(), "a null attach unmaps");
    assert_eq!(
        record.role(),
        SurfaceRole::Toplevel,
        "the role survives an unmap"
    );
    assert_eq!(
        record.generation(),
        before.generation(),
        "an unmap never bumps the generation"
    );
    assert_eq!(record.uuid(), before.uuid());
    assert_eq!(h.comp().registry.windows().count(), 0);

    // xdg-shell: an unmapped toplevel starts over with a bufferless commit and
    // a configure before its next buffer. The configure must actually arrive: a
    // real client (testkit-input-probe --hide-on-close) waits for it before it
    // attaches, and without one it never shows again (the comp_control_smoke gate).
    let configures = h.client.state.configures;
    h.client.commit(&surface);
    h.roundtrip();
    assert!(
        h.client.state.configures > configures,
        "the re-initial commit earns a new configure"
    );
    h.client.attach(&surface, 64, 48);
    h.roundtrip();
    let record = h.record(&handle).unwrap();
    assert!(record.mapped(), "a placed window follows its buffer");
    assert_eq!(record.generation(), before.generation());
    assert_eq!(record.uuid(), before.uuid());
    assert_eq!(record.workspace(), before.workspace());
    assert_eq!(h.wire.inner.space.state.element_location(&window), at);
    assert_eq!(h.service_lifecycle(), 0, "remap needs no new placement");
    assert_eq!(h.tick_frame(), 0, "a window is placed once");
}

#[test]
fn repeated_commits_queue_one_initial_placement() {
    let mut h = Harness::new();
    let surface = h.client.create_surface();
    let (_xdg, _toplevel) = h.client.toplevel(&surface);
    h.client.commit(&surface);
    h.roundtrip();
    h.client.attach(&surface, 64, 48);
    h.roundtrip();
    h.client.commit(&surface);
    h.roundtrip();
    h.client.attach(&surface, 64, 48);
    h.roundtrip();
    assert_eq!(h.wire.inner.pending_placements(), 1);
    assert_eq!(h.service_lifecycle(), 1);
    let handle = h.handle_of(&surface);
    let before = h.record(&handle).unwrap().clone();
    let at = h.wire.inner.space.state.element_location(
        h.wire.inner.space.state.elements().next().unwrap(),
    );
    for _ in 0..3 {
        h.client.commit(&surface);
        h.roundtrip();
        assert_eq!(h.wire.inner.pending_placements(), 0);
        assert_eq!(h.service_lifecycle(), 0);
    }
    assert_eq!(h.tick_frame(), 0);
    let after = h.record(&handle).unwrap();
    assert!(after.mapped());
    assert_eq!(after.uuid(), before.uuid());
    assert_eq!(after.generation(), before.generation());
    assert_eq!(after.workspace(), before.workspace());
    assert_eq!(h.wire.inner.space.state.element_location(
        h.wire.inner.space.state.elements().next().unwrap(),
    ), at);
}

#[test]
fn destroy_before_lifecycle_does_not_resurrect_window() {
    let mut h = Harness::new();
    let surface = h.client.create_surface();
    let (xdg, toplevel) = h.client.toplevel(&surface);
    h.client.commit(&surface);
    h.roundtrip();
    h.client.attach(&surface, 64, 48);
    h.roundtrip();
    let handle = h.handle_of(&surface);
    let window = h.wire.inner.space.state.elements().next().unwrap().clone();
    assert_eq!(h.wire.inner.pending_placements(), 1);
    toplevel.destroy();
    xdg.destroy();
    h.roundtrip();
    // Retain membership deliberately: production Space GC can lag role death.
    h.wire.inner.space.state.map_element(window.clone(), (0, 0), false);
    assert_eq!(h.record(&handle).unwrap().role(), SurfaceRole::Dormant);
    assert_eq!(h.service_lifecycle(), 0);
    assert_eq!(h.wire.inner.pending_placements(), 0);
    assert!(!h.record(&handle).unwrap().mapped());
    assert_eq!(h.comp().registry.windows().count(), 0);
    assert_eq!(h.tick_frame(), 0);
    surface.destroy();
    h.roundtrip();
    assert!(h.record(&handle).is_none());
}

#[test]
fn null_buffer_before_lifecycle_does_not_map_window() {
    let mut h = Harness::new();
    let surface = h.client.create_surface();
    let (_xdg, _toplevel) = h.client.toplevel(&surface);
    h.client.commit(&surface);
    h.roundtrip();
    h.client.attach(&surface, 64, 48);
    h.roundtrip();
    assert_eq!(h.wire.inner.pending_placements(), 1);
    h.client.attach_null(&surface);
    h.roundtrip();
    assert_eq!(h.service_lifecycle(), 0);
    assert!(!h.record(&h.handle_of(&surface)).unwrap().mapped());
    assert_eq!(h.comp().registry.windows().count(), 0);
    assert_eq!(h.wire.inner.pending_placements(), 1, "retain the only candidate");
    h.client.commit(&surface);
    h.roundtrip();
    h.client.attach(&surface, 64, 48);
    h.roundtrip();
    assert_eq!(h.wire.inner.pending_placements(), 1, "the marker prevents duplicates");
    assert_eq!(h.service_lifecycle(), 1);
    assert!(h.record(&h.handle_of(&surface)).unwrap().mapped());
    assert_eq!(h.tick_frame(), 0);
}

#[test]
fn bufferless_candidate_survives_cross_world_activation() {
    use dispatcher::wire::trait_::surface_event::SurfaceHandle;
    use dispatcher::wire::trait_::wire_trait::{ActivationOrigin, WireTrait};
    use world::window::lifecycle::event::event::WindowLifecycleEvent;

    // Cover the requested return-then-reattach sequence and a buffer arriving
    // while away: placement policy must wait for the owning world in both.
    for reattach_before_return in [false, true] {
        let mut h = Harness::new();
        let (other_surface, _other_xdg, _other_top) = h.mapped_toplevel(80, 60);
        let other_handle = h.handle_of(&other_surface);
        let other_uuid = h.record(&other_handle).unwrap().uuid().unwrap();
        let other = h.wire.inner.space.state.elements().next().unwrap().clone();
        h.wire.inner.switch_world();
        h.wire.inner.serviced_lifecycle.clear();

        let surface = h.client.create_surface();
        let (_xdg, _toplevel) = h.client.toplevel(&surface);
        h.client.commit(&surface);
        h.roundtrip();
        h.client.attach(&surface, 64, 48);
        h.roundtrip();
        let handle = h.handle_of(&surface);
        let before = h.record(&handle).unwrap().clone();
        let uuid = before.uuid().unwrap();
        let window = h.wire.inner.space.state.elements().next().unwrap().clone();
        let server = window.toplevel().unwrap().wl_surface().clone();
        assert_eq!(h.wire.inner.pending_placements(), 1);
        h.client.attach_null(&surface);
        h.roundtrip();
        h.wire
            .inner
            .request_activation(window.clone(), ActivationOrigin::Foreign);
        h.wire.inner.fullscreen_request(window.clone(), true);
        h.wire.inner.settle_toplevel_drag(server.clone());
        h.wire
            .inner
            .request_activation(other.clone(), ActivationOrigin::Foreign);

        assert_eq!(h.service_lifecycle(), 0);
        assert_eq!(
            h.wire.inner.active_world, 0,
            "unrelated activation switches worlds"
        );
        assert_eq!(h.wire.inner.serviced_lifecycle, [(other_uuid, "activated")]);
        assert!(h.wire.inner.space.state.element_location(&window).is_none());
        assert!(
            h.wire
                .inner
                .other_space
                .state
                .element_location(&window)
                .is_some()
        );
        for _ in 0..3 {
            assert_eq!(h.service_lifecycle(), 0);
            assert_eq!(h.tick_frame(), 0);
            assert_eq!(h.wire.inner.active_world, 0);
            assert_eq!(
                h.wire.inner.pending_placements(),
                1,
                "retain A's only candidate"
            );
            assert!(!h.record(&handle).unwrap().mapped());
            assert!(
                matches!(h.wire.inner.lifecycle[1].as_slice(), [
                WindowLifecycleEvent::InitialMap(map),
                WindowLifecycleEvent::Activate(activation, _),
                WindowLifecycleEvent::Fullscreen(fullscreen, true),
                WindowLifecycleEvent::DragSettled(settled),
            ] if map == &window && activation == &window
                && fullscreen == &window && settled == &server),
                "A's dependent events remain ordered behind its candidate"
            );
        }

        if reattach_before_return {
            h.client.commit(&surface);
            h.roundtrip();
            h.client.attach(&surface, 64, 48);
            h.roundtrip();
            assert!(world::comp::initial_map_is_live(
                &h.wire.inner.comp,
                &h.wire.inner.other_space.state,
                &window,
            ));
            assert_eq!(
                h.service_lifecycle(),
                0,
                "a ready buffer cannot place into B's world"
            );
            assert_eq!(h.tick_frame(), 0);
            assert_eq!(h.wire.inner.active_world, 0, "A's activation still waits");
            assert_eq!(h.wire.inner.lifecycle[1].len(), 4);
            assert!(!h.record(&handle).unwrap().mapped());
        }

        h.wire.inner.switch_world();
        assert_eq!(h.wire.inner.active_world, 1, "return to A's owning world");
        if !reattach_before_return {
            assert_eq!(
                h.service_lifecycle(),
                0,
                "return alone cannot replace a buffer"
            );
            h.client.commit(&surface);
            h.roundtrip();
            h.client.attach(&surface, 64, 48);
            h.roundtrip();
        }
        assert_eq!(
            h.wire.inner.pending_placements(),
            1,
            "reattach queues no duplicate"
        );
        assert_eq!(h.service_lifecycle(), 1);
        assert_eq!(
            h.wire.inner.serviced_lifecycle,
            [
                (other_uuid, "activated"),
                (uuid, "placed"),
                (uuid, "activated"),
                (uuid, "fullscreen"),
            ],
            "placement precedes A's dependent events"
        );
        let record = h.record(&handle).unwrap();
        assert!(record.mapped());
        assert_eq!(record.uuid(), before.uuid());
        assert_eq!(record.generation(), before.generation());
        assert!(
            h.comp()
                .registry
                .windows()
                .any(|record| record.uuid() == Some(uuid)),
            "A appears in the mapped registry"
        );
        assert_eq!(h.comp().registry.windows().count(), 2);
        assert!(h.record(&other_handle).unwrap().mapped());
        assert!(
            h.wire
                .inner
                .other_space
                .state
                .element_location(&window)
                .is_none()
        );
        let at = h.wire.inner.space.state.element_location(&window);
        assert!(at.is_some(), "placement stays in A's owning Space");
        assert_eq!(SurfaceHandle::of_window(&window).as_ref(), Some(&handle));
        for _ in 0..3 {
            h.client.commit(&surface);
            h.roundtrip();
            assert_eq!(h.service_lifecycle(), 0);
            assert_eq!(h.tick_frame(), 0);
            assert_eq!(h.wire.inner.pending_placements(), 0);
            assert!(h.wire.inner.lifecycle.iter().all(Vec::is_empty));
            assert_eq!(h.wire.inner.space.state.element_location(&window), at);
        }
        assert_eq!(
            h.wire
                .inner
                .serviced_lifecycle
                .iter()
                .filter(|event| **event == (uuid, "placed"))
                .count(),
            1,
            "A is placed exactly once"
        );
    }
}

#[test]
fn bufferless_candidate_defers_only_its_own_lifecycle() {
    use dispatcher::wire::trait_::wire_trait::{ActivationOrigin, WireTrait};
    use world::window::lifecycle::event::event::WindowLifecycleEvent;

    let mut h = Harness::new();
    let second = h.add_client();
    let third = h.add_client();
    let gone = h.extra_clients[third].create_surface();
    let (gone_xdg, gone_top) = h.extra_clients[third].toplevel(&gone);
    h.extra_clients[third].commit(&gone);
    h.roundtrip_client(third);
    h.extra_clients[third].attach(&gone, 64, 48);
    h.roundtrip_client(third);
    assert_eq!(h.service_lifecycle(), 1);
    let gone_handle = h.handle_of_client(third, &gone);
    let gone_uuid = h.record(&gone_handle).unwrap().uuid().unwrap();
    h.wire.inner.serviced_lifecycle.clear();

    let first = h.client.create_surface();
    let (_first_xdg, _first_top) = h.client.toplevel(&first);
    h.client.commit(&first);
    h.roundtrip();
    h.client.attach(&first, 64, 48);
    h.roundtrip();
    let first_handle = h.handle_of(&first);
    let first_uuid = h.record(&first_handle).unwrap().uuid().unwrap();
    let first_window = h
        .wire
        .inner
        .space
        .state
        .elements()
        .find(|window| {
            dispatcher::wire::trait_::surface_event::SurfaceHandle::of_window(window).as_ref()
                == Some(&first_handle)
        })
        .unwrap()
        .clone();
    let first_server = first_window.toplevel().unwrap().wl_surface().clone();
    h.client.attach_null(&first);
    h.roundtrip();
    h.wire
        .inner
        .request_activation(first_window.clone(), ActivationOrigin::Foreign);
    h.wire.inner.fullscreen_request(first_window.clone(), true);
    h.wire.inner.settle_toplevel_drag(first_server.clone());
    assert_eq!(h.service_lifecycle(), 0);

    let next = h.extra_clients[second].create_surface();
    let (_next_xdg, _next_top) = h.extra_clients[second].toplevel(&next);
    h.extra_clients[second].commit(&next);
    h.roundtrip_client(second);
    h.extra_clients[second].attach(&next, 80, 60);
    h.roundtrip_client(second);
    let next_handle = h.handle_of_client(second, &next);
    let next_uuid = h.record(&next_handle).unwrap().uuid().unwrap();
    let next_window = h
        .wire
        .inner
        .space
        .state
        .elements()
        .find(|window| {
            dispatcher::wire::trait_::surface_event::SurfaceHandle::of_window(window).as_ref()
                == Some(&next_handle)
        })
        .unwrap()
        .clone();
    h.wire
        .inner
        .request_activation(next_window.clone(), ActivationOrigin::Foreign);
    h.wire.inner.fullscreen_request(next_window, true);
    gone_top.destroy();
    gone_xdg.destroy();
    h.roundtrip_client(third);

    assert_eq!(
        h.service_lifecycle(),
        1,
        "another client maps despite the bufferless candidate"
    );
    assert!(h.record(&next_handle).unwrap().mapped());
    assert_eq!(h.record(&gone_handle).unwrap().role(), SurfaceRole::Dormant);
    assert_eq!(
        h.wire.inner.serviced_lifecycle,
        [
            (next_uuid, "placed"),
            (next_uuid, "activated"),
            (next_uuid, "fullscreen"),
            (gone_uuid, "destroyed"),
        ],
        "unrelated maps, activation, fullscreen and teardown all progress"
    );
    assert!(!h.record(&first_handle).unwrap().mapped());
    assert_eq!(h.wire.inner.pending_placements(), 1);
    assert!(
        matches!(h.wire.inner.lifecycle[0].as_slice(), [
        WindowLifecycleEvent::InitialMap(map), WindowLifecycleEvent::Activate(activation, _),
        WindowLifecycleEvent::Fullscreen(fullscreen, true), WindowLifecycleEvent::DragSettled(surface),
    ] if map == &first_window && activation == &first_window
        && fullscreen == &first_window && surface == &first_server),
        "only the first window's ordered tail waits"
    );
    for _ in 0..3 {
        assert_eq!(h.service_lifecycle(), 0);
        assert_eq!(
            h.tick_frame(),
            0,
            "VT resume does not unblock a bufferless window"
        );
        assert_eq!(h.wire.inner.lifecycle[0].len(), 4);
    }

    h.client.commit(&first);
    h.roundtrip();
    h.client.attach(&first, 64, 48);
    h.roundtrip();
    h.client.commit(&first);
    h.roundtrip();
    assert_eq!(
        h.wire.inner.pending_placements(),
        1,
        "reattach queues no duplicate candidate"
    );
    assert_eq!(h.service_lifecycle(), 1);
    assert!(h.record(&first_handle).unwrap().mapped());
    assert_eq!(
        &h.wire.inner.serviced_lifecycle[4..],
        [
            (first_uuid, "placed"),
            (first_uuid, "activated"),
            (first_uuid, "fullscreen"),
        ],
        "placement precedes the retained dependent events"
    );
    assert_eq!(h.service_lifecycle(), 0);
    assert_eq!(
        h.tick_frame(),
        0,
        "reattached candidate is placed exactly once"
    );
    assert!(h.wire.inner.lifecycle.iter().all(Vec::is_empty));
    gone.destroy();
    h.roundtrip_client(third);
    assert!(h.record(&gone_handle).is_none());
}

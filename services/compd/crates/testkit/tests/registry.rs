//! The comp registry follows real clients through compd's real engine.

use surfaces::SurfaceRole;
use testkit::Harness;
use world::comp::CURRENT_WORKSPACE;

/// (a) An xdg toplevel takes its role, maps only at the frame step that places
/// it, goes dormant when its role object dies and leaves the registry with its
/// surface.
#[test]
fn xdg_toplevel_maps_at_frame_and_unmaps_on_destroy() {
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
    assert!(
        record.uuid().is_some(),
        "the drain binds a uuid to a new window"
    );
    assert!(
        h.client.state.configures >= 1,
        "the initial configure arrived"
    );

    h.client.attach(&surface, 64, 48);
    h.roundtrip();
    assert!(
        !h.record(&handle).unwrap().mapped(),
        "a toplevel's buffer does not map it: its placement does"
    );
    assert_eq!(h.wire.inner.pending_placements(), 1);

    assert_eq!(h.tick_frame(), 1);
    let record = h.record(&handle).unwrap();
    assert!(record.mapped());
    assert_eq!(record.workspace(), Some(CURRENT_WORKSPACE));
    assert!(record.is_window_row());
    assert_eq!(
        record.app_id().map(|id| &**id),
        Some("testkit.toplevel")
    );
    assert_eq!(h.comp().registry.windows().count(), 1);

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
    assert_eq!(h.tick_frame(), 0, "a window is placed once");
}

// SPDX-License-Identifier: MIT OR Apache-2.0
//! Production geometry execution and real xdg ACK/commit semantics. This fixture
//! has no renderer, scene settings worker or GPU: fitted coordinates are protocol
//! geometry evidence, not native pixels or a settings-to-frame timing measure.

use policy_host::geometry::{self, GeometryChange};
use protocols::window::shell::shell;
use smithay::desktop::Window;
use smithay::output::{Mode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::utils::{Size, Transform};
use surfaces::SurfaceId;
use testkit::{Harness, client::protocol_id};
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_protocols::xdg::shell::client::{
    xdg_surface::XdgSurface,
    xdg_toplevel::{self, XdgToplevel},
};
use world::camera::transform::translate::{fit::window_fit, slot};
use world::comp::usable::Reserved;
use world::window::interface::data::data::WindowFullscreen;
use world::window::interface::record::window::LoopWindow;

fn window(h: &Harness, surface: &WlSurface) -> (SurfaceId, Window) {
    let handle = h.handle_of(surface);
    let id = h
        .comp()
        .registry
        .id_for_handle(&handle)
        .expect("registry identity");
    let window = h
        .wire
        .inner
        .space
        .state
        .elements()
        .find(|window| {
            dispatcher::wire::trait_::surface_event::SurfaceHandle::of_window(window).as_ref()
                == Some(&handle)
        })
        .expect("mapped window")
        .clone();
    (id, window)
}

fn maximize(h: &mut Harness, id: SurfaceId, window: &Window, enabled: bool) -> GeometryChange {
    let host = &mut h.wire.inner;
    geometry::set_maximized(&mut host.comp, &mut host.space.state, id, window, enabled)
}

fn refresh(h: &mut Harness, id: SurfaceId, window: &Window) -> GeometryChange {
    let host = &mut h.wire.inner;
    geometry::refresh_usable(
        &mut host.comp,
        &mut host.space.state,
        &[(id, window.clone())],
    )
}

fn bottom(h: &mut Harness, px: i32) {
    h.wire.inner.comp.reserved.insert(
        h.wire.inner.output.name(),
        Reserved {
            bottom: px,
            ..Reserved::default()
        },
    );
}

fn serial(h: &Harness, xdg: &XdgSurface) -> u32 {
    h.client
        .state
        .xdg_configures
        .iter()
        .rev()
        .find(|(id, _)| *id == protocol_id(xdg))
        .expect("received configure")
        .1
}

fn count(h: &Harness, top: &XdgToplevel) -> usize {
    h.client
        .state
        .toplevel_configures
        .iter()
        .filter(|event| event.toplevel == protocol_id(top))
        .count()
}

fn configured(h: &Harness, top: &XdgToplevel, size: (i32, i32), maximized: bool) {
    let event = h
        .client
        .state
        .toplevel_configures
        .iter()
        .rev()
        .find(|event| event.toplevel == protocol_id(top))
        .expect("toplevel configure");
    assert_eq!(event.size, size);
    assert_eq!(
        event
            .states
            .contains(&(xdg_toplevel::State::Maximized as u32)),
        maximized
    );
}

fn fit(h: &Harness, window: &Window) -> world::camera::transform::translate::fit::WindowFit {
    let content = window.geometry();
    window_fit(
        h.wire.inner.space.state.element_location(window).unwrap(),
        content,
        content.size,
        slot::decided_size(window).unwrap(),
        false,
    )
}

#[test]
fn delayed_ack_and_older_buffer_never_regress_the_decided_slot() {
    let mut h = Harness::new();
    let (surface, xdg, top) = h.mapped_toplevel(640, 480);
    let (id, window) = window(&h, &surface);
    h.wire
        .inner
        .space
        .state
        .map_element(window.clone(), (32, 24), false);
    refresh(&mut h, id, &window);
    assert!(maximize(&mut h, id, &window, true).windows);
    h.roundtrip();
    configured(&h, &top, (1920, 1080), true);
    h.client.attach(&surface, 1920, 1080);
    h.roundtrip();
    assert_eq!(window.geometry().size, Size::from((1920, 1080)));
    let restore = h.comp().maximize_restore(id).unwrap();
    h.client.state.hold_xdg_configures = true;

    let before = count(&h, &top);
    bottom(&mut h, 80);
    assert_eq!(
        refresh(&mut h, id, &window),
        GeometryChange {
            usable: true,
            windows: true
        }
    );
    h.roundtrip();
    assert_eq!(count(&h, &top), before + 1);
    configured(&h, &top, (1920, 1000), true);
    let older = serial(&h, &xdg);
    assert_eq!(slot::decided_size(&window), Some((1920, 1000).into()));
    assert_eq!(window.geometry().size, Size::from((1920, 1080)));
    let delayed_fit = fit(&h, &window);
    assert_eq!(
        delayed_fit.fit_sx, delayed_fit.fit_sy,
        "no resize gesture stretch"
    );
    assert_eq!(delayed_fit.fit_sx, 1.0);
    assert_eq!(
        delayed_fit.fit_surf,
        (0.0, -40.0),
        "old content centres in the smaller authoritative slot"
    );
    xdg.ack_configure(older);
    h.roundtrip();
    assert_eq!(slot::decided_size(&window), Some((1920, 1000).into()));
    assert_eq!(
        window.geometry().size,
        Size::from((1920, 1080)),
        "ACK alone is not a replacement buffer"
    );

    bottom(&mut h, 160);
    assert!(refresh(&mut h, id, &window).windows);
    h.roundtrip();
    configured(&h, &top, (1920, 920), true);
    h.client.attach(&surface, 1920, 1000); // legally ACKed older configure
    h.roundtrip();
    assert_eq!(window.geometry().size, Size::from((1920, 1000)));
    assert_eq!(slot::decided_size(&window), Some((1920, 920).into()));
    let before = count(&h, &top);
    assert_eq!(refresh(&mut h, id, &window), GeometryChange::default());
    h.roundtrip();
    assert_eq!(
        count(&h, &top),
        before,
        "no configure churn while newest ACK is held"
    );
    xdg.ack_configure(serial(&h, &xdg));
    h.client.attach(&surface, 1920, 920);
    h.roundtrip();
    assert_eq!(fit(&h, &window).fit_sx, 1.0);
    assert_eq!(fit(&h, &window).fit_surf, (0.0, 0.0));

    bottom(&mut h, 0);
    assert!(refresh(&mut h, id, &window).windows);
    h.roundtrip();
    configured(&h, &top, (1920, 1080), true);
    xdg.ack_configure(serial(&h, &xdg));
    h.client.attach(&surface, 1920, 1080);
    h.roundtrip();
    assert_eq!(h.comp().maximize_restore(id), Some(restore.clone()));
    assert!(maximize(&mut h, id, &window, false).windows);
    h.roundtrip();
    configured(&h, &top, (640, 480), false);
    assert_eq!(
        h.wire.inner.space.state.element_location(&window),
        Some(restore.location)
    );
    assert_eq!(slot::decided_size(&window), Some(restore.size));
    assert!(h.comp().maximize_restore(id).is_none());
}

fn output(name: &str, size: (i32, i32)) -> Output {
    let output = Output::new(
        name.into(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "testkit".into(),
            model: "headless".into(),
            serial_number: name.into(),
        },
    );
    output.change_current_state(
        Some(Mode {
            size: size.into(),
            refresh: 60_000,
        }),
        Some(Transform::Normal),
        Some(Scale::Integer(1)),
        None,
    );
    output
}

#[test]
fn only_the_owning_output_reconfigures_and_restore_survives_output_loss() {
    let mut h = Harness::new();
    let secondary = output("secondary", (1280, 800));
    h.wire.inner.space.state.map_output(&secondary, (1920, 0));
    let (surface, _, top) = h.mapped_toplevel(640, 480);
    let (id, window) = window(&h, &surface);
    h.wire
        .inner
        .space
        .state
        .map_element(window.clone(), (2000, 100), false);
    h.wire.inner.space.state.refresh();
    refresh(&mut h, id, &window);
    maximize(&mut h, id, &window, true);
    h.roundtrip();
    configured(&h, &top, (1280, 800), true);
    let restore = h.comp().maximize_restore(id).unwrap();
    assert_eq!(restore.output, "secondary");
    let before = count(&h, &top);
    bottom(&mut h, 80); // unrelated primary output
    assert_eq!(
        refresh(&mut h, id, &window),
        GeometryChange {
            usable: true,
            windows: false
        }
    );
    h.roundtrip();
    assert_eq!(count(&h, &top), before);
    h.wire.inner.comp.reserved.insert(
        "secondary".into(),
        Reserved {
            bottom: 80,
            ..Reserved::default()
        },
    );
    assert!(refresh(&mut h, id, &window).windows);
    h.roundtrip();
    configured(&h, &top, (1280, 720), true);
    h.wire.inner.space.state.unmap_output(&secondary);
    assert!(refresh(&mut h, id, &window).windows);
    h.roundtrip();
    configured(&h, &top, (1920, 1000), true);
    let fallback = h.comp().maximize_restore(id).unwrap();
    assert_eq!(
        (fallback.location, fallback.size),
        (restore.location, restore.size)
    );
    let primary = h.wire.inner.output.clone();
    h.wire.inner.space.state.unmap_output(&primary);
    let before = count(&h, &top);
    assert_eq!(
        refresh(&mut h, id, &window),
        GeometryChange {
            usable: true,
            windows: false
        }
    );
    h.roundtrip();
    assert_eq!(count(&h, &top), before);
    assert_eq!(h.comp().maximize_restore(id), Some(fallback));
    h.wire.inner.space.state.map_output(&secondary, (1920, 0));
    assert!(refresh(&mut h, id, &window).windows);
    h.roundtrip();
    configured(&h, &top, (1280, 720), true);
    let recovered = h.comp().maximize_restore(id).unwrap();
    assert_eq!(recovered, restore);
    maximize(&mut h, id, &window, false);
    h.roundtrip();
    assert_eq!(
        h.wire.inner.space.state.element_location(&window),
        Some((2000, 100).into())
    );
    configured(&h, &top, (640, 480), false);
}

#[test]
fn fullscreen_holds_geometry_until_exit_commit_then_uses_latest_work_area() {
    let mut h = Harness::new();
    let (surface, xdg, top) = h.mapped_toplevel(640, 480);
    let (id, window) = window(&h, &surface);
    refresh(&mut h, id, &window);
    maximize(&mut h, id, &window, true);
    h.roundtrip();
    h.client.attach(&surface, 1920, 1080);
    h.roundtrip();
    let restore = h.comp().maximize_restore(id).unwrap();
    window.set_fullscreen(Some(WindowFullscreen {
        restore_loc: (0, 0).into(),
        restore_size: (1920, 1080).into(),
    }));
    bottom(&mut h, 40);
    assert!(
        !refresh(&mut h, id, &window).windows,
        "compositor fullscreen record owns slot before protocol intent"
    );
    // Stage the real protocol fullscreen ownership. Loop-owned restore data is
    // separately fenced by the same production helper; no fake Loop is built.
    shell::set_fullscreen(&window, true);
    shell::send(&window);
    h.client.state.hold_xdg_configures = true;
    h.roundtrip();
    let before = count(&h, &top);
    bottom(&mut h, 80);
    assert!(
        !refresh(&mut h, id, &window).windows,
        "pending fullscreen owns slot"
    );
    h.roundtrip();
    assert_eq!(count(&h, &top), before);
    xdg.ack_configure(serial(&h, &xdg));
    h.client.attach(&surface, 1920, 1080);
    h.roundtrip();
    shell::set_fullscreen(&window, false);
    window.set_fullscreen(None);
    shell::send(&window);
    h.roundtrip();
    let before = count(&h, &top);
    bottom(&mut h, 160);
    assert!(
        !refresh(&mut h, id, &window).windows,
        "committed fullscreen retains ownership through delayed exit"
    );
    h.roundtrip();
    assert_eq!(count(&h, &top), before);
    assert_eq!(slot::decided_size(&window), Some((1920, 1080).into()));
    xdg.ack_configure(serial(&h, &xdg));
    h.client.attach(&surface, 1920, 1080);
    h.roundtrip();
    assert!(
        refresh(&mut h, id, &window).windows,
        "unchanged usable map must still reconcile exit"
    );
    h.roundtrip();
    configured(&h, &top, (1920, 920), true);
    let before = count(&h, &top);
    assert_eq!(refresh(&mut h, id, &window), GeometryChange::default());
    h.roundtrip();
    assert_eq!(count(&h, &top), before);
    assert_eq!(h.comp().maximize_restore(id), Some(restore));
}

#[test]
fn unmaximise_restores_the_decided_size_when_client_geometry_lags() {
    let mut h = Harness::new();
    let (surface, _, top) = h.mapped_toplevel(640, 480);
    let (id, window) = window(&h, &surface);
    h.wire
        .inner
        .space
        .state
        .map_element(window.clone(), (16, 24), false);
    slot::set_expected_size(&window, (800, 600).into());
    assert_eq!(window.geometry().size, Size::from((640, 480)));
    maximize(&mut h, id, &window, true);
    h.roundtrip();
    assert_eq!(
        h.comp().maximize_restore(id).unwrap().size,
        Size::from((800, 600))
    );
    bottom(&mut h, 80);
    assert!(refresh(&mut h, id, &window).windows);
    h.roundtrip();
    maximize(&mut h, id, &window, false);
    h.roundtrip();
    configured(&h, &top, (800, 600), false);
    assert_eq!(slot::decided_size(&window), Some((800, 600).into()));
    assert_eq!(
        h.wire.inner.space.state.element_location(&window),
        Some((16, 24).into())
    );
}

#[test]
fn unmaximise_without_a_restore_record_clears_intent_without_moving_the_slot() {
    let mut h = Harness::new();
    let (surface, _, top) = h.mapped_toplevel(640, 480);
    let (id, window) = window(&h, &surface);
    h.wire
        .inner
        .space
        .state
        .map_element(window.clone(), (16, 24), false);
    slot::set_expected_size(&window, (800, 600).into());
    shell::stage(&window, (800, 600).into(), false);
    shell::set_maximized(&window, true);
    shell::send(&window);
    h.roundtrip();
    configured(&h, &top, (800, 600), true);
    assert!(h.comp().maximize_restore(id).is_none());

    assert_eq!(
        maximize(&mut h, id, &window, false),
        GeometryChange::default()
    );
    h.roundtrip();
    configured(&h, &top, (800, 600), false);
    assert_eq!(slot::decided_size(&window), Some((800, 600).into()));
    assert_eq!(
        h.wire.inner.space.state.element_location(&window),
        Some((16, 24).into())
    );
    assert!(h.comp().maximize_restore(id).is_none());
    let before = count(&h, &top);
    maximize(&mut h, id, &window, false);
    h.roundtrip();
    assert_eq!(
        count(&h, &top),
        before + 1,
        "explicit requests retain their configure response"
    );
    configured(&h, &top, (800, 600), false);
    assert_eq!(slot::decided_size(&window), Some((800, 600).into()));
    bottom(&mut h, 80);
    let before = count(&h, &top);
    assert_eq!(
        refresh(&mut h, id, &window),
        GeometryChange {
            usable: true,
            windows: false
        }
    );
    h.roundtrip();
    assert_eq!(
        count(&h, &top),
        before,
        "work-area changes cannot recreate cleared intent"
    );
    assert_eq!(slot::decided_size(&window), Some((800, 600).into()));
    assert_eq!(
        h.wire.inner.space.state.element_location(&window),
        Some((16, 24).into())
    );
}

#[test]
fn refreshing_another_world_never_admits_its_window_into_this_space() {
    let mut h = Harness::new();
    let (surface, _, top) = h.mapped_toplevel(640, 480);
    let (id, window) = window(&h, &surface);
    refresh(&mut h, id, &window);
    maximize(&mut h, id, &window, true);
    h.roundtrip();
    let restore = h.comp().maximize_restore(id).unwrap();
    h.wire.inner.space.state.unmap_elem(&window);
    let mut other = smithay::desktop::Space::default();
    other.map_element(window.clone(), (10, 20), false);
    bottom(&mut h, 80);
    let before = count(&h, &top);
    assert_eq!(
        refresh(&mut h, id, &window),
        GeometryChange {
            usable: true,
            windows: false
        }
    );
    h.roundtrip();
    assert!(h.wire.inner.space.state.element_location(&window).is_none());
    assert_eq!(other.element_location(&window), Some((10, 20).into()));
    assert_eq!(count(&h, &top), before);
    assert_eq!(h.comp().maximize_restore(id), Some(restore));
    // Once its world is hosted again, the unchanged usable map still reflows.
    other.unmap_elem(&window);
    h.wire
        .inner
        .space
        .state
        .map_element(window.clone(), (32, 24), false);
    assert!(refresh(&mut h, id, &window).windows);
    h.roundtrip();
    configured(&h, &top, (1920, 1000), true);
}

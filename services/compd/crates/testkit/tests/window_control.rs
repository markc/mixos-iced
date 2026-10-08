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

#[test]
fn keyboard_only_ownership_uses_the_real_human_seat_grab() {
    use dispatcher::state::state::Dispatch;
    use smithay::backend::input::KeyState;
    use smithay::input::keyboard::{GrabStartData, KeyboardGrab, KeyboardInnerHandle, Keycode, ModifiersState};
    use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
    use smithay::utils::{Serial, SERIAL_COUNTER};

    struct OwnedKeyboard(GrabStartData<Dispatch>);
    impl KeyboardGrab<Dispatch> for OwnedKeyboard {
        fn input(&mut self, data: &mut Dispatch, handle: &mut KeyboardInnerHandle<'_, Dispatch>, keycode: Keycode, state: KeyState, modifiers: Option<ModifiersState>, serial: Serial, time: u32) {
            handle.input(data, keycode, state, modifiers, serial, time);
        }
        fn set_focus(&mut self, _: &mut Dispatch, _: &mut KeyboardInnerHandle<'_, Dispatch>, _: Option<WlSurface>, _: Serial) {}
        fn start_data(&self) -> &GrabStartData<Dispatch> { &self.0 }
        fn unset(&mut self, _: &mut Dispatch) {}
    }

    let mut h = Harness::new();
    let seat = h.wire.state.seat.seat.clone();
    let keyboard = seat.get_keyboard().expect("native human keyboard");
    assert!(!policy_host::input::human_keyboard_owned(&seat));
    keyboard.set_grab(&mut h.wire.state, OwnedKeyboard(GrabStartData { focus: None }), SERIAL_COUNTER.next_serial());
    assert!(seat.get_pointer().is_some_and(|pointer| !pointer.is_grabbed()), "keyboard ownership must not require pointer ownership");
    assert!(policy_host::input::human_keyboard_owned(&seat), "world activation and targeted input share the actual keyboard ownership fact");
    keyboard.unset_grab(&mut h.wire.state);
    assert!(!policy_host::input::human_keyboard_owned(&seat));
}

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

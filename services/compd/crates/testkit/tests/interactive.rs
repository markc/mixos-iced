//! Real interactive move/resize grabs: a client
//! names the button serial it was given in `xdg_toplevel.move` / `.resize`,
//! the grab starts on the primary seat and reports the pointer's travel;
//! the agent seat and a maximised window are refused.
//!
//! The grab only reports deltas into `CompState::interactive`; moving the
//! window is policy-host `apply_interactive`, which needs the full `Loop`
//! (the nested smoke covers it), so these tests stop at the record.

use smithay::input::pointer::{ButtonEvent, MotionEvent};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State;
use smithay::utils::{Point, SERIAL_COUNTER};
use testkit::Harness;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_protocols::xdg::shell::client::xdg_toplevel::ResizeEdge;

const BTN_LEFT: u32 = 0x110;
/// `TestClient::seat` indices: the primary seat is advertised first.
const PRIMARY: usize = 0;
const AGENT: usize = 1;

/// Move the server's primary pointer to `location` (grab-routed while a grab
/// holds the pointer) and let the drain run.
fn motion(h: &mut Harness, location: (f64, f64)) {
    let pointer = h
        .wire
        .state
        .seat
        .seat
        .get_pointer()
        .expect("primary pointer");
    let dispatch = &mut h.wire.state;
    pointer.motion(
        dispatch,
        None,
        &MotionEvent {
            location: Point::from(location),
            serial: SERIAL_COUNTER.next_serial(),
            time: 0,
        },
    );
    pointer.frame(dispatch);
    h.roundtrip();
}

/// Let the button go on the server's primary pointer. Not `Harness::release`:
/// the grab clears client focus, so the client sees no release to assert on.
fn let_go(h: &mut Harness) {
    let pointer = h
        .wire
        .state
        .seat
        .seat
        .get_pointer()
        .expect("primary pointer");
    let dispatch = &mut h.wire.state;
    pointer.button(
        dispatch,
        &ButtonEvent {
            serial: SERIAL_COUNTER.next_serial(),
            time: 0,
            button: BTN_LEFT,
            state: smithay::backend::input::ButtonState::Released,
        },
    );
    pointer.frame(dispatch);
    h.roundtrip();
}

/// Commit `state` on the server toplevel of `surface` and have the client
/// ack and commit it.
fn commit_state(h: &mut Harness, surface: &WlSurface, state: State) {
    let handle = h.handle_of(surface);
    let toplevel = h
        .wire
        .inner
        .space
        .state
        .elements()
        .find(|window| {
            dispatcher::wire::trait_::surface_event::SurfaceHandle::of_window(window).as_ref()
                == Some(&handle)
        })
        .and_then(|window| window.toplevel().cloned())
        .expect("the window is in the Space");
    toplevel.with_pending_state(|pending| {
        pending.states.set(state);
    });
    toplevel.send_configure();
    h.roundtrip();
    h.client.commit(surface);
    h.roundtrip();
}

#[test]
fn a_move_on_the_primary_seat_grabs_and_reports_the_travel() {
    let mut h = Harness::new();
    let (surface, _xdg, toplevel) = h.mapped_toplevel(64, 48);
    let serial = h.press(&surface, BTN_LEFT);
    toplevel._move(h.client.seat(PRIMARY), serial);
    h.roundtrip();
    let grab = h.comp().interactive.expect("the move started a grab");
    assert_eq!((grab.edges, grab.updated, grab.ended), (0, false, false));
    // The harness pressed at (1, 1).
    motion(&mut h, (13.0, 4.0));
    let grab = h.comp().interactive.expect("still grabbing");
    assert_eq!((grab.delta, grab.updated), ((12.0, 3.0), true));
    let_go(&mut h);
    assert!(
        h.comp()
            .interactive
            .expect("the record waits for the host")
            .ended
    );
}

#[test]
fn a_resize_carries_its_edges() {
    let mut h = Harness::new();
    let (surface, _xdg, toplevel) = h.mapped_toplevel(64, 48);
    let serial = h.press(&surface, BTN_LEFT);
    toplevel.resize(h.client.seat(PRIMARY), serial, ResizeEdge::BottomRight);
    h.roundtrip();
    assert_eq!(h.comp().interactive.map(|grab| grab.edges), Some(10));
    motion(&mut h, (21.0, 31.0));
    assert_eq!(
        h.comp().interactive.map(|grab| grab.delta),
        Some((20.0, 30.0))
    );
}

#[test]
fn the_agent_seat_cannot_move_a_window() {
    let mut h = Harness::new();
    let (surface, _xdg, toplevel) = h.mapped_toplevel(64, 48);
    // A valid primary-seat serial: only the seat differs from the passing case.
    let serial = h.press(&surface, BTN_LEFT);
    toplevel._move(h.client.seat(AGENT), serial);
    h.roundtrip();
    assert!(h.comp().interactive.is_none(), "the agent seat is refused");
    toplevel.resize(h.client.seat(AGENT), serial, ResizeEdge::Right);
    h.roundtrip();
    assert!(h.comp().interactive.is_none());
}

#[test]
fn a_maximised_window_is_not_moved() {
    let mut h = Harness::new();
    let (surface, _xdg, toplevel) = h.mapped_toplevel(64, 48);
    commit_state(&mut h, &surface, State::Maximized);
    let serial = h.press(&surface, BTN_LEFT);
    toplevel._move(h.client.seat(PRIMARY), serial);
    h.roundtrip();
    assert!(
        h.comp().interactive.is_none(),
        "a committed-maximised window starts no move"
    );
}

#[test]
fn a_stale_serial_starts_nothing() {
    let mut h = Harness::new();
    let (surface, _xdg, toplevel) = h.mapped_toplevel(64, 48);
    let serial = h.press(&surface, BTN_LEFT);
    toplevel._move(h.client.seat(PRIMARY), serial.wrapping_add(1000));
    h.roundtrip();
    assert!(
        h.comp().interactive.is_none(),
        "the serial must name the held button"
    );
}

#[test]
fn tile_admission_refuses_real_native_move_and_resize_before_its_first_commit() {
    let mut h = Harness::new();
    let (surface, _, top) = h.mapped_toplevel(320, 240);
    let handle = h.handle_of(&surface);
    let record = h
        .comp()
        .registry
        .get(h.comp().registry.id_for_handle(&handle).unwrap())
        .unwrap();
    let target = policy::tiling::Target {
        id: record.id(),
        generation: record.generation(),
    };
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
        .unwrap()
        .clone();
    {
        let host = &mut h.wire.inner;
        world::comp::geometry::set_tiled(
            &mut host.comp,
            &mut host.space.state,
            target,
            &window,
            true,
            None,
        )
        .unwrap();
    }
    h.roundtrip();
    assert!(!protocols::window::shell::shell::committed_tiled(&window));
    let serial = h.press(&surface, BTN_LEFT);
    top._move(h.client.seat(PRIMARY), serial);
    top.resize(h.client.seat(PRIMARY), serial, ResizeEdge::Right);
    h.roundtrip();
    assert!(h.comp().interactive.is_none());
    h.release(&surface, BTN_LEFT);
    {
        let host = &mut h.wire.inner;
        world::comp::geometry::set_tiled(
            &mut host.comp,
            &mut host.space.state,
            target,
            &window,
            false,
            None,
        )
        .unwrap();
    }
    h.roundtrip();
    h.client.attach(&surface, 320, 240);
    h.roundtrip();
    assert!(!protocols::window::shell::shell::tile_input_owned(
        window.toplevel().unwrap().wl_surface()
    ));
    let serial = h.press(&surface, BTN_LEFT);
    top._move(h.client.seat(PRIMARY), serial);
    h.roundtrip();
    assert!(
        h.comp().interactive.is_some(),
        "untile restores ordinary real grab admission"
    );
}

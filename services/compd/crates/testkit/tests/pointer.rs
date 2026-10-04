//! The harness drives a pointer button through the server's primary seat and
//! the client receives it with a serial it can hand to `xdg_toplevel.move` /
//! `.resize` (the harness half; the real-grab tests build on this).

use testkit::Harness;

const BTN_LEFT: u32 = 0x110;

#[test]
fn a_press_reaches_the_client_with_a_serial_and_its_seat() {
    let mut h = Harness::new();
    let (surface, _xdg, _toplevel) = h.mapped_toplevel(64, 48);
    let serial = h.press(&surface, BTN_LEFT);
    assert!(h.client.state.enter_serial.is_some(), "the pointer entered the surface first");
    let (_, button, pressed, seat) = *h
        .client
        .state
        .buttons
        .iter()
        .find(|(s, ..)| *s == serial)
        .expect("the press arrived");
    assert_eq!(button, BTN_LEFT);
    assert!(pressed);
    // The seat it came from is the one a move/resize request must name.
    let _primary = h.client.seat(seat);
    let released = h.release(&surface, BTN_LEFT);
    assert_ne!(released, serial, "each button event has its own serial");
}

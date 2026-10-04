use smithay::input::pointer::CursorImageStatus;
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::reexports::wayland_protocols::wp::pointer_constraints::zv1::server::zwp_confined_pointer_v1::ZwpConfinedPointerV1;
use smithay::reexports::wayland_protocols::wp::pointer_constraints::zv1::server::zwp_locked_pointer_v1::ZwpLockedPointerV1;
use smithay::reexports::wayland_protocols::wp::pointer_constraints::zv1::server::zwp_pointer_constraints_v1::ZwpPointerConstraintsV1;
use smithay::reexports::wayland_protocols::wp::pointer_gestures::zv1::server::zwp_pointer_gesture_hold_v1::ZwpPointerGestureHoldV1;
use smithay::reexports::wayland_protocols::wp::pointer_gestures::zv1::server::zwp_pointer_gesture_pinch_v1::ZwpPointerGesturePinchV1;
use smithay::reexports::wayland_protocols::wp::pointer_gestures::zv1::server::zwp_pointer_gesture_swipe_v1::ZwpPointerGestureSwipeV1;
use smithay::reexports::wayland_protocols::wp::pointer_gestures::zv1::server::zwp_pointer_gestures_v1::ZwpPointerGesturesV1;
use smithay::reexports::wayland_protocols::wp::relative_pointer::zv1::server::zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1;
use smithay::reexports::wayland_protocols::wp::relative_pointer::zv1::server::zwp_relative_pointer_v1::ZwpRelativePointerV1;
use smithay::reexports::wayland_server::protocol::wl_seat::WlSeat;
use smithay::reexports::wayland_server::{Client, Dispatch, DisplayHandle, GlobalDispatch};
use smithay::xwayland::XWaylandClientData;
use smithay::wayland::GlobalData;
use smithay::wayland::pointer_constraints::{PointerConstraintsState, PointerConstraintUserData};
use smithay::wayland::pointer_gestures::{PointerGesturesState, PointerGestureUserData};
use smithay::wayland::relative_pointer::{RelativePointerManagerState, RelativePointerUserData};
use smithay::wayland::seat::{SeatGlobalData, WaylandFocus};
use crate::state::state::DispatchWire;

/// The input seat: every physical and nested input device routes here.
/// Named `seat0` on the wire.
pub const PRIMARY_SEAT: &str = "seat0";

/// The agent seat: the per-agent input lane. Created and advertised now, routed
/// nowhere yet — no device feeds it and the `SeatHandler` callbacks ignore it
/// (`state.rs`), so it cannot disturb the primary seat's focus, cursor or LEDs.
pub const AGENT_SEAT: &str = "agent";

/// One named `wl_seat` on `seat_state`, with a keyboard (the saved layout, NumLock
/// on) and a pointer. Callable once per seat name; capabilities that belong to the
/// session rather than a seat (touch, the pointer-protocol managers) are NOT added
/// here, so a second call never duplicates a global.
pub fn seat_named<I: SeatHandler + 'static>(
    seat_state: &mut SeatState<I>,
    display_handle: &DisplayHandle,
    name: &str,
    visible: impl Fn(&Client) -> bool + Send + Sync + 'static,
) -> Seat<I>
where
    I: GlobalDispatch<WlSeat, SeatGlobalData<I>>,
    <I as SeatHandler>::PointerFocus: WaylandFocus,
    <I as SeatHandler>::KeyboardFocus: WaylandFocus,
{
    // The seat global is advertised only to clients
    // `visible` accepts.
    let mut seat: Seat<I> = seat_state.new_wl_seat_with_filter(display_handle, name, visible);
    let layout = protocols::seat::xkb::xkb::load();
    let csv = protocols::seat::xkb::xkb::layout_csv(&layout);
    let cfg = protocols::seat::xkb::xkb::checked_config(&layout, &csv);
    let keyboard = seat.add_keyboard(cfg, 200, 25).unwrap();
    let mut mods = keyboard.modifier_state();
    mods.num_lock = true;
    keyboard.set_modifier_state(mods);
    seat.add_pointer();
    seat
}

pub fn new<I: DispatchWire>(
    display_handle: &DisplayHandle,
) -> protocols::seat::state::Seat<I>
where
    I: GlobalDispatch<ZwpRelativePointerManagerV1, GlobalData>,
    I: Dispatch<ZwpRelativePointerManagerV1, GlobalData>,
    I: Dispatch<ZwpRelativePointerV1, RelativePointerUserData<I>>,
    I: GlobalDispatch<WlSeat, SeatGlobalData<I>> + SeatHandler + 'static,
    I: GlobalDispatch<ZwpPointerConstraintsV1, GlobalData>,
    I: Dispatch<ZwpPointerConstraintsV1, GlobalData>,
    I: Dispatch<ZwpConfinedPointerV1, PointerConstraintUserData<I>>,
    I: Dispatch<ZwpLockedPointerV1, PointerConstraintUserData<I>>,
    I: GlobalDispatch<ZwpPointerGesturesV1, GlobalData>,
    I: Dispatch<ZwpPointerGesturesV1, GlobalData>,
    I: Dispatch<ZwpPointerGestureSwipeV1, PointerGestureUserData<I>>,
    I: Dispatch<ZwpPointerGesturePinchV1, PointerGestureUserData<I>>,
    I: Dispatch<ZwpPointerGestureHoldV1, PointerGestureUserData<I>>,
    <I as SeatHandler>::PointerFocus: WaylandFocus,
    <I as SeatHandler>::KeyboardFocus: WaylandFocus,
{
    // A seat groups keyboards/pointer/touch and maintains keyboard + pointer focus.
    let mut seat_state = SeatState::new();

    // The primary seat, then the agent seat on the SAME `SeatState`. Keyboard
    // (200 ms delay, 25/s; saved layout via `seat.xkb`, compile-checked so a bad
    // preference cannot panic the unwrap) and NumLock-on are set in `seat_named`.
    // In Wayland the compositor owns the xkb state, so NumLock is ours to set; it
    // is a LOCKED modifier and is mirrored to the LEDs via `led_state_changed`.
    // Primary first: clients that pick "the first wl_seat" get the input seat.
    let mut seat: Seat<I> = seat_named(&mut seat_state, display_handle, PRIMARY_SEAT, |_| true);
    // The agent seat is hidden from Xwayland: an X server binding it would treat
    // a second seat as more X input devices.
    let agent: Seat<I> = seat_named(&mut seat_state, display_handle, AGENT_SEAT, |client| {
        client.get_data::<XWaylandClientData>().is_none()
    });

    // (`seat_named` already added the pointer: clients render their own cursor
    // icons and hover states from its enter/leave/motion.)

    // wl_touch: native multi-touch for client surfaces.
    //
    // UNCONDITIONAL, unlike the tablet capability (which `wire.input` adds and removes
    // per device). `wl_seat` is a LOGICAL seat — capabilities are a per-seat bitmask,
    // not per-device — so the conditional form would mean advertising on the first
    // `DeviceCapability::Touch` device and calling `remove_touch()` when the last one
    // goes. That is deliberately NOT done here yet: this factory is shared by the udev
    // and winit backends, and only udev sees libinput device events, so gating on them
    // would silently stop advertising touch under nested winit. Making it conditional
    // therefore needs a backend-specific hook plus a decision about churning
    // `wl_seat.capabilities` at runtime, which some toolkits handle poorly.
    //
    // Cost of leaving it: a machine with no touchscreen still advertises touch, so some
    // toolkits enable touch code paths / hide hover affordances. No leak — the
    // `TouchHandle` is one `Arc` held for the process lifetime.
    seat.add_touch();

    let relative_pointer_manager_state = RelativePointerManagerState::new::<I>(&display_handle);

    let pointer_constraints_state = PointerConstraintsState::new::<I>(&display_handle);

    // Touchpad gesture protocol: clients that bind it receive native pinch/swipe
    // gestures when focused (the canvas otherwise repurposes them for zoom/pan).
    let pointer_gestures_state = PointerGesturesState::new::<I>(&display_handle);

    return protocols::seat::state::Seat {
        state: seat_state,
        seat: seat,
        agent: Some(agent),
        pointer_status: CursorImageStatus::default_named(),
        relative_pointer_manager_state,
        pointer_constraints_state,
        pointer_gestures_state,
        force_cursor: None,
        unlock_restoration_location: None,
        constraints_suspended: false,
        corner_engaged: false,
        constraint_deferred: false,
        previous_focus: None,
        libseat: None,
        keyboards: Vec::new(),
        touch_devices: Vec::new(),
    };
}

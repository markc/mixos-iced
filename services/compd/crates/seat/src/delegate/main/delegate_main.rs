use smithay::backend::input::{
    AbsolutePositionEvent, ButtonState, InputBackend, InputEvent, KeyboardKeyEvent,
    PointerButtonEvent, Switch, SwitchState, SwitchToggleEvent,
};
use world::state::Loop;
use crate::pointer::input::touch;

/// Delegation of input events from the compositor seat loop
pub fn process_input_event<I: InputBackend>(_loop: &mut Loop, event: &InputEvent<I>) {
    // Track the active input modality. Touch emulation drives the pointer handlers
    // directly and never routes back through here, so these are genuine device events.
    // Mirrored into world storage because the camera / canvas systems read storage,
    // not the tracker. A real pointer also dismisses the sticky touch pane; the PEN
    // deliberately does not — it is allowed to operate the pane (touch/pen parity).
    if let Some(m) = world::seat::gesture::touch::touch::classify::<I>(event) {
        _loop.inner.touch.modality = m;
        _loop.inner.canvas_mut().input_modality = m;
        if !m.is_direct() {
            _loop.inner.touch.pane_world = None;
        }
    }
    // Touchpad swipe / pinch carry their own policy — delegated whole.
    if crate::delegate::gesture::delegate_gesture::process_input_event::<I>(
        _loop, event,
    ) {
        return;
    }
    match event {
        InputEvent::Keyboard { event, .. } => {
            crate::keyboard::input::keyboard::input_received::<I>(event, _loop);
        }
        InputEvent::PointerMotionAbsolute { event, .. } => {
            crate::pointer::input::motion::absolute::<I>(event, _loop)
        }
        InputEvent::PointerButton { event, .. } => {
            crate::pointer::input::button::button::<I>(event, _loop);
        }
        InputEvent::PointerAxis { event, .. } => {
            crate::pointer::input::axis::axis::<I>(event, _loop);
        }

        InputEvent::PointerMotion { event, .. } => {
            crate::pointer::input::motion::relative::<I>(event, _loop);
        }

        InputEvent::DeviceAdded { .. } => {}
        InputEvent::DeviceRemoved { .. } => {}
        // Touch: the session router forwards to a client's `wl_touch`, emulates the
        // pointer, or runs a canvas gesture — decided per sequence.
        InputEvent::TouchDown { event, .. } => touch::session::down::<I>(event, _loop),
        InputEvent::TouchMotion { event, .. } => touch::session::motion::<I>(event, _loop),
        InputEvent::TouchUp { event, .. } => touch::session::up::<I>(event, _loop),
        InputEvent::TouchCancel { event, .. } => touch::session::cancel::<I>(event, _loop),
        InputEvent::TouchFrame { event, .. } => touch::session::frame::<I>(event, _loop),
        // Pen / stylus: native zwp_tablet_v2 to a tablet-aware client, else pan the
        // canvas. Role latched at tip-down (see seat.pointer tablet::session).
        InputEvent::TabletToolProximity { event, .. } => {
            crate::pointer::input::tablet::proximity::proximity::<I>(event, _loop)
        }
        InputEvent::TabletToolAxis { event, .. } => {
            crate::pointer::input::tablet::axis::axis::<I>(event, _loop)
        }
        InputEvent::TabletToolTip { event, .. } => {
            crate::pointer::input::tablet::tip::tip::<I>(event, _loop)
        }
        InputEvent::TabletToolButton { event, .. } => {
            crate::pointer::input::tablet::button::button::<I>(event, _loop)
        }
        InputEvent::SwitchToggle { event, .. } => {
            if event.switch() == Some(Switch::Lid) {
                // libinput: switch On == lid closed.
                let lid_open = event.state() == SwitchState::Off;
                crate::lid::policy::policy::on_lid_toggle(_loop, lid_open);
            }
        }
        InputEvent::Special(_) => {}
        // Swipe / pinch already returned above via `delegate_gesture`.
        _ => {}
    }
}

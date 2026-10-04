//! Touchpad swipe / pinch arms of the main seat delegate.
//!
//! Split out of `delegate.main` so that stays a flat router: these arms carry real
//! policy (the finger-count split between a continuous canvas zoom and a discrete
//! window command), which is the only place in the delegate that decides rather than
//! forwards.

use world::state::Loop;
use smithay::backend::input::{
    GestureBeginEvent, GestureEndEvent, GesturePinchUpdateEvent, GestureSwipeUpdateEvent,
    InputBackend, InputEvent,
};

/// Handle a gesture event. Returns `false` if `event` was not a gesture, so the
/// caller can continue matching.
pub fn process_input_event<I: InputBackend>(_loop: &mut Loop, event: &InputEvent<I>) -> bool {
    match event {
        InputEvent::GestureSwipeBegin { event, .. } => _loop.inner.gesture.begin(event.fingers()),
        InputEvent::GestureSwipeUpdate { event, .. } => {
            _loop.inner.gesture.update(event.delta_x(), event.delta_y());
        }
        InputEvent::GestureSwipeEnd { event, .. } => {
            let cancelled = event.cancelled();
            let _ = cancelled;
            _loop.inner.gesture.active = false;
            // A three-finger swipe has no navigator to drive (cut with the camera
            // controls); nothing to fire.
        }
        // Two/three-finger pinch is a continuous canvas (or forwarded window) zoom; a
        // FOUR-finger pinch is a discrete window command (fit one / fit all),
        // accumulated here and dispatched to the gesture handler at end.
        InputEvent::GesturePinchBegin { event, .. } => {
            let fingers = event.fingers();
            _loop.inner.gesture.pinch_fingers = fingers;
            if fingers >= 4 {
                _loop.inner.gesture.pinch_scale = 1.0;
            } else {
                crate::pointer::input::pinch::begin::<I>(event, _loop);
            }
        }
        InputEvent::GesturePinchUpdate { event, .. } => {
            if _loop.inner.gesture.pinch_fingers >= 4 {
                _loop.inner.gesture.pinch_scale = event.scale();
            } else {
                crate::pointer::input::pinch::update::<I>(event, _loop);
            }
        }
        InputEvent::GesturePinchEnd { event, .. } => {
            if _loop.inner.gesture.pinch_fingers >= 4 {
                let scale = _loop.inner.gesture.pinch_scale;
                let _ = scale;
                _loop.inner.gesture.pinch_fingers = 0;
                // compd: a four-finger pinch framed windows through the navigator; cut.
            } else {
                crate::pointer::input::pinch::end::<I>(event, _loop);
            }
        }
        InputEvent::GestureHoldBegin { .. } | InputEvent::GestureHoldEnd { .. } => {}
        _ => return false,
    }
    true
}

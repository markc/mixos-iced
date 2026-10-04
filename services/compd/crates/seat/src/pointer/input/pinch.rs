use smithay::backend::input::{
    Event, GestureBeginEvent, GestureEndEvent, GesturePinchUpdateEvent as BackendPinchUpdate, InputBackend,
};
use smithay::input::pointer as sp;
use smithay::utils::{Logical, Point, SERIAL_COUNTER};
use world::state::Loop;
use slots::input::event::base::{InputEvent, InputFlow, PinchPhase};

/// Pinch begin: latch a fresh gesture and decide ownership ONCE. The world bus
/// (`CameraSystem`) answers `Consume` when the canvas owns the pinch (hand mode
/// or empty space → canvas zoom) or `Pass` when a focused window owns it. The
/// decision is held on the seat accumulator for the whole begin→update*→end
/// sequence so updates never flip target mid-pinch.
pub fn begin<I: InputBackend>(event: &I::GesturePinchBeginEvent, loop_: &mut Loop) {
    let fingers = event.fingers();
    let time = event.time_msec();
    loop_.inner.gesture.pinch_prev_scale = 1.0;

    let (x, y) = pointer_location(loop_);
    let to_window = route(loop_, InputEvent::PointerPinch { phase: PinchPhase::Begin, scale: 1.0, x, y })
        == InputFlow::Pass;
    loop_.inner.gesture.pinch_to_window = to_window;

    if to_window {
        let pointer = loop_.state.seat.seat.get_pointer().unwrap();
        pointer.gesture_pinch_begin(
            &mut loop_.state,
            &sp::GesturePinchBeginEvent { serial: SERIAL_COUNTER.next_serial(), time, fingers },
        );
    }
}

/// Pinch update: forward to the focused window (native protocol) or apply a
/// cursor-anchored canvas zoom. libinput reports `scale()` as an ABSOLUTE factor
/// relative to begin; the canvas wants the INCREMENTAL factor since the previous
/// update, so divide by the latched previous scale.
pub fn update<I: InputBackend>(event: &I::GesturePinchUpdateEvent, loop_: &mut Loop) {
    if loop_.inner.gesture.pinch_to_window {
        let delta = Point::<f64, Logical>::from((event.delta_x(), event.delta_y()));
        let pointer = loop_.state.seat.seat.get_pointer().unwrap();
        pointer.gesture_pinch_update(
            &mut loop_.state,
            &sp::GesturePinchUpdateEvent { time: event.time_msec(), delta, scale: event.scale(), rotation: event.rotation() },
        );
        return;
    }

    let scale = event.scale();
    let prev = loop_.inner.gesture.pinch_prev_scale;
    let factor = if prev > 0.0 { scale / prev } else { 1.0 };
    loop_.inner.gesture.pinch_prev_scale = scale;

    let (x, y) = pointer_location(loop_);
    route(loop_, InputEvent::PointerPinch { phase: PinchPhase::Update, scale: factor, x, y });
}

/// Pinch end: close out the native gesture if it was window-owned, then clear the
/// latched state. The canvas needs no end signal (each update is self-contained).
pub fn end<I: InputBackend>(event: &I::GesturePinchEndEvent, loop_: &mut Loop) {
    if loop_.inner.gesture.pinch_to_window {
        let pointer = loop_.state.seat.seat.get_pointer().unwrap();
        pointer.gesture_pinch_end(
            &mut loop_.state,
            &sp::GesturePinchEndEvent { serial: SERIAL_COUNTER.next_serial(), time: event.time_msec(), cancelled: event.cancelled() },
        );
    }
    loop_.inner.gesture.pinch_to_window = false;
    loop_.inner.gesture.pinch_prev_scale = 1.0;
}

fn pointer_location(loop_: &Loop) -> (f64, f64) {
    let loc = loop_.state.seat.seat.get_pointer().unwrap().current_location();
    (loc.x, loc.y)
}

fn route(loop_: &mut Loop, ev: InputEvent) -> InputFlow {
    crate::input::drive::drive::route(loop_, ev)
}

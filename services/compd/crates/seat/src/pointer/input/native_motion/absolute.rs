use crate::pointer::input::native_motion::dispatch;
use smithay::backend::input::{Axis, AxisSource, Event, InputBackend, PointerAxisEvent};
use smithay::input::pointer::{AxisFrame, MotionEvent, PointerHandle};
use smithay::utils::{Logical, Point, SERIAL_COUNTER};
use world::state::Loop;

pub fn input_received_normalized<I: InputBackend>(
    event: &I::PointerMotionAbsoluteEvent,
    _loop: &mut Loop,
    position_normalized: &Point<f64, Logical>,
    position_screen: &Point<f64, Logical>,
) {
    let position_normalized = position_normalized.clone().into();
    let serial = SERIAL_COUNTER.next_serial();
    let pointer = _loop.state.seat.seat.get_pointer().unwrap();
    dispatch::dispatch(
        _loop,
        event.time_msec(),
        serial,
        pointer,
        position_normalized,
        None,
        false,
    );
}

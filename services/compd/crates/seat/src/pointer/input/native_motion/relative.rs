use crate::pointer::input::native_motion::dispatch;
use smithay::backend::input::{Axis, AxisSource, Event, InputBackend, PointerAxisEvent};
use smithay::input::pointer::{AxisFrame, MotionEvent, PointerHandle};
use smithay::utils::{Logical, Physical, Point, SERIAL_COUNTER};
use world::camera::transform::translate::translate;
use world::state::Loop;
use world::surface::interface::hit;
use world::surface::interface::hit::SurfaceHit;

pub fn input_received_normalized<I: InputBackend>(
    event: &I::PointerMotionEvent,
    _loop: &mut Loop,
    position_normalized: Point<f64, Logical>,
    position_screen: &Point<f64, Logical>,
    delta: (Point<f64, Logical>, Point<f64, Logical>),
    was_constrain_locked: bool,
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
        Some(delta),
        was_constrain_locked,
    );
}

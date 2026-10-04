//! Implementation of input backend trait for types provided by `libinput`

use crate::backend::input::{
    self as backend, Axis, AxisRelativeDirection, AxisSource, InputBackend, InputEvent,
};
#[cfg(feature = "backend_session")]
use crate::backend::session::{AsErrno, Session};
use input as libinput;
use input::event;

use std::{
    io,
    os::unix::io::{AsFd, BorrowedFd},
    path::PathBuf,
    // compd: opt-in per-turn dispatch budget
    time::{Duration, Instant},
};
#[cfg(feature = "backend_session")]
use std::{os::unix::io::OwnedFd, path::Path};

use calloop::{EventSource, Interest, Mode, Poll, PostAction, Readiness, Token, TokenFactory};

use tracing::{debug_span, info, trace};

mod tablet;

/// Libinput based [`InputBackend`].
///
/// Tracks input of all devices given manually or via a udev seat to a provided libinput
/// context.
#[derive(Debug)]
pub struct LibinputInputBackend {
    context: libinput::Libinput,
    token: Option<Token>,
    span: tracing::Span,
    // compd: opt-in dispatch fairness; `None` keeps upstream's
    // unlimited drain
    dispatch_budget: Option<(usize, Duration)>,
    dispatch_pending: bool,
    dispatched_this_turn: bool,
}

impl LibinputInputBackend {
    /// Initialize a new [`LibinputInputBackend`] from a given already initialized
    /// [libinput context](libinput::Libinput).
    pub fn new(context: libinput::Libinput) -> Self {
        let span = debug_span!("backend_libinput");
        let _guard = span.enter();

        info!("Initializing a libinput backend");

        drop(_guard);
        LibinputInputBackend {
            context,
            token: None,
            span,
            // compd: unconfigured = upstream behaviour
            dispatch_budget: None,
            dispatch_pending: false,
            dispatched_this_turn: false,
        }
    }

    /// Returns a reference to the underlying libinput context
    pub fn context(&self) -> &libinput::Libinput {
        &self.context
    }

    // compd: opt-in per-turn dispatch budget
    /// Bound event routing per calloop turn, retaining FIFO order.
    ///
    /// Opt-in: a backend on which this is never called drains every queued
    /// event per dispatch, exactly as upstream does. Once set, at most
    /// `max_events` events (or `max_time`, whichever is reached first, with at
    /// least one event always routed) are delivered per turn; the rest are
    /// left in libinput's queue and a continuation turn is requested through
    /// `before_sleep`, so a sustained input flood cannot monopolise the event
    /// loop (and starve client frame callbacks / buffer commits).
    ///
    /// The time limit is cooperative: an individual event callback and
    /// libinput's dispatch cannot be preempted. A reached limit schedules
    /// another turn even if libinput has already consumed all kernel fd
    /// readiness.
    pub fn set_dispatch_budget(&mut self, max_events: usize, max_time: Duration) {
        assert!(max_events > 0 && !max_time.is_zero());
        self.dispatch_budget = Some((max_events, max_time));
    }
}

impl backend::Device for libinput::Device {
    fn id(&self) -> String {
        self.sysname().into()
    }

    fn name(&self) -> String {
        self.name().into()
    }

    fn has_capability(&self, capability: backend::DeviceCapability) -> bool {
        libinput::Device::has_capability(self, capability.into())
    }

    fn usb_id(&self) -> Option<(u32, u32)> {
        Some((
            libinput::Device::id_product(self),
            libinput::Device::id_vendor(self),
        ))
    }

    fn syspath(&self) -> Option<PathBuf> {
        #[cfg(feature = "udev")]
        return unsafe { libinput::Device::udev_device(self) }.map(|d| d.syspath().to_owned());

        #[cfg(not(feature = "udev"))]
        None
    }
}

impl From<backend::DeviceCapability> for libinput::DeviceCapability {
    #[inline]
    fn from(other: backend::DeviceCapability) -> libinput::DeviceCapability {
        match other {
            backend::DeviceCapability::Gesture => libinput::DeviceCapability::Gesture,
            backend::DeviceCapability::Keyboard => libinput::DeviceCapability::Keyboard,
            backend::DeviceCapability::Pointer => libinput::DeviceCapability::Pointer,
            backend::DeviceCapability::Switch => libinput::DeviceCapability::Switch,
            backend::DeviceCapability::TabletPad => libinput::DeviceCapability::TabletPad,
            backend::DeviceCapability::TabletTool => libinput::DeviceCapability::TabletTool,
            backend::DeviceCapability::Touch => libinput::DeviceCapability::Touch,
        }
    }
}

impl backend::Event<LibinputInputBackend> for event::keyboard::KeyboardKeyEvent {
    fn time(&self) -> u64 {
        event::keyboard::KeyboardEventTrait::time_usec(self)
    }

    fn device(&self) -> libinput::Device {
        event::EventTrait::device(self)
    }
}

impl backend::KeyboardKeyEvent<LibinputInputBackend> for event::keyboard::KeyboardKeyEvent {
    fn key_code(&self) -> backend::Keycode {
        use input::event::keyboard::KeyboardEventTrait;
        (self.key() + 8).into()
    }

    fn state(&self) -> backend::KeyState {
        use input::event::keyboard::KeyboardEventTrait;
        self.key_state().into()
    }

    fn count(&self) -> u32 {
        self.seat_key_count()
    }
}

impl backend::Event<LibinputInputBackend> for event::switch::SwitchToggleEvent {
    fn time(&self) -> u64 {
        event::switch::SwitchEventTrait::time_usec(self)
    }

    fn device(&self) -> <LibinputInputBackend as InputBackend>::Device {
        event::EventTrait::device(self)
    }
}

impl backend::SwitchToggleEvent<LibinputInputBackend> for event::switch::SwitchToggleEvent {
    fn switch(&self) -> Option<backend::Switch> {
        event::switch::SwitchToggleEvent::switch(self).and_then(|switch| {
            Some(match switch {
                event::switch::Switch::Lid => backend::Switch::Lid,
                event::switch::Switch::TabletMode => backend::Switch::TabletMode,
                _ => return None,
            })
        })
    }

    fn state(&self) -> backend::SwitchState {
        match event::switch::SwitchToggleEvent::switch_state(self) {
            event::switch::SwitchState::Off => backend::SwitchState::Off,
            event::switch::SwitchState::On => backend::SwitchState::On,
        }
    }
}

/// Generic pointer scroll event from libinput
#[derive(Debug, PartialEq, Eq, Hash)]
pub enum PointerScrollAxis {
    /// Scroll event from pointer wheel
    Wheel(event::pointer::PointerScrollWheelEvent),
    /// Scroll event from pointer finger
    Finger(event::pointer::PointerScrollFingerEvent),
    /// Continuous scroll event
    Continuous(event::pointer::PointerScrollContinuousEvent),
}

impl PointerScrollAxis {
    fn has_axis(&self, axis: event::pointer::Axis) -> bool {
        use input::event::pointer::PointerScrollEvent;
        match self {
            Self::Wheel(evt) => evt.has_axis(axis),
            Self::Finger(evt) => evt.has_axis(axis),
            Self::Continuous(evt) => evt.has_axis(axis),
        }
    }
}

impl backend::Event<LibinputInputBackend> for PointerScrollAxis {
    fn time(&self) -> u64 {
        match self {
            Self::Wheel(evt) => event::pointer::PointerEventTrait::time_usec(evt),
            Self::Finger(evt) => event::pointer::PointerEventTrait::time_usec(evt),
            Self::Continuous(evt) => event::pointer::PointerEventTrait::time_usec(evt),
        }
    }

    fn device(&self) -> libinput::Device {
        match self {
            Self::Wheel(evt) => event::EventTrait::device(evt),
            Self::Finger(evt) => event::EventTrait::device(evt),
            Self::Continuous(evt) => event::EventTrait::device(evt),
        }
    }
}

impl backend::PointerAxisEvent<LibinputInputBackend> for PointerScrollAxis {
    fn amount(&self, axis: Axis) -> Option<f64> {
        use input::event::pointer::PointerScrollEvent;
        let axis = axis.into();
        if self.has_axis(axis) {
            Some(match self {
                Self::Wheel(evt) => evt.scroll_value(axis),
                Self::Finger(evt) => evt.scroll_value(axis),
                Self::Continuous(evt) => evt.scroll_value(axis),
            })
        } else {
            None
        }
    }

    fn amount_v120(&self, axis: Axis) -> Option<f64> {
        let axis = axis.into();
        if self.has_axis(axis) {
            match self {
                Self::Wheel(evt) => Some(evt.scroll_value_v120(axis)),
                Self::Finger(_evt) => None,
                Self::Continuous(_evt) => None,
            }
        } else {
            None
        }
    }

    fn source(&self) -> AxisSource {
        match self {
            Self::Wheel(_) => AxisSource::Wheel,
            Self::Finger(_) => AxisSource::Finger,
            Self::Continuous(_) => AxisSource::Continuous,
        }
    }

    fn relative_direction(&self, _axis: Axis) -> backend::AxisRelativeDirection {
        let device = backend::Event::<LibinputInputBackend>::device(self);
        if device.config_scroll_natural_scroll_enabled() {
            AxisRelativeDirection::Inverted
        } else {
            AxisRelativeDirection::Identical
        }
    }
}

impl backend::Event<LibinputInputBackend> for event::pointer::PointerButtonEvent {
    fn time(&self) -> u64 {
        event::pointer::PointerEventTrait::time_usec(self)
    }

    fn device(&self) -> libinput::Device {
        event::EventTrait::device(self)
    }
}

impl backend::PointerButtonEvent<LibinputInputBackend> for event::pointer::PointerButtonEvent {
    fn button_code(&self) -> u32 {
        self.button()
    }

    fn state(&self) -> backend::ButtonState {
        self.button_state().into()
    }
}

impl backend::Event<LibinputInputBackend> for event::pointer::PointerMotionEvent {
    fn time(&self) -> u64 {
        event::pointer::PointerEventTrait::time_usec(self)
    }

    fn device(&self) -> libinput::Device {
        event::EventTrait::device(self)
    }
}

impl backend::PointerMotionEvent<LibinputInputBackend> for event::pointer::PointerMotionEvent {
    fn delta_x(&self) -> f64 {
        self.dx()
    }

    fn delta_y(&self) -> f64 {
        self.dy()
    }

    fn delta_x_unaccel(&self) -> f64 {
        self.dx_unaccelerated()
    }

    fn delta_y_unaccel(&self) -> f64 {
        self.dy_unaccelerated()
    }
}

impl backend::Event<LibinputInputBackend> for event::pointer::PointerMotionAbsoluteEvent {
    fn time(&self) -> u64 {
        event::pointer::PointerEventTrait::time_usec(self)
    }

    fn device(&self) -> libinput::Device {
        event::EventTrait::device(self)
    }
}

impl backend::PointerMotionAbsoluteEvent<LibinputInputBackend>
    for event::pointer::PointerMotionAbsoluteEvent
{
}

impl backend::AbsolutePositionEvent<LibinputInputBackend> for event::pointer::PointerMotionAbsoluteEvent {
    fn x(&self) -> f64 {
        self.absolute_x()
    }

    fn y(&self) -> f64 {
        self.absolute_y()
    }

    fn x_transformed(&self, width: i32) -> f64 {
        self.absolute_x_transformed(width as u32)
    }

    fn y_transformed(&self, height: i32) -> f64 {
        self.absolute_y_transformed(height as u32)
    }
}

impl<T> backend::GestureBeginEvent<LibinputInputBackend> for T
where
    T: event::gesture::GestureEventTrait + backend::Event<LibinputInputBackend>,
{
    fn fingers(&self) -> u32 {
        self.finger_count() as u32
    }
}

impl<T> backend::GestureEndEvent<LibinputInputBackend> for T
where
    T: event::gesture::GestureEndEvent + backend::Event<LibinputInputBackend>,
{
    fn cancelled(&self) -> bool {
        self.cancelled()
    }
}

impl backend::Event<LibinputInputBackend> for event::gesture::GestureSwipeBeginEvent {
    fn time(&self) -> u64 {
        event::gesture::GestureEventTrait::time_usec(self)
    }

    fn device(&self) -> libinput::Device {
        event::EventTrait::device(self)
    }
}

impl backend::GestureSwipeBeginEvent<LibinputInputBackend> for event::gesture::GestureSwipeBeginEvent {}

impl backend::Event<LibinputInputBackend> for event::gesture::GestureSwipeUpdateEvent {
    fn time(&self) -> u64 {
        event::gesture::GestureEventTrait::time_usec(self)
    }

    fn device(&self) -> libinput::Device {
        event::EventTrait::device(self)
    }
}

impl backend::GestureSwipeUpdateEvent<LibinputInputBackend> for event::gesture::GestureSwipeUpdateEvent {
    fn delta_x(&self) -> f64 {
        event::gesture::GestureEventCoordinates::dx(self)
    }

    fn delta_y(&self) -> f64 {
        event::gesture::GestureEventCoordinates::dy(self)
    }
}

impl backend::Event<LibinputInputBackend> for event::gesture::GestureSwipeEndEvent {
    fn time(&self) -> u64 {
        event::gesture::GestureEventTrait::time_usec(self)
    }

    fn device(&self) -> libinput::Device {
        event::EventTrait::device(self)
    }
}

impl backend::GestureSwipeEndEvent<LibinputInputBackend> for event::gesture::GestureSwipeEndEvent {}

impl backend::Event<LibinputInputBackend> for event::gesture::GesturePinchBeginEvent {
    fn time(&self) -> u64 {
        event::gesture::GestureEventTrait::time_usec(self)
    }

    fn device(&self) -> libinput::Device {
        event::EventTrait::device(self)
    }
}

impl backend::GesturePinchBeginEvent<LibinputInputBackend> for event::gesture::GesturePinchBeginEvent {}

impl backend::Event<LibinputInputBackend> for event::gesture::GesturePinchUpdateEvent {
    fn time(&self) -> u64 {
        event::gesture::GestureEventTrait::time_usec(self)
    }

    fn device(&self) -> libinput::Device {
        event::EventTrait::device(self)
    }
}

impl backend::GesturePinchUpdateEvent<LibinputInputBackend> for event::gesture::GesturePinchUpdateEvent {
    fn delta_x(&self) -> f64 {
        event::gesture::GestureEventCoordinates::dx(self)
    }

    fn delta_y(&self) -> f64 {
        event::gesture::GestureEventCoordinates::dy(self)
    }

    fn scale(&self) -> f64 {
        event::gesture::GesturePinchEventTrait::scale(self)
    }

    fn rotation(&self) -> f64 {
        self.angle_delta()
    }
}

impl backend::Event<LibinputInputBackend> for event::gesture::GesturePinchEndEvent {
    fn time(&self) -> u64 {
        event::gesture::GestureEventTrait::time_usec(self)
    }

    fn device(&self) -> libinput::Device {
        event::EventTrait::device(self)
    }
}

impl backend::GesturePinchEndEvent<LibinputInputBackend> for event::gesture::GesturePinchEndEvent {}

impl backend::Event<LibinputInputBackend> for event::gesture::GestureHoldBeginEvent {
    fn time(&self) -> u64 {
        event::gesture::GestureEventTrait::time_usec(self)
    }

    fn device(&self) -> libinput::Device {
        event::EventTrait::device(self)
    }
}

impl backend::GestureHoldBeginEvent<LibinputInputBackend> for event::gesture::GestureHoldBeginEvent {}

impl backend::Event<LibinputInputBackend> for event::gesture::GestureHoldEndEvent {
    fn time(&self) -> u64 {
        event::gesture::GestureEventTrait::time_usec(self)
    }

    fn device(&self) -> libinput::Device {
        event::EventTrait::device(self)
    }
}

impl backend::GestureHoldEndEvent<LibinputInputBackend> for event::gesture::GestureHoldEndEvent {}

impl backend::Event<LibinputInputBackend> for event::touch::TouchDownEvent {
    fn time(&self) -> u64 {
        event::touch::TouchEventTrait::time_usec(self)
    }

    fn device(&self) -> libinput::Device {
        event::EventTrait::device(self)
    }
}

impl backend::TouchDownEvent<LibinputInputBackend> for event::touch::TouchDownEvent {}

// compd: touch slots are SEAT-wide, not device-local.
//
// Upstream forwarded libinput's `slot()`: the DEVICE's MT slot, or `None` for a
// single-touch device (`wl_touch` id -1). The seat keys its touch focus map on that
// slot and puts it on the wire, so two touch devices on one seat collide — two
// single-touch panels both report `None`, two multitouch panels both start at
// hardware slot 0 — and the second contact overwrites the first's focus and reuses
// an id the client still holds. libinput's `seat_slot()` is allocated across the
// seat and unique among simultaneous contacts, which is exactly the uniqueness
// `wl_touch` ids need; it is always assigned, hence `Some(…)`. All four touch event
// impls below use it: a single one left on `slot()` would key that event class
// differently from the rest (the guard below pins all four).
impl backend::TouchEvent<LibinputInputBackend> for event::touch::TouchDownEvent {
    fn slot(&self) -> backend::TouchSlot {
        Some(event::touch::TouchEventSlot::seat_slot(self)).into()
    }
}

impl backend::AbsolutePositionEvent<LibinputInputBackend> for event::touch::TouchDownEvent {
    fn x(&self) -> f64 {
        event::touch::TouchEventPosition::x(self)
    }

    fn y(&self) -> f64 {
        event::touch::TouchEventPosition::y(self)
    }

    fn x_transformed(&self, width: i32) -> f64 {
        event::touch::TouchEventPosition::x_transformed(self, width as u32)
    }

    fn y_transformed(&self, height: i32) -> f64 {
        event::touch::TouchEventPosition::y_transformed(self, height as u32)
    }
}

impl backend::Event<LibinputInputBackend> for event::touch::TouchMotionEvent {
    fn time(&self) -> u64 {
        event::touch::TouchEventTrait::time_usec(self)
    }

    fn device(&self) -> libinput::Device {
        event::EventTrait::device(self)
    }
}

impl backend::TouchMotionEvent<LibinputInputBackend> for event::touch::TouchMotionEvent {}

impl backend::TouchEvent<LibinputInputBackend> for event::touch::TouchMotionEvent {
    fn slot(&self) -> backend::TouchSlot {
        Some(event::touch::TouchEventSlot::seat_slot(self)).into()
    }
}

impl backend::AbsolutePositionEvent<LibinputInputBackend> for event::touch::TouchMotionEvent {
    fn x(&self) -> f64 {
        event::touch::TouchEventPosition::x(self)
    }

    fn y(&self) -> f64 {
        event::touch::TouchEventPosition::y(self)
    }

    fn x_transformed(&self, width: i32) -> f64 {
        event::touch::TouchEventPosition::x_transformed(self, width as u32)
    }

    fn y_transformed(&self, height: i32) -> f64 {
        event::touch::TouchEventPosition::y_transformed(self, height as u32)
    }
}

impl backend::Event<LibinputInputBackend> for event::touch::TouchUpEvent {
    fn time(&self) -> u64 {
        event::touch::TouchEventTrait::time_usec(self)
    }

    fn device(&self) -> libinput::Device {
        event::EventTrait::device(self)
    }
}

impl backend::TouchUpEvent<LibinputInputBackend> for event::touch::TouchUpEvent {}

impl backend::TouchEvent<LibinputInputBackend> for event::touch::TouchUpEvent {
    fn slot(&self) -> backend::TouchSlot {
        Some(event::touch::TouchEventSlot::seat_slot(self)).into()
    }
}

impl backend::Event<LibinputInputBackend> for event::touch::TouchCancelEvent {
    fn time(&self) -> u64 {
        event::touch::TouchEventTrait::time_usec(self)
    }

    fn device(&self) -> libinput::Device {
        event::EventTrait::device(self)
    }
}

impl backend::TouchCancelEvent<LibinputInputBackend> for event::touch::TouchCancelEvent {}

impl backend::TouchEvent<LibinputInputBackend> for event::touch::TouchCancelEvent {
    fn slot(&self) -> backend::TouchSlot {
        Some(event::touch::TouchEventSlot::seat_slot(self)).into()
    }
}

impl backend::Event<LibinputInputBackend> for event::touch::TouchFrameEvent {
    fn time(&self) -> u64 {
        event::touch::TouchEventTrait::time_usec(self)
    }

    fn device(&self) -> libinput::Device {
        event::EventTrait::device(self)
    }
}

impl backend::TouchFrameEvent<LibinputInputBackend> for event::touch::TouchFrameEvent {}

impl InputBackend for LibinputInputBackend {
    type Device = libinput::Device;
    type KeyboardKeyEvent = event::keyboard::KeyboardKeyEvent;
    type PointerAxisEvent = PointerScrollAxis;
    type PointerButtonEvent = event::pointer::PointerButtonEvent;
    type PointerMotionEvent = event::pointer::PointerMotionEvent;
    type PointerMotionAbsoluteEvent = event::pointer::PointerMotionAbsoluteEvent;

    type GestureSwipeBeginEvent = event::gesture::GestureSwipeBeginEvent;
    type GestureSwipeUpdateEvent = event::gesture::GestureSwipeUpdateEvent;
    type GestureSwipeEndEvent = event::gesture::GestureSwipeEndEvent;
    type GesturePinchBeginEvent = event::gesture::GesturePinchBeginEvent;
    type GesturePinchUpdateEvent = event::gesture::GesturePinchUpdateEvent;
    type GesturePinchEndEvent = event::gesture::GesturePinchEndEvent;
    type GestureHoldBeginEvent = event::gesture::GestureHoldBeginEvent;
    type GestureHoldEndEvent = event::gesture::GestureHoldEndEvent;

    type TouchDownEvent = event::touch::TouchDownEvent;
    type TouchUpEvent = event::touch::TouchUpEvent;
    type TouchMotionEvent = event::touch::TouchMotionEvent;
    type TouchCancelEvent = event::touch::TouchCancelEvent;
    type TouchFrameEvent = event::touch::TouchFrameEvent;
    type TabletToolAxisEvent = event::tablet_tool::TabletToolAxisEvent;
    type TabletToolProximityEvent = event::tablet_tool::TabletToolProximityEvent;
    type TabletToolTipEvent = event::tablet_tool::TabletToolTipEvent;
    type TabletToolButtonEvent = event::tablet_tool::TabletToolButtonEvent;

    type SwitchToggleEvent = event::switch::SwitchToggleEvent;

    // local patch: surface tablet-pad events (buttons/ring/strip) — smithay models
    // tablet tools but not pads. Carrying the raw libinput pad event through the
    // existing `SpecialEvent` channel keeps the `InputBackend` trait (and every
    // other backend impl) untouched; the compositor consumes it in its libinput
    // closure and speaks zwp_tablet_pad_v2 itself.
    type SpecialEvent = event::tablet_pad::TabletPadEvent;
}

impl From<event::keyboard::KeyState> for backend::KeyState {
    #[inline]
    fn from(libinput: event::keyboard::KeyState) -> Self {
        match libinput {
            event::keyboard::KeyState::Pressed => backend::KeyState::Pressed,
            event::keyboard::KeyState::Released => backend::KeyState::Released,
        }
    }
}

impl From<event::pointer::Axis> for backend::Axis {
    #[inline]
    fn from(libinput: event::pointer::Axis) -> Self {
        match libinput {
            event::pointer::Axis::Vertical => backend::Axis::Vertical,
            event::pointer::Axis::Horizontal => backend::Axis::Horizontal,
        }
    }
}

impl From<backend::Axis> for event::pointer::Axis {
    #[inline]
    fn from(axis: backend::Axis) -> Self {
        match axis {
            backend::Axis::Vertical => event::pointer::Axis::Vertical,
            backend::Axis::Horizontal => event::pointer::Axis::Horizontal,
        }
    }
}

impl From<event::pointer::ButtonState> for backend::ButtonState {
    #[inline]
    fn from(libinput: event::pointer::ButtonState) -> Self {
        match libinput {
            event::pointer::ButtonState::Pressed => backend::ButtonState::Pressed,
            event::pointer::ButtonState::Released => backend::ButtonState::Released,
        }
    }
}

impl From<crate::input::keyboard::LedState> for libinput::Led {
    #[inline]
    fn from(value: crate::input::keyboard::LedState) -> Self {
        let mut leds = libinput::Led::empty();
        if value.num.unwrap_or_default() {
            leds |= libinput::Led::NUMLOCK;
        }
        if value.caps.unwrap_or_default() {
            leds |= libinput::Led::CAPSLOCK;
        }
        if value.scroll.unwrap_or_default() {
            leds |= libinput::Led::SCROLLLOCK;
        }
        leds
    }
}

/// Wrapper for types implementing the [`Session`] trait to provide
/// a [`libinput::LibinputInterface`] implementation.
#[cfg(feature = "backend_session")]
#[derive(Debug)]
pub struct LibinputSessionInterface<S: Session>(S);

#[cfg(feature = "backend_session")]
impl<S: Session> From<S> for LibinputSessionInterface<S> {
    #[inline]
    fn from(session: S) -> LibinputSessionInterface<S> {
        LibinputSessionInterface(session)
    }
}

#[cfg(feature = "backend_session")]
impl<S: Session> libinput::LibinputInterface for LibinputSessionInterface<S> {
    fn open_restricted(&mut self, path: &Path, flags: i32) -> Result<OwnedFd, i32> {
        use rustix::fs::OFlags;
        self.0
            .open(path, OFlags::from_bits_truncate(flags as u32))
            .map_err(|err| err.as_errno().unwrap_or(1 /*Use EPERM by default*/))
    }

    fn close_restricted(&mut self, fd: OwnedFd) {
        let _ = self.0.close(fd);
    }
}

impl AsFd for LibinputInputBackend {
    #[inline]
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.context.as_fd()
    }
}

// compd: budget check BEFORE advancing the iterator, so
// yielding never consumes (and loses) the first event of the next batch. Generic
// so the drain boundary is testable without a privileged input device
fn next_dispatch_event<I: Iterator>(
    events: &mut I,
    budget: Option<(usize, Duration)>,
    processed: &mut usize,
    started: Instant,
    pending: &mut bool,
) -> Option<I::Item> {
    if budget
        .is_some_and(|(count, time)| *processed >= count || (*processed > 0 && started.elapsed() >= time))
    {
        // An exactly full final batch costs one harmless empty turn.
        *pending = true;
        return None;
    }
    let event = events.next()?;
    *processed += 1;
    Some(event)
}

impl EventSource for LibinputInputBackend {
    type Event = InputEvent<LibinputInputBackend>;
    type Metadata = ();
    type Ret = ();
    type Error = io::Error;

    // compd: continuation via synthetic readiness when a
    // budgeted batch yielded
    const NEEDS_EXTRA_LIFECYCLE_EVENTS: bool = true;

    fn before_sleep(&mut self) -> calloop::Result<Option<(Readiness, Token)>> {
        Ok(self.token.filter(|_| self.dispatch_pending).map(|token| {
            (
                Readiness {
                    readable: true,
                    ..Readiness::EMPTY
                },
                token,
            )
        }))
    }

    fn before_handle_events(&mut self, _: calloop::EventIterator<'_>) {
        self.dispatched_this_turn = false;
    }

    #[profiling::function]
    fn process_events<F>(&mut self, _: Readiness, token: Token, mut callback: F) -> io::Result<PostAction>
    where
        F: FnMut(Self::Event, &mut ()) -> Self::Ret,
    {
        if Some(token) == self.token {
            // compd: synthetic readiness and fd readiness can
            // both arrive in one poll; with a budget they share one batch rather
            // than multiplying the turn's budget
            if self.dispatch_budget.is_some() && self.dispatched_this_turn {
                return Ok(PostAction::Continue);
            }
            self.dispatched_this_turn = true;
            self.dispatch_pending = false;
            let started = Instant::now();
            let _guard = self.span.enter();
            self.context.dispatch()?;

            // compd: bounded drain (unbounded when no budget)
            let mut processed = 0;
            while let Some(event) = next_dispatch_event(
                &mut self.context,
                self.dispatch_budget,
                &mut processed,
                started,
                &mut self.dispatch_pending,
            ) {
                match event {
                    libinput::Event::Device(device_event) => match device_event {
                        event::DeviceEvent::Added(device_added_event) => {
                            let added = event::EventTrait::device(&device_added_event);

                            info!("New device {:?}", added.sysname(),);

                            callback(InputEvent::DeviceAdded { device: added }, &mut ());
                        }
                        event::DeviceEvent::Removed(device_removed_event) => {
                            let removed = event::EventTrait::device(&device_removed_event);

                            info!("Removed device {:?}", removed.sysname(),);

                            callback(InputEvent::DeviceRemoved { device: removed }, &mut ());
                        }
                        _ => {
                            trace!("Unknown libinput device event");
                        }
                    },
                    libinput::Event::Touch(touch_event) => match touch_event {
                        event::TouchEvent::Down(down_event) => {
                            callback(InputEvent::TouchDown { event: down_event }, &mut ());
                        }
                        event::TouchEvent::Motion(motion_event) => {
                            callback(InputEvent::TouchMotion { event: motion_event }, &mut ());
                        }
                        event::TouchEvent::Up(up_event) => {
                            callback(InputEvent::TouchUp { event: up_event }, &mut ());
                        }
                        event::TouchEvent::Cancel(cancel_event) => {
                            callback(InputEvent::TouchCancel { event: cancel_event }, &mut ());
                        }
                        event::TouchEvent::Frame(frame_event) => {
                            callback(InputEvent::TouchFrame { event: frame_event }, &mut ());
                        }
                        _ => {
                            trace!("Unknown libinput touch event");
                        }
                    },
                    libinput::Event::Keyboard(keyboard_event) => match keyboard_event {
                        event::KeyboardEvent::Key(key_event) => {
                            callback(InputEvent::Keyboard { event: key_event }, &mut ());
                        }
                        _ => {
                            trace!("Unknown libinput keyboard event");
                        }
                    },
                    libinput::Event::Pointer(pointer_event) => match pointer_event {
                        event::PointerEvent::Motion(motion_event) => {
                            callback(InputEvent::PointerMotion { event: motion_event }, &mut ());
                        }
                        event::PointerEvent::MotionAbsolute(motion_abs_event) => {
                            callback(
                                InputEvent::PointerMotionAbsolute {
                                    event: motion_abs_event,
                                },
                                &mut (),
                            );
                        }
                        event::PointerEvent::ScrollWheel(scroll_event) => {
                            callback(
                                InputEvent::PointerAxis {
                                    event: PointerScrollAxis::Wheel(scroll_event),
                                },
                                &mut (),
                            );
                        }
                        event::PointerEvent::ScrollFinger(scroll_event) => {
                            callback(
                                InputEvent::PointerAxis {
                                    event: PointerScrollAxis::Finger(scroll_event),
                                },
                                &mut (),
                            );
                        }
                        event::PointerEvent::ScrollContinuous(scroll_event) => {
                            callback(
                                InputEvent::PointerAxis {
                                    event: PointerScrollAxis::Continuous(scroll_event),
                                },
                                &mut (),
                            );
                        }
                        event::PointerEvent::Button(button_event) => {
                            callback(InputEvent::PointerButton { event: button_event }, &mut ());
                        }
                        _ => {
                            trace!("Unknown libinput pointer event");
                        }
                    },
                    libinput::Event::Gesture(gesture_event) => match gesture_event {
                        event::GestureEvent::Swipe(event::gesture::GestureSwipeEvent::Begin(event)) => {
                            callback(InputEvent::GestureSwipeBegin { event }, &mut ());
                        }
                        event::GestureEvent::Swipe(event::gesture::GestureSwipeEvent::Update(event)) => {
                            callback(InputEvent::GestureSwipeUpdate { event }, &mut ());
                        }
                        event::GestureEvent::Swipe(event::gesture::GestureSwipeEvent::End(event)) => {
                            callback(InputEvent::GestureSwipeEnd { event }, &mut ());
                        }
                        event::GestureEvent::Pinch(event::gesture::GesturePinchEvent::Begin(event)) => {
                            callback(InputEvent::GesturePinchBegin { event }, &mut ());
                        }
                        event::GestureEvent::Pinch(event::gesture::GesturePinchEvent::Update(event)) => {
                            callback(InputEvent::GesturePinchUpdate { event }, &mut ());
                        }
                        event::GestureEvent::Pinch(event::gesture::GesturePinchEvent::End(event)) => {
                            callback(InputEvent::GesturePinchEnd { event }, &mut ());
                        }
                        event::GestureEvent::Hold(event::gesture::GestureHoldEvent::Begin(event)) => {
                            callback(InputEvent::GestureHoldBegin { event }, &mut ());
                        }
                        event::GestureEvent::Hold(event::gesture::GestureHoldEvent::End(event)) => {
                            callback(InputEvent::GestureHoldEnd { event }, &mut ());
                        }
                        _ => {
                            trace!("Unknown libinput gesture event");
                        }
                    },
                    libinput::Event::Tablet(tablet_event) => match tablet_event {
                        event::TabletToolEvent::Axis(event) => {
                            callback(InputEvent::TabletToolAxis { event }, &mut ());
                        }
                        event::TabletToolEvent::Proximity(event) => {
                            callback(InputEvent::TabletToolProximity { event }, &mut ());
                        }
                        event::TabletToolEvent::Tip(event) => {
                            callback(InputEvent::TabletToolTip { event }, &mut ());
                        }
                        event::TabletToolEvent::Button(event) => {
                            callback(InputEvent::TabletToolButton { event }, &mut ());
                        }
                        _ => {
                            trace!("Unknown libinput tablet event");
                        }
                    },
                    libinput::Event::Switch(switch_event) => match switch_event {
                        event::SwitchEvent::Toggle(event) => {
                            callback(InputEvent::SwitchToggle { event }, &mut ());
                        }
                        _ => {
                            trace!("Unknown libinput switch event");
                        }
                    },
                    // local patch: forward tablet-pad events through SpecialEvent.
                    libinput::Event::TabletPad(pad_event) => {
                        callback(InputEvent::Special(pad_event), &mut ());
                    }
                    _ => {} //FIXME: What to do with the rest.
                }
            }
        }

        Ok(PostAction::Continue)
    }

    fn register(&mut self, poll: &mut Poll, factory: &mut TokenFactory) -> calloop::Result<()> {
        self.token = Some(factory.token());
        // Safety: the FD cannot be closed without removing the LibinputInputBackend from the event loop
        unsafe { poll.register(self.as_fd(), Interest::READ, Mode::Level, self.token.unwrap()) }
    }

    fn reregister(&mut self, poll: &mut Poll, factory: &mut TokenFactory) -> calloop::Result<()> {
        self.token = Some(factory.token());
        poll.reregister(self.as_fd(), Interest::READ, Mode::Level, self.token.unwrap())
    }

    fn unregister(&mut self, poll: &mut Poll) -> calloop::Result<()> {
        self.token = None;
        poll.unregister(self.as_fd())
    }
}

// compd: guards for the opt-in dispatch budget
#[cfg(test)]
mod fairness_tests {
    use super::*;

    #[test]
    fn count_budget_yields_without_consuming_next_event() {
        let mut events = 0..48;
        let mut routed = Vec::new();
        for turn in 0..4 {
            let mut pending = false;
            let mut processed = 0;
            let started = Instant::now();
            while let Some(event) = next_dispatch_event(
                &mut events,
                Some((16, Duration::from_secs(60))),
                &mut processed,
                started,
                &mut pending,
            ) {
                routed.push(event);
            }
            assert_eq!(processed, if turn < 3 { 16 } else { 0 });
            assert_eq!(pending, turn < 3);
        }
        assert_eq!(routed, (0..48).collect::<Vec<_>>());
    }

    #[test]
    fn no_budget_drains_everything_in_one_turn() {
        // The patch is opt-in: an unconfigured source keeps upstream's unlimited drain.
        let mut events = 0..1000;
        let mut pending = false;
        let mut processed = 0;
        let started = Instant::now() - Duration::from_secs(10);
        let mut routed = 0;
        while next_dispatch_event(&mut events, None, &mut processed, started, &mut pending).is_some() {
            routed += 1;
        }
        assert_eq!(routed, 1000);
        assert!(!pending);
    }

    #[test]
    fn elapsed_budget_yields_but_always_makes_progress() {
        let mut events = 0..3;
        let mut pending = false;
        let mut processed = 0;
        let started = Instant::now() - Duration::from_millis(10);
        let budget = Some((16, Duration::from_millis(2)));
        assert_eq!(
            next_dispatch_event(&mut events, budget, &mut processed, started, &mut pending),
            Some(0)
        );
        assert_eq!(
            next_dispatch_event(&mut events, budget, &mut processed, started, &mut pending),
            None
        );
        assert!(pending);
        assert_eq!(events.next(), Some(1));
    }

    struct NoDevices;

    impl libinput::LibinputInterface for NoDevices {
        fn open_restricted(&mut self, _: &std::path::Path, _: i32) -> Result<std::os::fd::OwnedFd, i32> {
            panic!("test must not open input devices")
        }
        fn close_restricted(&mut self, _: std::os::fd::OwnedFd) {}
    }

    #[test]
    fn continuation_wakes_without_fd_readiness_and_duplicate_dispatch_yields() {
        let mut event_loop = calloop::EventLoop::<usize>::try_new().unwrap();
        let mut backend = LibinputInputBackend::new(libinput::Libinput::new_from_path(NoDevices));
        backend.set_dispatch_budget(16, Duration::from_millis(2));
        // Simulate a full final batch. The real source must clear this in a
        // continuation even though there are no devices or readable input fd.
        backend.dispatch_pending = true;
        let source = calloop::Dispatcher::new(backend, |_, _, _: &mut usize| panic!("no devices"));
        event_loop.handle().register_dispatcher(source.clone()).unwrap();
        let (ping, ping_source) = calloop::ping::make_ping().unwrap();
        event_loop
            .handle()
            .insert_source(ping_source, |_, _, turns| *turns += 1)
            .unwrap();
        ping.ping();
        let mut turns = 0;
        event_loop
            .dispatch(Some(Duration::from_secs(1)), &mut turns)
            .unwrap();
        assert_eq!(turns, 1);
        assert!(!source.as_source_ref().dispatch_pending);
        {
            let mut backend = source.as_source_mut();
            assert!(backend.dispatched_this_turn);
            backend.dispatch_pending = true;
            let token = backend.token.unwrap();
            backend
                .process_events(
                    Readiness {
                        readable: true,
                        ..Readiness::EMPTY
                    },
                    token,
                    |_, _| panic!("duplicate batch"),
                )
                .unwrap();
            assert!(
                backend.dispatch_pending,
                "duplicate readiness must not drain again"
            );
        }
        // No ping now: only synthetic readiness can promptly run this turn.
        let started = Instant::now();
        event_loop
            .dispatch(Some(Duration::from_secs(1)), &mut turns)
            .unwrap();
        assert!(started.elapsed() < Duration::from_millis(500));
        assert!(!source.as_source_ref().dispatch_pending);
        assert!(source.as_source_mut().before_sleep().unwrap().is_none());
    }
}

// compd guard for the seat-slot patch. A source guard: the
// libinput backend cannot be instantiated without a real device (a live proof
// needs two touch devices).
#[cfg(test)]
mod seat_slot_guard {
    #[test]
    fn every_touch_event_uses_the_seat_slot() {
        let src = include_str!("mod.rs");
        let code = &src[..src.find("mod seat_slot_guard").unwrap()];
        let seat = code.matches("TouchEventSlot::seat_slot(self)").count();
        let device = code.matches("TouchEventSlot::slot(self)").count();
        assert_eq!(seat, 4, "down, motion, up and cancel must all use seat_slot()");
        assert_eq!(device, 0, "no touch event may key on the device-local slot()");
    }
}

//! Bus-injected HUMAN input (`comp.input.*` on the human seat).
//!
//! An injected human event is delivered exactly as a device event, so it
//! meets the same bindings, grabs, corners and focus policy. The path is a
//! synthetic [`InputBackend`] (like `touch::backend::TouchEmu`)
//! whose events go through the very handlers a real device's do. It enters
//! below `process_input_event`'s modality tracking and below the human-input
//! point (`scenegraph` lifecycle), so injected input never counts as a person
//! at the keyboard. The agent seat does not come here: its events go straight
//! to its own smithay handles (policy-host `input`).

use smithay::backend::input::{
    AbsolutePositionEvent, Axis, AxisRelativeDirection, AxisSource, ButtonState, Device,
    DeviceCapability, Event, InputBackend, KeyState, KeyboardKeyEvent, PointerAxisEvent,
    PointerButtonEvent, PointerMotionAbsoluteEvent, PointerMotionEvent, UnusedEvent,
};
use smithay::input::keyboard::Keycode;
use smithay::utils::{Physical, Point};
use std::path::PathBuf;
use world::state::Loop;
use world::state::state::CoordinateTrait;

/// Zero-variant marker backend carrying the injected event types.
pub enum Injected {}

#[derive(PartialEq, Eq, Hash)]
pub struct InjectedDevice;

impl Device for InjectedDevice {
    fn id(&self) -> String {
        "comp-injected".into()
    }
    fn name(&self) -> String {
        "comp-injected".into()
    }
    fn has_capability(&self, capability: DeviceCapability) -> bool {
        matches!(capability, DeviceCapability::Pointer | DeviceCapability::Keyboard)
    }
    fn usb_id(&self) -> Option<(u32, u32)> {
        None
    }
    fn syspath(&self) -> Option<PathBuf> {
        None
    }
}

macro_rules! injected_event {
    ($ty:ty) => {
        impl Event<Injected> for $ty {
            fn time(&self) -> u64 {
                u64::from(self.time) * 1000
            }
            fn device(&self) -> InjectedDevice {
                InjectedDevice
            }
        }
    };
}

/// Absolute motion as a 0..1 fraction of the output, as real backends give it.
pub struct Abs {
    pub time: u32,
    pub nx: f64,
    pub ny: f64,
}
injected_event!(Abs);
impl AbsolutePositionEvent<Injected> for Abs {
    fn x(&self) -> f64 {
        self.nx
    }
    fn y(&self) -> f64 {
        self.ny
    }
    fn x_transformed(&self, width: i32) -> f64 {
        self.nx * f64::from(width)
    }
    fn y_transformed(&self, height: i32) -> f64 {
        self.ny * f64::from(height)
    }
}
impl PointerMotionAbsoluteEvent<Injected> for Abs {}

/// A relative delta; accelerated == unaccelerated (the `comp.input.*` contract).
pub struct Rel {
    pub time: u32,
    pub dx: f64,
    pub dy: f64,
}
injected_event!(Rel);
impl PointerMotionEvent<Injected> for Rel {
    fn delta_x(&self) -> f64 {
        self.dx
    }
    fn delta_y(&self) -> f64 {
        self.dy
    }
    fn delta_x_unaccel(&self) -> f64 {
        self.dx
    }
    fn delta_y_unaccel(&self) -> f64 {
        self.dy
    }
}

pub struct Btn {
    pub time: u32,
    pub button: u32,
    pub state: ButtonState,
}
injected_event!(Btn);
impl PointerButtonEvent<Injected> for Btn {
    fn button_code(&self) -> u32 {
        self.button
    }
    fn state(&self) -> ButtonState {
        self.state
    }
}

/// One scroll frame: an amount (and optional v120) per axis.
pub struct Scroll {
    pub time: u32,
    pub horizontal: Option<f64>,
    pub vertical: Option<f64>,
    pub v120: (Option<i32>, Option<i32>),
    pub source: AxisSource,
}
injected_event!(Scroll);
impl PointerAxisEvent<Injected> for Scroll {
    fn amount(&self, axis: Axis) -> Option<f64> {
        match axis {
            Axis::Horizontal => self.horizontal,
            Axis::Vertical => self.vertical,
        }
    }
    fn amount_v120(&self, axis: Axis) -> Option<f64> {
        match axis {
            Axis::Horizontal => self.v120.0,
            Axis::Vertical => self.v120.1,
        }
        .map(f64::from)
    }
    fn source(&self) -> AxisSource {
        self.source
    }
    fn relative_direction(&self, _axis: Axis) -> AxisRelativeDirection {
        AxisRelativeDirection::Identical
    }
}

pub struct Key {
    pub time: u32,
    pub code: Keycode,
    pub state: KeyState,
}
injected_event!(Key);
impl KeyboardKeyEvent<Injected> for Key {
    fn key_code(&self) -> Keycode {
        self.code
    }
    fn state(&self) -> KeyState {
        self.state
    }
    fn count(&self) -> u32 {
        u32::from(self.state == KeyState::Pressed)
    }
}

impl InputBackend for Injected {
    type Device = InjectedDevice;
    type KeyboardKeyEvent = Key;
    type PointerAxisEvent = Scroll;
    type PointerButtonEvent = Btn;
    type PointerMotionEvent = Rel;
    type PointerMotionAbsoluteEvent = Abs;
    type GestureSwipeBeginEvent = UnusedEvent;
    type GestureSwipeUpdateEvent = UnusedEvent;
    type GestureSwipeEndEvent = UnusedEvent;
    type GesturePinchBeginEvent = UnusedEvent;
    type GesturePinchUpdateEvent = UnusedEvent;
    type GesturePinchEndEvent = UnusedEvent;
    type GestureHoldBeginEvent = UnusedEvent;
    type GestureHoldEndEvent = UnusedEvent;
    type TouchDownEvent = UnusedEvent;
    type TouchUpEvent = UnusedEvent;
    type TouchMotionEvent = UnusedEvent;
    type TouchCancelEvent = UnusedEvent;
    type TouchFrameEvent = UnusedEvent;
    type TabletToolAxisEvent = UnusedEvent;
    type TabletToolProximityEvent = UnusedEvent;
    type TabletToolTipEvent = UnusedEvent;
    type TabletToolButtonEvent = UnusedEvent;
    type SwitchToggleEvent = UnusedEvent;
    type SpecialEvent = ();
}

/// Move the human cursor to `position`, physical pixels on the output it is
/// on (the same space as a real absolute device).
pub fn pointer_to(lp: &mut Loop, position: Point<f64, Physical>, time: u32) {
    let (width, height) = lp.size_ctx_all().screen_size_physical;
    let event = Abs {
        time,
        nx: position.x / width.max(1.0),
        ny: position.y / height.max(1.0),
    };
    crate::pointer::input::motion::absolute::<Injected>(&event, lp);
}

/// Move the human cursor by a relative device delta.
pub fn pointer_by(lp: &mut Loop, dx: f64, dy: f64, time: u32) {
    crate::pointer::input::motion::relative::<Injected>(&Rel { time, dx, dy }, lp);
}

pub fn button(lp: &mut Loop, button: u32, pressed: bool, time: u32) {
    let state = if pressed { ButtonState::Pressed } else { ButtonState::Released };
    crate::pointer::input::button::button::<Injected>(&Btn { time, button, state }, lp);
}

pub fn scroll(lp: &mut Loop, scroll: Scroll) {
    crate::pointer::input::axis::axis::<Injected>(&scroll, lp);
}

pub fn key(lp: &mut Loop, code: Keycode, pressed: bool, time: u32) {
    let state = if pressed { KeyState::Pressed } else { KeyState::Released };
    crate::keyboard::input::keyboard::input_received::<Injected>(&Key { time, code, state }, lp);
}

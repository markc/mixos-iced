use slots::define_buffer;
use slots::input::event::base::{InputEvent, InputFlow};
use slots::input::layer::base as input_layer;
use slots::storage::token::base::{Token, TokenMut};
use slots::trait_::system::base::{BufferCx, System, SystemCx, WorldBuilder};
use dispatcher::state::state::Dispatch;
use crate::canvas::state::state::CanvasState;
use crate::surface::system::base::announce_iced_button;
use smithay::backend::input::ButtonState;
use smithay::input::pointer::ButtonEvent;
use smithay::utils::SERIAL_COUNTER;
use std::any::Any;
use std::time::{SystemTime, UNIX_EPOCH};

pub static CANVAS: Token<CanvasState> = Token::new();
/// TRANSITIONAL pub: legacy call sites still write this slot directly until
/// their logic moves into systems/events.
pub static CANVAS_MUT: TokenMut<CanvasState> = TokenMut::new(&CANVAS);

pub(crate) enum CanvasCmd {
    PanUpdating(bool),
}
define_buffer!(CANVAS_BUF: CanvasCmd);

/// Owns the canvas slot and the canvas-direct pointer handlers. `input()`
/// handles pointer PRESS (`press.rs`) and RELEASE here.
///
/// There are no canvas tools (Move / Scale / Select / select-box / Hand) and no
/// motion transforms behind them, so no grab is ever armed: a release only
/// clears the pan flag and forwards the button-up to the client and to iced.
#[derive(Default)]
pub struct CanvasSystem;

impl System for CanvasSystem {
    fn name(&self) -> &'static str {
        "canvas"
    }

    fn register(&mut self, builder: &mut WorldBuilder) {
        builder.storage.insert(&CANVAS, CanvasState::new());
        // Generic teleport-suppression lock (refcount) lives in world storage so any
        // system can acquire/release it; seeded to 0 here (canvas is its first client).
        builder.storage.insert(&drivers::output::base::TELEPORT_SUPPRESS, 0u32);
        builder.input(input_layer::WORLD);
    }

    fn input(&mut self, cx: &mut SystemCx, event: &InputEvent) -> InputFlow {
        let InputEvent::PointerButton { button, pressed, x, y, modality } = event else {
            return InputFlow::Pass;
        };
        if *pressed {
            return crate::canvas::system::press::press(cx, *button, *x, *y, *modality);
        }

        cx.write(&CANVAS_BUF, CanvasCmd::PanUpdating(false));

        // The wayland pointer button-up goes to the window under the pointer.
        let serial = SERIAL_COUNTER.next_serial();
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u32)
            .unwrap_or(0);
        if let Some(dispatch) = cx.seat.as_deref_mut().and_then(|s| s.downcast_mut::<Dispatch>())
            && let Some(pointer) = dispatch.seat.seat.get_pointer()
        {
            pointer.button(
                dispatch,
                &ButtonEvent { button: *button, state: ButtonState::Released, serial, time },
            );
            pointer.frame(dispatch);
        }

        // Iced button-up ALWAYS routes through the surface system's slot: an iced
        // Button emits on release. A no-op when nothing was pressed on iced.
        announce_iced_button(cx.channels, *button, false);

        InputFlow::Consume
    }

    fn buffer(&mut self, cx: &mut BufferCx, message: Box<dyn Any>) {
        match *message.downcast::<CanvasCmd>().expect("canvas buffer type") {
            CanvasCmd::PanUpdating(value) => {
                let canvas = cx.storage.get_mut(&CANVAS_MUT);
                let was = canvas.position_updating;
                canvas.position_updating = value;
                // Pan is one client of the teleport-suppression lock: acquire on pan
                // START (edge false→true), release on END (true→false). Edge-detected so
                // the unconditional PanUpdating(false) on every button release doesn't
                // decrement another system's lock when no pan was active.
                if value != was {
                    let lock = cx.storage.get_mut(&drivers::output::base::TELEPORT_SUPPRESS_MUT);
                    *lock = if value { lock.saturating_add(1) } else { lock.saturating_sub(1) };
                }
            }
        }
    }
}

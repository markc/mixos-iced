//! Pointer PRESS, migrated from the rim (`canvas.input/input.pointer/press.rs`
//! + `window.input/input.pointer/window.rs`) into `CanvasSystem::input`.
//!
//! There are no canvas tools (Move / Scale / Select / select-box / Hand, touch
//! selection frame, placeholder grabs), so no grab is ever armed and only the
//! plain-click path exists: a press over a window or compositor iced UI passes to the rim's
//! `native_press`; a press on empty canvas deactivates every window, drops
//! keyboard focus and forwards the button.
//!
//! Consume is ALL-OR-NOTHING: this returns `InputFlow::Consume` exactly when the
//! old rim handler returned `true`, and `InputFlow::Pass` when it returned
//! `false` (so the rim's `native_press` runs against the window beneath).

use slots::input::event::base::{InputFlow, Modality};
use slots::trait_::system::base::SystemCx;
use dispatcher::state::state::Dispatch;
use crate::surface::interface::core::hit::surface_under_filtered_cx;
use crate::surface::system::base::{announce_iced_button, announce_iced_focus};
use smithay::backend::input::{ButtonState, KeyState};
use smithay::input::keyboard::Keycode;
use smithay::input::pointer::ButtonEvent;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, SERIAL_COUNTER};
use std::time::{SystemTime, UNIX_EPOCH};
use protocols::window::shell::shell;

pub(crate) fn press(cx: &mut SystemCx, button: u32, x: f64, y: f64, _modality: Modality) -> InputFlow {
    let cursor = Point::<f64, Logical>::from((x, y));

    let over_surface = surface_under_filtered_cx(cx.storage, cursor, &|_hit| true);
    // "Over ice" = over any iced surface (registry `Iced` hits, or a layer-shell
    // surface flagged ice).
    let over_ice =
        matches!(&over_surface, Some(hit) if hit.ice().is_some() || hit.is_iced());

    let mut temporary_passthrough = false;
    if let Some(ice_layer) = over_surface.as_ref().and_then(|w| w.iced_layer()) {
        temporary_passthrough = (ice_layer
            & crate::scene::layer::base::Layer::SCENE_SURFACE_GROUP.bits())
            != 0;
    }

    // Over a window (or anything hit-testable): Pass so the rim's native_press
    // routes the click to it.
    if over_surface.is_some() && !temporary_passthrough {
        return InputFlow::Pass;
    }
    if over_ice {
        return InputFlow::Pass;
    }

    finalize_non_hand(cx, button);

    if temporary_passthrough {
        InputFlow::Pass
    } else {
        InputFlow::Consume
    }
}

/// Deactivate every window, release held keys, drop wayland + iced keyboard
/// focus, and forward the press to the seat/iced (the rim's non-hand tail).
fn finalize_non_hand(cx: &mut SystemCx, button: u32) {
    let serial = SERIAL_COUNTER.next_serial();
    let time = now_msec();

    if let Some(platform) = cx
        .platform
        .as_deref_mut()
        .and_then(|p| p.downcast_mut::<crate::scene::platform::platform::Platform>())
    {
        for window in platform.space().elements() {
            window.set_activated(false);
            shell::send_pending(window);
        }
    }

    if let Some(dispatch) = cx.seat.as_deref_mut().and_then(|s| s.downcast_mut::<Dispatch>()) {
        // Release held (non-modifier) keys before dropping focus so clients that
        // track their own keyboard state don't get stuck key-down on re-focus.
        release_held_keys(dispatch);

        if let Some(keyboard) = dispatch.seat.seat.get_keyboard() {
            keyboard.set_focus(dispatch, Option::<WlSurface>::None, serial);
        }

        if let Some(pointer) = dispatch.seat.seat.get_pointer() {
            pointer.button(
                dispatch,
                &ButtonEvent { button, state: ButtonState::Pressed, serial, time },
            );
            pointer.frame(dispatch);
        }
    }

    // Iced deactivation: clear keyboard focus + dispatch the button-down. Both
    // route through the surface system's slot (we can't touch its registry).
    announce_iced_focus(cx.channels, None);
    announce_iced_button(cx.channels, button, true);
}

/// Reimplementation of `seat::keyboard::input::keyboard::
/// release_held_keys` reading `cx.seat`'s `Dispatch` instead of `&mut Loop`:
/// release every held NON-modifier key to the focused client (forwarded, no
/// intercept) before keyboard focus is cleared.
fn release_held_keys(dispatch: &mut Dispatch) {
    let Some(keyboard) = dispatch.seat.seat.get_keyboard() else {
        return;
    };
    let to_release: Vec<Keycode> = keyboard.with_pressed_keysyms(|syms| {
        syms.iter()
            .filter(|h| !is_modifier_keysym(h.modified_sym().raw()))
            .map(|h| h.raw_code())
            .collect()
    });
    if to_release.is_empty() {
        return;
    }
    let time = now_msec();
    for key in to_release {
        let serial = SERIAL_COUNTER.next_serial();
        let _ = keyboard.input::<(), _>(dispatch, key, KeyState::Released, serial, time, |_, _, _| {
            smithay::input::keyboard::FilterResult::Forward
        });
    }
}

/// X11 keysym ranges for modifier keys (see the rim `keyboard.rs` copy).
fn is_modifier_keysym(raw: u32) -> bool {
    matches!(raw, 0xffe1..=0xffee | 0xff7f | 0xfe01..=0xfe13)
}

fn now_msec() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u32)
        .unwrap_or(0)
}

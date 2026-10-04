//! Clear held key state on a focus/session boundary.
//!
//! Extracted from the winit backend's `Focus(false)` handler. The same problem
//! exists on native TTY switch; both routes now call this one entry.
//!
//! CHECK: PR smithay upstream for a per-key
//! `clear_held_keys` on KeyboardHandle; until then we synthesize releases for
//! the known modifier keycodes.

use smithay::utils::SERIAL_COUNTER;
use world::state::Loop;

/// Keycodes of the modifier keys we force-release (evdev + 8 offset space, as
/// used by the original handler).
pub const MOD_KEYCODES: &[u32] = &[50, 62, 37, 105, 64, 108, 133, 134];

/// Force-release held modifiers and clear the surface registry's notion of
/// held modifiers. Safe to call on any focus-loss or session-pause boundary.
pub fn clear_held_modifiers(state: &mut Loop) {
    let Some(keyboard) = state.state.seat.seat.get_keyboard() else {
        trace!("clear_held_modifiers: no keyboard on seat; nothing to clear");
        return;
    };
    info!("clearing held modifier keys (focus/session boundary)");
    let serial = SERIAL_COUNTER.next_serial();

    for _ in 0..3 {
        for &kc in MOD_KEYCODES {
            keyboard.input::<(), _>(
                &mut state.state,
                kc.into(),
                smithay::backend::input::KeyState::Released,
                serial,
                0,
                |_, _, _| smithay::input::keyboard::FilterResult::Intercept(()),
            );
        }
    }

    if let Some(registry) = state.inner.surface_mut().registry.as_mut() {
        registry.release_all_modifiers();
    }
}

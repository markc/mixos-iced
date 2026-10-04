//! Opt-in (Law 7): estimated-vblank pacing of frame callbacks when a frame
//! produced empty damage and nothing was committed — prevents clients from
//! busy-looping on frame callbacks. DOUBLE-GATED: the `flip-estimate` cargo
//! feature + `SafetyEnable::estimate_pacing` at the wiring site.
//!
//! Explicitly NOT a redraw state machine: backends keep the existing redraw
//! protocol; this is a timer aid that bolts onto it when enabled.

#[cfg(feature = "flip-estimate")]
pub use gated::{arm, disarm};

#[cfg(feature = "flip-estimate")]
mod gated {
    use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
    use smithay::reexports::calloop::{LoopHandle, RegistrationToken};
    use std::time::Duration;

    /// Arm a one-shot timer at the estimated next vblank to deliver throttled
    /// frame callbacks for an empty-damage frame. Registration failure is not
    /// self-recovering — panic.
    pub fn arm<D: 'static>(
        handle: &LoopHandle<'static, D>,
        until_estimated_vblank: Duration,
        deliver_callbacks: impl FnOnce(&mut D) + 'static,
    ) -> RegistrationToken {
        let mut deliver = Some(deliver_callbacks);
        handle
            .insert_source(
                Timer::from_duration(until_estimated_vblank),
                move |_, _, data| {
                    if let Some(f) = deliver.take() {
                        f(data);
                    }
                    TimeoutAction::Drop
                },
            )
            .expect("estimate timer registration failed")
    }

    pub fn disarm<D>(handle: &LoopHandle<'static, D>, token: RegistrationToken) {
        handle.remove(token);
    }
}

//! Opt-in (Law 7): re-time vblanks that buggy drivers deliver far earlier
//! than the refresh interval (observed as `passed < refresh / 2`).
//! DOUBLE-GATED: the `timing-throttle` cargo feature +
//! `SafetyEnable::vblank_throttle` at the wiring site. Loop-agnostic (Law 4):
//! generic over the loop user-data type.

#[cfg(feature = "timing-throttle")]
pub use gated::VblankThrottle;

#[cfg(feature = "timing-throttle")]
mod gated {
    use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
    use smithay::reexports::calloop::{LoopHandle, RegistrationToken};
    use std::time::Duration;

    #[derive(Debug, Default)]
    pub struct VblankThrottle {
        last_vblank: Option<Duration>,
        pending: Option<RegistrationToken>,
        warned: bool,
    }

    impl VblankThrottle {
        pub fn new() -> Self {
            Self::default()
        }

        /// Returns true when the vblank was deferred (caller must NOT process
        /// it now; `deliver` runs at the corrected instant). Timer
        /// registration failure is not self-recovering — panic.
        pub fn throttle<D: 'static>(
            &mut self,
            handle: &LoopHandle<'static, D>,
            refresh_interval: Duration,
            timestamp: Duration,
            deliver: impl FnMut(&mut D) + 'static,
        ) -> bool {
            let mut deliver = deliver;
            if let Some(token) = self.pending.take() {
                handle.remove(token);
            }
            if let Some(last) = self.last_vblank {
                let passed = timestamp.saturating_sub(last);
                if passed < refresh_interval / 2 {
                    if !self.warned {
                        self.warned = true;
                        warn!(
                            "vblanks arriving faster than expected (after {passed:?}, refresh \
                             {refresh_interval:?}); throttling"
                        );
                    }
                    let remaining = refresh_interval - passed;
                    let token = handle
                        .insert_source(Timer::from_duration(remaining), move |_, _, data| {
                            deliver(data);
                            TimeoutAction::Drop
                        })
                        .expect("vblank throttle timer registration failed");
                    self.pending = Some(token);
                    self.last_vblank = Some(timestamp + remaining);
                    return true;
                }
            }
            self.last_vblank = Some(timestamp);
            false
        }
    }
}

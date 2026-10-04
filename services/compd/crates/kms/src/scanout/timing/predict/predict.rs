//! Opt-in (Law 7): next-presentation-time estimation, VRR-agnostic (VRR
//! control left the project; the clock treats the refresh interval as fixed).
//! DOUBLE-GATED: the `timing-predict` cargo feature + 
//! `SafetyEnable::presentation_predict` at the wiring site. Pure arithmetic
//! over caller-supplied values; holds no loop state.

#[cfg(feature = "timing-predict")]
pub use gated::PresentationClock;

#[cfg(feature = "timing-predict")]
mod gated {
    use std::time::Duration;

    #[derive(Debug, Clone, Copy)]
    pub struct PresentationClock {
        last_presentation: Option<Duration>,
        refresh_interval: Duration,
    }

    impl PresentationClock {
        pub fn new(refresh_interval: Duration) -> Self {
            Self {
                last_presentation: None,
                refresh_interval,
            }
        }

        pub fn presented(&mut self, time: Duration) {
            if !time.is_zero() {
                self.last_presentation = Some(time);
            }
        }

        /// Predict the next presentation instant given `now`.
        pub fn next_presentation(&self, now: Duration) -> Duration {
            let Some(last) = self.last_presentation else {
                return now + self.refresh_interval;
            };
            let interval = self.refresh_interval;
            let now = if now <= last { last + interval } else { now };
            let since = now - last;
            let intervals = since.as_nanos() / interval.as_nanos().max(1) + 1;
            last + interval.saturating_mul(intervals as u32)
        }
    }
}

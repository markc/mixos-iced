//! The "may park" predicate: the eligibility list and its 30 s settlement cap.
//!
//! With `PREEMPTIVE_DEFAULT = Engaged` an empty frame parks the loop: no flip,
//! so no vblank and no next frame until something requests one. That is right
//! only when "empty" is the truth. Some damage the tracker cannot see yet:
//!
//! - a client dmabuf import not yet resolved;
//! - a session resume whose first flip has not been seen.
//!
//! While any of these [`Holds`], an empty frame does not park: the executor asks
//! for another frame at the next estimated vblank. Bounded — if the holds never
//! clear, [`Settlement`] reports it once after [`SETTLEMENT_CAP`] and lets the
//! loop park anyway, so a stuck producer costs a log line instead of a
//! full-rate render loop forever.
//!
//! Deliberately NOT holds:
//! - a live capture. Holding on it made every static stretch of a recording 30 s
//!   of per-vblank empty renders, an `error!`, then a recording that stopped
//!   encoding. A frame that is truly empty has nothing new to encode (the
//!   recorder is variable-frame-rate); capture's polls that DO need frames ask
//!   for their own (Save As / re-encode, paced per vblank by
//!   `Schedule::begin_render`), and its time-based keep-alive asks for a frame
//!   at its own deadline;
//! - an off-thread publish in flight. Its only producer is iced, whose
//!   `wants_frame` already keeps the loop going while a publish is pending — a
//!   second hold for the same thing, attributed to a `Background` compd does not
//!   have, bought nothing.

use std::time::{Duration, Instant};

/// How long the loop may be held unsettled before parking regardless.
pub const SETTLEMENT_CAP: Duration = Duration::from_secs(30);

/// What is keeping an empty frame from being the truth.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Holds {
    pub dmabuf_import: bool,
    pub resuming: bool,
}

impl Holds {
    pub fn any(&self) -> bool {
        self.dmabuf_import || self.resuming
    }

    pub fn names(&self) -> Vec<&'static str> {
        [(self.dmabuf_import, "dmabuf_import"), (self.resuming, "resuming")]
            .into_iter()
            .filter_map(|(on, name)| on.then_some(name))
            .collect()
    }

    /// The reason the continuation frame is requested for.
    pub fn reason(&self) -> protocols::redraw::schedule::schedule::RedrawReason {
        use protocols::redraw::schedule::schedule::RedrawReason;
        if self.resuming {
            RedrawReason::Resume
        } else {
            RedrawReason::Commit
        }
    }
}

/// What to do with an empty frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Nothing holds the loop: park.
    Park,
    /// Something holds it: request another frame.
    Hold,
    /// Held for longer than [`SETTLEMENT_CAP`]: park, and report it (once).
    Timeout,
}

/// How long the loop has been held unsettled.
#[derive(Debug, Default)]
pub struct Settlement {
    since: Option<Instant>,
    reported: bool,
}

impl Settlement {
    /// A frame was submitted: whatever held the loop produced its damage, so a
    /// later hold is timed afresh.
    pub fn reset(&mut self) {
        self.since = None;
        self.reported = false;
    }

    /// Observe one empty frame's holds at `now`.
    pub fn observe(&mut self, holds: Holds, now: Instant) -> Verdict {
        if !holds.any() {
            self.since = None;
            self.reported = false;
            return Verdict::Park;
        }
        let since = *self.since.get_or_insert(now);
        if now.saturating_duration_since(since) < SETTLEMENT_CAP {
            return Verdict::Hold;
        }
        if self.reported {
            // Already said so; keep parking until the holds clear.
            return Verdict::Park;
        }
        self.reported = true;
        Verdict::Timeout
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held() -> Holds {
        Holds {
            dmabuf_import: true,
            ..Holds::default()
        }
    }

    #[test]
    fn nothing_held_parks_and_resets() {
        let mut s = Settlement::default();
        let t = Instant::now();
        assert_eq!(s.observe(held(), t), Verdict::Hold);
        assert_eq!(s.observe(Holds::default(), t), Verdict::Park);
        // The clock restarted: a fresh hold is not timed from the old one.
        assert_eq!(s.observe(held(), t + SETTLEMENT_CAP), Verdict::Hold);
    }

    #[test]
    fn a_hold_times_out_once_after_the_cap() {
        let mut s = Settlement::default();
        let t = Instant::now();
        assert_eq!(s.observe(held(), t), Verdict::Hold);
        assert_eq!(s.observe(held(), t + SETTLEMENT_CAP / 2), Verdict::Hold);
        assert_eq!(s.observe(held(), t + SETTLEMENT_CAP), Verdict::Timeout);
        assert_eq!(
            s.observe(held(), t + SETTLEMENT_CAP * 2),
            Verdict::Park,
            "reported once"
        );
    }

    #[test]
    fn a_submitted_frame_restarts_the_clock() {
        let mut s = Settlement::default();
        let t = Instant::now();
        assert_eq!(s.observe(held(), t), Verdict::Hold);
        s.reset();
        assert_eq!(s.observe(held(), t + SETTLEMENT_CAP), Verdict::Hold);
    }

    #[test]
    fn holds_name_themselves_and_pick_a_reason() {
        use protocols::redraw::schedule::schedule::RedrawReason;
        let h = Holds {
            dmabuf_import: true,
            resuming: true,
        };
        assert_eq!(h.names(), ["dmabuf_import", "resuming"]);
        assert_eq!(h.reason(), RedrawReason::Resume);
        assert!(!Holds::default().any());
    }
}

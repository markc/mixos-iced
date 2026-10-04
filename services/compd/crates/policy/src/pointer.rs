// The pointer-watch lease. The sample types live in comp-model
// (`PointerSample`, on the wire).

//! `comp.pointer.watch`'s demand lease: `pointer.changed` is published only
//! while a watcher renewed the lease in the last [`LEASE`], at most once per
//! [`INTERVAL`], and only when the pointer moved. The engine arms one timer
//! at [`PointerLease::deadline`]; with no watcher there is no timer.

use std::time::{Duration, Instant};

pub const LEASE: Duration = Duration::from_secs(3);
pub const INTERVAL: Duration = Duration::from_millis(34);

#[derive(Clone, Debug, Default)]
pub struct PointerLease {
    until: Option<Instant>,
    last: Option<Instant>,
    dirty: bool,
}

impl PointerLease {
    /// `comp.pointer.watch`: (re)open the lease; the next pass publishes.
    pub fn renew(&mut self, now: Instant) {
        self.until = Some(now + LEASE);
        self.dirty = true;
    }

    pub fn active(&self, now: Instant) -> bool {
        self.until.is_some_and(|until| now < until)
    }

    /// The pointer (or whether it may be reported) changed.
    pub fn changed(&mut self) {
        self.dirty = true;
    }

    /// Whether to publish a sample now (and mark it published).
    pub fn take(&mut self, now: Instant) -> bool {
        if !self.active(now) || !self.dirty || self.last.is_some_and(|last| now < last + INTERVAL) {
            return false;
        }
        self.last = Some(now);
        self.dirty = false;
        true
    }

    /// When the engine must look again: the end of the rate limit while a
    /// change waits, else the lease's end; `None` once it has lapsed.
    pub fn deadline(&self, now: Instant) -> Option<Instant> {
        let until = self.until.filter(|until| now < *until)?;
        Some(if self.dirty {
            self.last.map_or(now, |last| (last + INTERVAL).min(until))
        } else {
            until
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_demand_has_no_publication_or_timer() {
        let now = Instant::now();
        let mut lease = PointerLease::default();
        lease.changed();
        assert!(!lease.take(now));
        assert!(lease.deadline(now).is_none());
        lease.renew(now);
        assert!(lease.take(now));
        assert_eq!(lease.deadline(now), Some(now + LEASE));
        assert!(!lease.take(now + LEASE));
        assert!(lease.deadline(now + LEASE).is_none());
    }

    #[test]
    fn rapid_motion_and_renewal_cannot_exceed_rate() {
        let now = Instant::now();
        let mut lease = PointerLease::default();
        lease.renew(now);
        assert!(lease.take(now));
        for millis in 1..34 {
            lease.changed();
            lease.renew(now + Duration::from_millis(millis));
            assert!(!lease.take(now + Duration::from_millis(millis)));
        }
        assert_eq!(lease.deadline(now), Some(now + INTERVAL));
        assert!(lease.take(now + INTERVAL));
        assert!(!lease.take(now + INTERVAL));
    }

    #[test]
    fn renewal_recovers_after_expiry_without_retaining_motion_history() {
        let now = Instant::now();
        let mut lease = PointerLease::default();
        lease.renew(now);
        assert!(lease.take(now));
        for _ in 0..100_000 {
            lease.changed();
        }
        assert!(!lease.take(now + LEASE));
        lease.renew(now + LEASE);
        assert!(lease.take(now + LEASE));
        assert!(!lease.take(now + LEASE));
    }
}

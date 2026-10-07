// SPDX-License-Identifier: Apache-2.0
//! Native presentation observations for the buffer commit following a request.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Process-unique checked identity of one native feedback request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PresentationId(u64);

impl PresentationId {
    /// The process-local request identity, not an output or application sequence.
    pub fn get(self) -> u64 {
        self.0
    }
}

/// A request could not obtain native presentation evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresentationError {
    /// The display backend or compositor does not provide this protocol.
    Unsupported,
    /// The bounded outstanding feedback/receipt budget is occupied.
    Capacity,
    /// The checked process identity allocator is exhausted.
    Exhausted,
    /// The surface or native protocol object has retired.
    Closed,
}

/// Read-only native feedback capacity. Actual reservation retirement wakes
/// every live Wayland event loop in this process through its existing awakener.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PresentationCapacity {
    /// Process release generation, read before the availability counters.
    /// At exhaustion callers must reconcile on every native wake.
    pub release_epoch: u64,
    /// Both the window and process can presently admit a request. This is a
    /// snapshot, not a reservation; request admission remains authoritative.
    pub available: bool,
}

/// The compositor's terminal observation for a particular buffer commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresentationOutcome {
    /// The compositor reached its presentation boundary. Nested compositors
    /// attest their own submission, not physical host monitor scanout.
    Presented {
        /// Native clock identity, when announced by the compositor.
        clock_id: Option<u32>,
        /// Timestamp seconds in that clock.
        seconds: u64,
        /// Timestamp nanoseconds.
        nanoseconds: u32,
        /// Nominal refresh interval, or zero when unknown.
        refresh_ns: u32,
        /// Compositor output sequence; unrelated to the request identity.
        output_sequence: u64,
        /// Native protocol flags, preserving unknown bits.
        flags: u32,
    },
    /// The compositor discarded the commit.
    Discarded,
}

/// One terminal native receipt. Retaining this value (or a clone) retains its
/// budget slot; read the fields and drop it when observation handling ends.
#[derive(Clone)]
pub struct PresentationFeedback {
    /// Identity returned by the synchronous request.
    pub id: PresentationId,
    /// Actual compositor observation.
    pub outcome: PresentationOutcome,
    _lease: Arc<dyn Send + Sync>,
}

impl PresentationFeedback {
    pub(crate) fn new(
        id: PresentationId,
        outcome: PresentationOutcome,
        lease: Arc<dyn Send + Sync>,
    ) -> Self {
        Self { id, outcome, _lease: lease }
    }
}

impl fmt::Debug for PresentationFeedback {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PresentationFeedback")
            .field("id", &self.id)
            .field("outcome", &self.outcome)
            .finish()
    }
}
impl PartialEq for PresentationFeedback {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.outcome == other.outcome
    }
}

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

pub(crate) fn next_id() -> Result<PresentationId, PresentationError> {
    allocate(&NEXT_ID)
}

fn allocate(counter: &AtomicU64) -> Result<PresentationId, PresentationError> {
    counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| value.checked_add(1))
        .map(|previous| PresentationId(previous + 1))
        .map_err(|_| PresentationError::Exhausted)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identities_do_not_wrap_or_reuse() {
        let counter = AtomicU64::new(u64::MAX - 1);
        assert_eq!(allocate(&counter).unwrap().get(), u64::MAX);
        assert_eq!(allocate(&counter), Err(PresentationError::Exhausted));
        assert_eq!(counter.load(Ordering::Relaxed), u64::MAX);
    }
}

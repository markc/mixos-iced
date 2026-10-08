// SPDX-License-Identifier: MIT
//! Immutable rendered-view identity and observation-only presentation receipts.

use super::Id;

#[cfg(feature = "native-frame-probe")]
#[doc(hidden)]
pub mod probe;
use std::{fmt, sync::Arc};

/// Identity supplied by the owner of the view being constructed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FrameStamp {
    /// Checked successful presentation activation epoch.
    pub activation_epoch: u64,
    /// Checked local preparation revision associated with that activation.
    pub local_revision: u64,
}

/// Native evidence or an explicit inability to obtain it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameOutcome {
    /// The compositor presented the exact successfully submitted commit.
    Presented {
        /// Compositor clock identity.
        clock_id: Option<u32>,
        /// Native timestamp seconds.
        seconds: u64,
        /// Native timestamp nanoseconds.
        nanoseconds: u32,
        /// Nominal refresh interval.
        refresh_ns: u32,
        /// Native output sequence, distinct from the application stamp.
        output_sequence: u64,
        /// Raw presentation protocol flags.
        flags: u32,
    },
    /// The submitted commit was discarded.
    Discarded,
    /// This backend/compositor does not supply native evidence.
    Unsupported,
    /// Outstanding evidence reached its bounded capacity.
    Capacity,
    /// The checked native request allocator exhausted its identities.
    Exhausted,
    /// The owning window retired.
    Closed,
    /// Submission failed after requesting feedback. Later feedback cannot
    /// certify the intended commit.
    SubmissionFailed,
}

/// Copied metadata, with no native protocol lease or application-state borrow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameObservation {
    /// Actual runtime window incarnation.
    pub window: Id,
    /// Identity of the interface actually drawn.
    pub stamp: FrameStamp,
    /// Native request ID, when the request was issued.
    pub request_id: Option<u64>,
    /// Native terminal outcome or explicit evidence unavailability.
    pub outcome: FrameOutcome,
}

/// An observation sink. Callbacks must only store bounded metadata and notify
/// existing waiters; they must not mutate application State or call rendering.
#[derive(Clone)]
pub struct FrameObserver {
    sink: Arc<dyn Fn(FrameObservation) + Send + Sync>,
    capture: Option<Arc<dyn Fn() -> FrameObserver + Send + Sync>>,
}

impl FrameObserver {
    /// Construct an owned, thread-safe metadata sink.
    pub fn new(observe: impl Fn(FrameObservation) + Send + Sync + 'static) -> Self {
        Self {
            sink: Arc::new(observe),
            capture: None,
        }
    }
    /// Stable view bindings can acquire fresh lifecycle provenance for each
    /// native request. The factory must return a terminal observation sink.
    pub fn with_capture(capture: impl Fn() -> FrameObserver + Send + Sync + 'static) -> Self {
        let capture: Arc<dyn Fn() -> FrameObserver + Send + Sync> = Arc::new(capture);
        let deliver = Arc::clone(&capture);
        Self {
            sink: Arc::new(move |receipt| deliver().observe(receipt)),
            capture: Some(capture),
        }
    }
    /// Capture the terminal sink for one native feedback request.
    pub fn captured(&self) -> Self {
        self.capture
            .as_ref()
            .map_or_else(|| self.clone(), |capture| capture())
    }
    /// Whether two sinks name the same observation owner.
    pub fn same_owner(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.captured().sink, &other.captured().sink)
    }
    /// Store one copied observation without sending an application message.
    pub fn observe(&self, observation: FrameObservation) {
        (self.sink)(observation);
    }
}
impl fmt::Debug for FrameObserver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrameObserver").finish_non_exhaustive()
    }
}

/// Bound alongside view construction and retained with that exact interface.
#[derive(Clone, Debug)]
pub struct FrameBinding {
    /// The immutable view stamp.
    pub stamp: FrameStamp,
    /// Owned metadata observer.
    pub observer: FrameObserver,
}
impl FrameBinding {
    /// Capture immediately when admitting native feedback, not at receipt or
    /// view construction. Held old requests then retain their original scope.
    pub fn captured(&self) -> Self {
        Self {
            stamp: self.stamp,
            observer: self.observer.captured(),
        }
    }
    /// Compare both view identity and observation ownership.
    pub fn same_presentation(&self, other: &Self) -> bool {
        self.stamp == other.stamp && self.observer.same_owner(&other.observer)
    }
    /// Deliver bounded copied metadata after native feedback ownership retires.
    pub fn observe(&self, window: Id, request_id: Option<u64>, outcome: FrameOutcome) {
        self.observer.observe(FrameObservation {
            window,
            stamp: self.stamp,
            request_id,
            outcome,
        });
    }
}

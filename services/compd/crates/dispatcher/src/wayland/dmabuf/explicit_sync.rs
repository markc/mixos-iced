//! Explicit-sync health (integration F6).
//!
//! The explicit-sync path latches faulted for good on any failure that
//! can leave a client waiting on a fence or release that never comes, and
//! from then on refuses new explicit-sync commits: it signals the commit's
//! release point and disconnects the client, rather than accept a buffer it
//! may never release correctly. compd has no retirement worker (smithay
//! releases buffers), so its faults are:
//! - a failed release-point signal (vendored smithay counts them:
//!   `drm_syncobj::signal_failures`);
//! - a fence source the loop refused (that commit's blocker never clears).
//!
//! An acquire point whose blocker could not be generated refuses THAT commit
//! but does not latch: the failure can be per-commit and transient (an fd
//! limit), and latching would disconnect every explicit-sync client.
//!
//! Process-wide and permanent: a restart clears it.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

static FAULTED: AtomicBool = AtomicBool::new(false);
static REASON: Mutex<Option<String>> = Mutex::new(None);

/// Latch the fault (the first reason is kept and logged).
pub fn fault(reason: impl Into<String>) {
    if !FAULTED.swap(true, Ordering::AcqRel) {
        let reason = reason.into();
        error!("explicit sync FAULTED ({reason}); new syncobj commits are refused until compd restarts");
        *REASON.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(reason);
    }
}

/// `info.explicit_sync_healthy`: no fault latched, and no release-point
/// signal has failed (a failure latches here when first seen).
pub fn healthy() -> bool {
    if FAULTED.load(Ordering::Acquire) {
        return false;
    }
    let failures = smithay::wayland::drm_syncobj::signal_failures();
    if failures > 0 {
        fault(format!("{failures} release-point signal(s) failed"));
        return false;
    }
    true
}

/// Why it latched, once it has.
pub fn reason() -> Option<String> {
    REASON.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Healthy until a fault; the fault latches for good and keeps its FIRST
    /// reason (the rest are logged by their callers, not recorded).
    #[test]
    fn a_fault_latches_with_its_first_reason() {
        assert!(healthy(), "healthy at startup: nothing has failed");
        assert_eq!(reason(), None);
        fault("first");
        fault("second");
        assert!(!healthy());
        assert!(!healthy(), "and stays latched");
        assert_eq!(reason().as_deref(), Some("first"));
    }
}

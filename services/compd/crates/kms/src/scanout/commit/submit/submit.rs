//! A real implementation with working mechanism bodies, compiled under the
//! `native-scanout` cargo feature and exercised by the assembly self-test
//! (TEST_ONLY atomic commits, kernel-validated on the real device).
//!
//! Atomic submission: the three commit shapes the machine performs. TEST_ONLY
//! is the kernel-side validation primitive — the self-test's proof and the
//! mode fallback chain's future native attempt. Failure policy: a TEST that
//! fails returns the error (callers branch — that IS the validating use);
//! a real commit that fails panics outside the resume window, matching
//! `flip.queue`.

#[cfg(feature = "native-scanout")]
pub use gated::*;

#[cfg(feature = "native-scanout")]
mod gated {
    use smithay::backend::drm::DrmDeviceFd;
    use smithay::reexports::drm::control::atomic::AtomicModeReq;
    use smithay::reexports::drm::control::{AtomicCommitFlags, Device as ControlDevice};

    /// Kernel-side validation; no state is touched. Local Result by design:
    /// the caller's branch (try the next mode / report the proof) is the use.
    pub fn test(drm: &DrmDeviceFd, req: AtomicModeReq, modeset: bool) -> Result<(), String> {
        let mut flags = AtomicCommitFlags::TEST_ONLY;
        if modeset {
            flags |= AtomicCommitFlags::ALLOW_MODESET;
        }
        drm.atomic_commit(flags, req)
            .map_err(|e| format!("TEST_ONLY commit rejected: {e}"))
    }

    /// The reclaiming/full modeset commit (blocking; resume and bring-up).
    pub fn commit_modeset(drm: &DrmDeviceFd, req: AtomicModeReq) {
        drm.atomic_commit(
            AtomicCommitFlags::ALLOW_MODESET | AtomicCommitFlags::PAGE_FLIP_EVENT,
            req,
        )
        .unwrap_or_else(|e| abort!("modeset commit failed: {e}"));
    }

    /// The steady-state page flip (nonblocking, event-carrying). The resume
    /// window tolerance lives in `flip.queue`'s policy, not here.
    pub fn flip(drm: &DrmDeviceFd, req: AtomicModeReq, resuming: bool) -> bool {
        match drm.atomic_commit(
            AtomicCommitFlags::PAGE_FLIP_EVENT | AtomicCommitFlags::NONBLOCK,
            req,
        ) {
            Ok(()) => true,
            Err(e) if resuming => {
                warn!("flip commit failed during resume window (watchdog recovers): {e}");
                false
            }
            Err(e) => abort!("flip commit failed outside the resume window: {e}"),
        }
    }
}

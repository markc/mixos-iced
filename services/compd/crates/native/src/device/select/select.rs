//! Decides what runs: preferences (ranking AND exclusion) + gpu topology.
//! Initial selection is exercised by `assemble.display`; hotplug-time the
//! decision is Register vs Ignore (multi-GPU activation left the project's
//! current scope — a registered device is bookkept, not driven).

use render_gles::preference::gpu::rank::rank::GpuRank;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Bookkeep the device (registry + topology).
    Register,
    /// Preference-excluded: do not even bookkeep.
    Ignore,
}

/// Hotplug-time decision for a newly appeared device.
pub fn decide(path: &Path, rank: &GpuRank) -> Decision {
    if rank.ignored.iter().any(|i| i == path) {
        Decision::Ignore
    } else {
        Decision::Register
    }
}

/// Initial primary selection (delegates to gpu preference.select policy).
pub fn select_primary(
    candidates: &[PathBuf],
    default_heuristic: Option<&PathBuf>,
    rank: &GpuRank,
) -> Option<PathBuf> {
    kms::gpu::preference::select::select::select(candidates, default_heuristic, rank)
}

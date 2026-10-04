//! Typed GPU preference: ranking + exclusion. A self-contained value type
//! (no configuration system). Population mechanism is out of scope; `get()`
//! returns the default (empty) preference.

use std::path::PathBuf;

#[derive(Debug, Clone, Default)]
pub struct GpuRank {
    /// Device paths preferred as primary, in order.
    pub preferred: Vec<PathBuf>,
    /// Device paths that must never be used (ignored nodes).
    pub ignored: Vec<PathBuf>,
}

pub fn get() -> GpuRank {
    GpuRank::default()
}

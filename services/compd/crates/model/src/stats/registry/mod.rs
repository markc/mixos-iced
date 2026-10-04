//! Process-global diagnostics registry (Statistics tab data source) — façade.
//!
//! The implementation lives in the sibling crates (`registry.hdr` — live HDR encode
//! tuning, `registry.counter` — hot-path atomics, `registry.meta` — rare metadata,
//! `registry.snapshot` — the derived read side). This crate re-exports everything under
//! the `base` module so every existing `base::item` path (and `use ...::base as stats`)
//! keeps resolving unchanged.

pub mod base {
    pub use crate::stats::registry::counter::*;
    pub use crate::stats::registry::gpu::gpu::{DeviceFormat, set_device_format};
    pub use crate::stats::registry::hdr::*;
    pub use crate::stats::registry::meta::*;
    pub use crate::stats::registry::shader::*;
    pub use crate::stats::registry::snapshot::*;
}

pub mod counter;
pub mod gpu;
pub mod hdr;
pub mod meta;
pub mod shader;
pub mod snapshot;

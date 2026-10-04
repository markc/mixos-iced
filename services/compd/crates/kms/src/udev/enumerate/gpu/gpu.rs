//! Default GPU heuristics: smithay's primary_gpu / all_gpus fallback.
//! Preference-free by design — preference-aware selection lives in
//! `backend.gpu/gpu.preference/preference.select`.

use smithay::backend::udev::{all_gpus, primary_gpu};
use std::path::PathBuf;

/// The seat's primary GPU device path, if udev reports one.
pub fn primary(seat: &str) -> Option<PathBuf> {
    primary_gpu(seat).ok().flatten()
}

/// Every GPU device path on the seat.
pub fn all(seat: &str) -> Vec<PathBuf> {
    all_gpus(seat).unwrap_or_default()
}

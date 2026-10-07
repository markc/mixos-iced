// SPDX-License-Identifier: MIT OR Apache-2.0
//! Shared headless settings contract and consumer. No renderer or owned transport.
#[cfg(feature = "cache")]
pub mod cache;
pub mod consumer;
pub mod domains;
pub mod fallback;
pub mod model;
#[cfg(feature = "native")]
pub mod native;
pub mod reducer;
pub mod resolve;
pub mod session;
pub use design::EMBEDDED_DEFAULT_SOURCE;
pub use model::*;
pub use resolve::{describe, resolve, resolve_with_embedded};

pub const CONTRACT_VERSION: &str = "0.1.0";
pub const SCHEMA: u32 = 1;
pub const MAX_SOURCE_BYTES: usize = 256 * 1024;
pub const MAX_SNAPSHOT_BYTES: usize = 1024 * 1024 - 64 * 1024;
pub const MAX_RECEIPTS: usize = 128;

/// Registered service ownership is settingsd, independent of profile name.
pub fn topic(profile: &str) -> String {
    format!("settingsd.desktop.changed.{profile}")
}

pub fn digest<T: serde::Serialize>(value: &T) -> Result<String, serde_json::Error> {
    Ok(blake3::hash(&serde_json::to_vec(value)?)
        .to_hex()
        .to_string())
}
pub fn source_digest(source: &str) -> String {
    blake3::hash(source.as_bytes()).to_hex().to_string()
}

// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pinned, immutable sets of fonts, icons and emoji on the local disk. No
//! downloader, daemon or storage service: an installer publishes a set as
//! `<root>/sets/<id>` and points `<root>/current` at it; this crate finds,
//! checks and reads it.
//!
//! A set is a directory holding a strict-data manifest
//! ([`MANIFEST_FILE`], schema [`SCHEMA`] or the versioned icon-metadata
//! schema [`SCHEMA_V2`]), the generated stylesheet ([`STYLESHEET_FILE`])
//! and the locked files, each with a size, a SHA-256 and a BLAKE3.
//! Opening a set checks the layout (no symlinks, no escapes), the
//! manifest and every size; [`AssetSet::verify`] checks the hashes.
//!
//! - [`Lookup`] is the generic resolver: the caller names the roots to
//!   search (an XDG subdirectory, an environment variable with a default,
//!   or explicit paths) and [`Lookup::discover`] selects the first
//!   activated set, following `current` exactly once, and verifies its
//!   hashes; [`Lookup::select`] does the same without hashing the
//!   payload, for a reader that trusts the installer's verification.
#![cfg_attr(
    feature = "mixos",
    doc = "- [`mixos`] (the default `mixos` feature) is the MixOS search path:"
)]
#![cfg_attr(
    feature = "mixos",
    doc = "  `mixos/assets` under the XDG data directories, then `assets` under the"
)]
#![cfg_attr(feature = "mixos", doc = "  share directory `config` resolves.")]
#![cfg_attr(
    feature = "verified",
    doc = "- [`AssetSet::read_verified`] (the `verified` feature) re-reads the"
)]
#![cfg_attr(
    feature = "verified",
    doc = "  manifest through a held directory descriptor and captures every"
)]
#![cfg_attr(
    feature = "verified",
    doc = "  locked file's bytes, checked once and owned: the returned"
)]
#![cfg_attr(
    feature = "verified",
    doc = "  `VerifiedSet` never touches the paths again, and its identity pins"
)]
#![cfg_attr(
    feature = "verified",
    doc = "  the set ID together with the BLAKE3 of the exact manifest bytes."
)]
#![cfg_attr(
    feature = "verified",
    doc = "- [`VerifiedSet::read_at`] and [`VerifiedSet::read_explicit`] (the `verified`"
)]
#![cfg_attr(
    feature = "verified",
    doc = "  feature) resolve an [`ExplicitRequest`] — a set ID with an optional"
)]
#![cfg_attr(
    feature = "verified",
    doc = "  manifest digest — under held approved root descriptors, selecting"
)]
#![cfg_attr(
    feature = "verified",
    doc = "  `sets/<id>` directly and never `current`: an absent set falls through,"
)]
#![cfg_attr(
    feature = "verified",
    doc = "  an encountered invalid or digest-mismatched set is a diagnostic."
)]
#![cfg_attr(
    feature = "verified",
    doc = "- [`VerifiedSet::read_current`] (the `verified` feature) is the"
)]
#![cfg_attr(
    feature = "verified",
    doc = "  descriptor-owned analogue of [`AssetSet::current`]: the initial"
)]
#![cfg_attr(
    feature = "verified",
    doc = "  omitted-resource selection follows `current` exactly once under a held"
)]
#![cfg_attr(
    feature = "verified",
    doc = "  root descriptor. Only that selection consults `current`; an"
)]
#![cfg_attr(
    feature = "verified",
    doc = "  expected-binding request uses `read_at`, which never does."
)]
//!
//! Native readers call `discover` once at startup and keep that selection
//! while `current` changes underneath them. An installer opens its own
//! root with [`AssetSet::open`] and calls `verify` before activating a set.
//!
//! ```no_run
//! let set = assets::Lookup::new()
//!     .xdg("example/assets")
//!     .root_from_env("EXAMPLE_ASSETS", "/usr/share/example/assets")
//!     .discover()?
//!     .expect("an activated set");
//! let sans = set.font_path("sans");
//! let delete = set.icon("delete");
//! # Ok::<(), assets::Error>(())
//! ```

mod error;
mod lookup;
mod manifest;
mod set;
#[cfg(feature = "verified")]
mod verified;

#[cfg(feature = "mixos")]
pub mod mixos;

pub use error::{Error, Result};
pub use lookup::{Lookup, XdgData};
pub use manifest::{
    AssetFile, DEFAULT_ICON_STYLE, IconAsset, IconCatalogue, IconDefault, MANIFEST_FILE,
    MAX_FACE_INDEX, MAX_FAMILY_BYTES, MAX_FILE_BYTES, MAX_FILES, MAX_ICON_ASSETS,
    MAX_ICON_CATALOGUES, MAX_ROLES, Manifest, ManifestV2, SCHEMA, SCHEMA_V2, STYLESHEET_FILE,
    valid_relative_path, valid_set_id,
};
pub use set::{AssetSet, CURRENT_LINK};
#[cfg(feature = "verified")]
pub use verified::{
    ExplicitRequest, ReadLimits, ResolvedCatalogue, SetIdentity, VerifiedFile, VerifiedSet,
};

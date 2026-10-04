// SPDX-License-Identifier: MIT OR Apache-2.0

//! Pinned, immutable sets of fonts, icons and emoji on the local disk. No
//! downloader, daemon or storage service: an installer publishes a set as
//! `<root>/sets/<id>` and points `<root>/current` at it; this crate finds,
//! checks and reads it.
//!
//! A set is a directory holding a strict-data manifest
//! ([`MANIFEST_FILE`], schema [`SCHEMA`]), the generated stylesheet
//! ([`STYLESHEET_FILE`]) and the locked files, each with a size, a
//! SHA-256 and a BLAKE3. Opening a set checks the layout (no symlinks, no
//! escapes), the manifest and every size; [`AssetSet::verify`] checks the
//! hashes.
//!
//! - [`Lookup`] is the generic resolver: the caller names the roots to
//!   search (an XDG subdirectory, an environment variable with a default,
//!   or explicit paths) and [`Lookup::discover`] selects the first
//!   activated set, following `current` exactly once.
//! - [`mixos`] (the default `mixos` feature) is the MixOS search path:
//!   `mixos/assets` under the XDG data directories, then `assets` under the
//!   share directory `config` resolves.
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

#[cfg(feature = "mixos")]
pub mod mixos;

pub use error::{Error, Result};
pub use lookup::{Lookup, XdgData};
pub use manifest::{
    AssetFile, MANIFEST_FILE, MAX_FILE_BYTES, MAX_FILES, MAX_ROLES, Manifest, SCHEMA,
    STYLESHEET_FILE, valid_relative_path, valid_set_id,
};
pub use set::{AssetSet, CURRENT_LINK};

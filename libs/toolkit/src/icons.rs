// SPDX-License-Identifier: MIT OR Apache-2.0
//! Icon lookup beyond the icon font: [`freedesktop`] resolves a themed icon
//! name or a desktop-entry ID to an image file on disk, for application
//! launchers, task lists and anything else that shows other programs' icons.
//! [`assets`] decodes owned, verified image bytes (`decode_owned`) and offers
//! the caller-owned [`Assets`] catalogue and renderer-ready [`Ready`] icons.

pub mod freedesktop;

#[cfg(feature = "image")]
pub mod assets;
#[cfg(feature = "image")]
pub use assets::{Assets, DecodedIcon, IconDecodeError, ImageFormat, Ready, decode_owned};

// SPDX-License-Identifier: MIT OR Apache-2.0

//! What can go wrong while finding, opening or verifying a set.

use std::path::PathBuf;

/// Why a set could not be found, opened or verified.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The file system refused a read or an inspection.
    #[error("{action} {}: {source}", path.display())]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The manifest is not strict data of the expected shape.
    #[error("parse {} as strict data: {source}", path.display())]
    Manifest {
        path: PathBuf,
        #[source]
        source: strict::Error,
    },
    /// The layout, the manifest or the activation link breaks a rule.
    #[error("{0}")]
    Invalid(String),
    /// A locked file's size or hash differs from the manifest.
    #[error("{0}")]
    Mismatch(String),
}

/// `std::result::Result` with this crate's [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;

pub(crate) fn io(action: &'static str, path: &std::path::Path) -> impl FnOnce(std::io::Error) -> Error {
    let path = path.to_path_buf();
    move |source| Error::Io { action, path, source }
}

pub(crate) fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}

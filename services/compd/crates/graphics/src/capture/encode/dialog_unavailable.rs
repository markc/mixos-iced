// SPDX-License-Identifier: MIT OR Apache-2.0
//! Save As compatibility is explicitly unavailable without an enabled adapter.
//! The ordinary native Save action still writes to the configured default path.

use std::path::PathBuf;

pub fn available() -> bool {
    false
}

pub fn save_file_dialog(_title: &str, _suggested_name: &str) -> Option<PathBuf> {
    warn!("capture Save As is unavailable: no native requester adapter is connected");
    None
}

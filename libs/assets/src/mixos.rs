// SPDX-License-Identifier: MIT OR Apache-2.0

//! The MixOS search path (the default `mixos` feature).
//!
//! Order: `$XDG_DATA_HOME/mixos/assets`, each `$XDG_DATA_DIRS` entry's
//! `mixos/assets`, then `assets` under the share directory `config`
//! resolves (`$MIXOS_SHARE`, default `/opt/mixos/share`). Unset or empty
//! XDG variables take their defaults; relative entries are ignored.

use std::path::Path;

use config::Dir;

use crate::error::Result;
use crate::lookup::{Lookup, XdgData};
use crate::set::AssetSet;

/// The subdirectory under each XDG data directory.
pub const XDG_SUBDIR: &str = "mixos/assets";

/// The subdirectory under the share directory.
pub const SHARE_SUBDIR: &str = "assets";

/// The MixOS lookup, from the process environment.
pub fn lookup() -> Lookup {
    lookup_in(&XdgData::current(), &config::path(Dir::Share))
}

/// The MixOS lookup for explicit inputs: `xdg` and the resolved share
/// directory (what `config::path(Dir::Share)` returns).
pub fn lookup_in(xdg: &XdgData, share: &Path) -> Lookup {
    Lookup::new()
        .xdg_in(XDG_SUBDIR, xdg)
        .root(share.join(SHARE_SUBDIR))
}

/// Select, open and verify the activated MixOS set once, at startup.
pub fn discover() -> Result<Option<AssetSet>> {
    lookup().discover()
}

/// Select and open the activated MixOS set without hashing its payload
/// ([`Lookup::select`]): layout, manifest and sizes checked, the
/// installer's verification trusted for the bytes.
pub fn select() -> Result<Option<AssetSet>> {
    lookup().select()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::path::PathBuf;

    #[test]
    fn search_order_is_user_then_distribution_then_share() {
        let xdg = XdgData {
            data_home: Some(PathBuf::from("/u/.local/share")),
            data_dirs: Some(OsString::from("/usr/local/share:/usr/share")),
            home: Some(PathBuf::from("/u")),
        };
        let lookup = lookup_in(&xdg, Path::new("/opt/mixos/share"));
        assert_eq!(
            lookup.roots(),
            [
                PathBuf::from("/u/.local/share/mixos/assets"),
                PathBuf::from("/usr/local/share/mixos/assets"),
                PathBuf::from("/usr/share/mixos/assets"),
                PathBuf::from("/opt/mixos/share/assets"),
            ]
        );
    }
}

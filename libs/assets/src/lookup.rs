// SPDX-License-Identifier: MIT OR Apache-2.0

//! Where a reader looks for an activated set.
//!
//! A [`Lookup`] is an ordered list of asset roots, each a directory that
//! may hold `current -> sets/<id>`. The caller names them: the XDG data
//! directories under a subdirectory of its choosing, an environment
//! variable with a default, or any explicit path. Nothing here knows which
//! project is asking.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::set::AssetSet;

/// The XDG base-directory inputs a lookup reads. [`XdgData::current`]
/// takes them from the process; a test fills the fields by hand.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct XdgData {
    /// `$XDG_DATA_HOME`; defaults to `$HOME/.local/share`.
    pub data_home: Option<PathBuf>,
    /// `$XDG_DATA_DIRS`, `:`-separated; defaults to
    /// `/usr/local/share:/usr/share`.
    pub data_dirs: Option<OsString>,
    /// `$HOME`.
    pub home: Option<PathBuf>,
}

impl XdgData {
    /// The process environment. An empty variable is unset.
    pub fn current() -> Self {
        let var = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
        Self {
            data_home: var("XDG_DATA_HOME").map(PathBuf::from),
            data_dirs: var("XDG_DATA_DIRS"),
            home: var("HOME").map(PathBuf::from),
        }
    }

    /// The data directories in search order: `data_home` (or its default
    /// under `home`) first, then each absolute entry of `data_dirs` (or
    /// the XDG defaults). Relative entries are ignored, as the XDG base
    /// directory specification requires.
    pub fn data_directories(&self) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        match self.data_home.as_ref().filter(|p| p.is_absolute()) {
            Some(data_home) => dirs.push(data_home.clone()),
            None => {
                if let Some(home) = self.home.as_ref().filter(|p| p.is_absolute()) {
                    dirs.push(home.join(".local/share"));
                }
            }
        }
        let defaults = OsString::from("/usr/local/share:/usr/share");
        let data_dirs = self
            .data_dirs
            .as_ref()
            .filter(|dirs| !dirs.is_empty())
            .unwrap_or(&defaults);
        dirs.extend(std::env::split_paths(data_dirs).filter(|dir| dir.is_absolute()));
        dirs
    }
}

/// An ordered, duplicate-free list of asset roots to search.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lookup {
    roots: Vec<PathBuf>,
}

impl Lookup {
    /// No roots yet.
    pub const fn new() -> Self {
        Self { roots: Vec::new() }
    }

    /// Append `<data dir>/<subdir>` for every XDG data directory of the
    /// process environment, user directory first. `subdir` is relative,
    /// for example `example/assets`.
    pub fn xdg(self, subdir: impl AsRef<Path>) -> Self {
        self.xdg_in(subdir, &XdgData::current())
    }

    /// [`xdg`](Self::xdg) with explicit inputs.
    pub fn xdg_in(mut self, subdir: impl AsRef<Path>, xdg: &XdgData) -> Self {
        let subdir = subdir.as_ref();
        debug_assert!(subdir.is_relative(), "the XDG subdirectory must be relative");
        for dir in xdg.data_directories() {
            self.push(dir.join(subdir));
        }
        self
    }

    /// Append one root.
    pub fn root(mut self, path: impl Into<PathBuf>) -> Self {
        self.push(path.into());
        self
    }

    /// Append the absolute path in `$var`, or `default` when the variable
    /// is unset, empty or relative.
    pub fn root_from_env(self, var: &str, default: impl Into<PathBuf>) -> Self {
        let configured = std::env::var_os(var)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .filter(|path| path.is_absolute());
        self.root(configured.unwrap_or_else(|| default.into()))
    }

    /// The roots in search order, each listed once.
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Select the first root with an activated set, open it and verify its
    /// bytes once: [`select`](Self::select) followed by
    /// [`AssetSet::verify`]. A root without `current` falls through; a root
    /// whose `current` is dangling or whose set is malformed is an error,
    /// so a broken override is reported rather than silently replaced by a
    /// system set.
    pub fn discover(&self) -> Result<Option<AssetSet>> {
        let set = self.select()?;
        if let Some(set) = &set {
            set.verify()?;
        }
        Ok(set)
    }

    /// Select and open the first root's activated set without hashing its
    /// payload: the layout, manifest, every size and the stylesheet text
    /// are checked, the SHA-256 and BLAKE3 of each file are not. For a
    /// reader at startup that trusts the installer's verification (a
    /// compositor loading tens of megabytes of fonts on every start). The
    /// same fall-through and error rules as [`discover`](Self::discover).
    pub fn select(&self) -> Result<Option<AssetSet>> {
        for root in &self.roots {
            if let Some(set) = AssetSet::current(root)? {
                return Ok(Some(set));
            }
        }
        Ok(None)
    }

    fn push(&mut self, root: PathBuf) {
        if !self.roots.contains(&root) {
            self.roots.push(root);
        }
    }
}

impl FromIterator<PathBuf> for Lookup {
    fn from_iter<I: IntoIterator<Item = PathBuf>>(roots: I) -> Self {
        let mut seen = BTreeSet::new();
        Self {
            roots: roots
                .into_iter()
                .filter(|root| seen.insert(root.clone()))
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xdg_order_is_home_then_dirs_and_relative_entries_are_dropped() {
        let xdg = XdgData {
            data_home: Some(PathBuf::from("/u/data")),
            data_dirs: Some(OsString::from("relative:/opt/share:/usr/share")),
            home: Some(PathBuf::from("/u")),
        };
        assert_eq!(
            xdg.data_directories(),
            vec![
                PathBuf::from("/u/data"),
                PathBuf::from("/opt/share"),
                PathBuf::from("/usr/share"),
            ]
        );
    }

    #[test]
    fn unset_empty_or_relative_variables_use_the_xdg_defaults() {
        let xdg = XdgData {
            data_home: Some(PathBuf::from("relative")),
            data_dirs: Some(OsString::new()),
            home: Some(PathBuf::from("/u")),
        };
        assert_eq!(
            xdg.data_directories(),
            vec![
                PathBuf::from("/u/.local/share"),
                PathBuf::from("/usr/local/share"),
                PathBuf::from("/usr/share"),
            ]
        );
        assert_eq!(
            XdgData::default().data_directories(),
            vec![PathBuf::from("/usr/local/share"), PathBuf::from("/usr/share")]
        );
    }

    #[test]
    fn roots_keep_order_and_drop_duplicates() {
        let xdg = XdgData {
            data_home: Some(PathBuf::from("/u/data")),
            data_dirs: Some(OsString::from("/usr/share:/u/data")),
            home: None,
        };
        let lookup = Lookup::new()
            .xdg_in("example/assets", &xdg)
            .root("/srv/example/assets")
            .root("/usr/share/example/assets");
        assert_eq!(
            lookup.roots(),
            [
                PathBuf::from("/u/data/example/assets"),
                PathBuf::from("/usr/share/example/assets"),
                PathBuf::from("/srv/example/assets"),
            ]
        );
        let collected: Lookup = lookup.roots().iter().cloned().collect();
        assert_eq!(collected, lookup);
    }

    #[test]
    fn an_empty_lookup_finds_nothing() {
        assert!(Lookup::new().discover().unwrap().is_none());
    }
}

// SPDX-License-Identifier: MIT OR Apache-2.0
//! App-owned filenames over config's shared per-application directories.
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;

pub const COMPONENT: &str = "ced";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppDirs(config::AppDirs);

impl std::ops::Deref for AppDirs {
    type Target = config::AppDirs;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AppDirs {
    pub fn resolve(component: &str) -> Option<Self> {
        config::AppDirs::resolve(component).map(Self)
    }
    pub fn resolve_with(component: &str, get: impl Fn(&str) -> Option<PathBuf>) -> Option<Self> {
        config::AppDirs::resolve_with(component, get).map(Self)
    }
    pub fn config_file(&self) -> PathBuf {
        self.config().join("ced.conf.mix")
    }
    /// Per-app theme override (layered over the shared `theme.conf.mix`).
    pub fn theme_override(&self) -> PathBuf {
        self.config().join("theme.conf.mix")
    }
    pub fn macros_dir(&self) -> PathBuf {
        self.config().join("macros")
    }
    pub fn session_file(&self) -> PathBuf {
        self.state().join("session.json")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<PathBuf> {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| PathBuf::from(v))
        }
    }

    #[test]
    fn resolution_order() {
        let all = &[
            ("MIXOS_APP_HOME", "/a"),
            ("MIXOS_APPS_HOME", "/b"),
            ("XDG_STATE_HOME", "/c"),
            ("HOME", "/h"),
        ];
        assert_eq!(
            AppDirs::resolve_with("ced", env(all)).unwrap().root(),
            Path::new("/a")
        );
        let three = &[
            ("MIXOS_APPS_HOME", "/b"),
            ("XDG_STATE_HOME", "/c"),
            ("HOME", "/h"),
        ];
        assert_eq!(
            AppDirs::resolve_with("ced", env(three)).unwrap().root(),
            Path::new("/b/ced")
        );
        let two = &[("XDG_STATE_HOME", "/c"), ("HOME", "/h")];
        assert_eq!(
            AppDirs::resolve_with("ced", env(two)).unwrap().root(),
            Path::new("/c/mixos/apps/ced")
        );
        let one = &[("HOME", "/h")];
        let d = AppDirs::resolve_with("ced", env(one)).unwrap();
        assert_eq!(d.root(), Path::new("/h/.local/state/mixos/apps/ced"));
        assert_eq!(
            d.session_file(),
            Path::new("/h/.local/state/mixos/apps/ced/state/session.json")
        );
        assert_eq!(
            d.config_file(),
            Path::new("/h/.local/state/mixos/apps/ced/config/ced.conf.mix")
        );
    }

    #[test]
    fn relative_values_and_bad_slugs_are_refused() {
        let rel = &[("MIXOS_APP_HOME", "rel"), ("HOME", "/h")];
        assert_eq!(
            AppDirs::resolve_with("ced", env(rel)).unwrap().root(),
            Path::new("/h/.local/state/mixos/apps/ced")
        );
        assert!(AppDirs::resolve_with("../x", env(&[("HOME", "/h")])).is_none());
        assert!(AppDirs::resolve_with("", env(&[("HOME", "/h")])).is_none());
    }
}

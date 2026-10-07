// SPDX-License-Identifier: MIT OR Apache-2.0
//! Shared per-application roots, preserving Ced and Dopus's existing paths.
//! Resolution performs no I/O and never creates a directory.
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppDirs {
    root: PathBuf,
}

impl AppDirs {
    pub fn resolve(component: &str) -> Option<Self> {
        Self::resolve_with(component, |key| std::env::var_os(key).map(PathBuf::from))
    }

    /// First absolute input wins: app override, apps parent, isolated Var,
    /// XDG state, then home state. Invalid slugs and absent roots return None.
    pub fn resolve_with(component: &str, get: impl Fn(&str) -> Option<PathBuf>) -> Option<Self> {
        if component.is_empty()
            || component == "."
            || component == ".."
            || component.contains(['/', '\\'])
        {
            return None;
        }
        let mut parts = Path::new(component).components();
        if !matches!(
            (parts.next(), parts.next()),
            (Some(Component::Normal(_)), None)
        ) {
            return None;
        }
        let absolute = |path: PathBuf| path.is_absolute().then_some(path);
        let root = get("MIXOS_APP_HOME")
            .and_then(absolute)
            .or_else(|| {
                get("MIXOS_APPS_HOME")
                    .and_then(absolute)
                    .map(|p| p.join(component))
            })
            .or_else(|| {
                get("MIXOS_VAR")
                    .and_then(absolute)
                    .or_else(|| get("MIXOS").and_then(absolute).map(|p| p.join("var")))
                    .map(|p| p.join("apps").join(component))
            })
            .or_else(|| {
                get("XDG_STATE_HOME")
                    .and_then(absolute)
                    .map(|p| p.join("mixos/apps").join(component))
            })
            .or_else(|| {
                get("HOME")
                    .and_then(absolute)
                    .map(|p| p.join(".local/state/mixos/apps").join(component))
            })?;
        Some(Self { root })
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn config(&self) -> PathBuf {
        self.root.join("config")
    }
    pub fn state(&self) -> PathBuf {
        self.root.join("state")
    }
    pub fn cache(&self) -> PathBuf {
        self.root.join("cache")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isolated_roots_take_precedence_without_relocating_existing_user_state() {
        let vars = [
            ("MIXOS_APP_HOME", "/one"),
            ("MIXOS_APPS_HOME", "/two"),
            ("MIXOS_VAR", "/three"),
            ("MIXOS", "/four"),
            ("XDG_STATE_HOME", "/five"),
            ("HOME", "/six"),
        ];
        let expected = [
            "/one",
            "/two/ced",
            "/three/apps/ced",
            "/four/var/apps/ced",
            "/five/mixos/apps/ced",
            "/six/.local/state/mixos/apps/ced",
        ];
        for (offset, expected) in expected.into_iter().enumerate() {
            let dirs = AppDirs::resolve_with("ced", |key| {
                vars[offset..]
                    .iter()
                    .find(|(name, _)| *name == key)
                    .map(|(_, path)| PathBuf::from(path))
            })
            .unwrap();
            assert_eq!(dirs.root(), Path::new(expected));
            assert_eq!(dirs.cache(), Path::new(expected).join("cache"));
        }
    }

    #[test]
    fn absent_or_relative_roots_and_unsafe_component_names_never_escape() {
        for slug in [
            "",
            ".",
            "..",
            "../ced",
            "ced/../other",
            "ced\\other",
            "/ced",
        ] {
            assert!(AppDirs::resolve_with(slug, |_| Some(PathBuf::from("/owned"))).is_none());
        }
        assert!(AppDirs::resolve_with("ced", |_| None).is_none());
        assert!(AppDirs::resolve_with("ced", |_| Some(PathBuf::from("relative"))).is_none());
        let dirs = AppDirs::resolve_with("ced", |key| match key {
            "MIXOS_APP_HOME" => Some(PathBuf::from("relative")),
            "HOME" => Some(PathBuf::from("/home/user")),
            _ => None,
        })
        .unwrap();
        assert_eq!(
            dirs.root(),
            Path::new("/home/user/.local/state/mixos/apps/ced")
        );
    }
}

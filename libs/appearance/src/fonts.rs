// SPDX-License-Identifier: MIT OR Apache-2.0
//! The pinned asset set as toolkit font sources: the set's `sans`, `mono`,
//! `serif`, `display` and `emoji` roles become a [`FontSet`] of paths, and
//! its `icons` role with the catalogue beside it becomes the [`IconFont`].
//!
//! Nothing is read here but the set's manifest, which `assets` has already
//! checked: the font files are read by `toolkit::fonts::install`. Without a
//! set, the sources are empty and toolkit falls back to iced's generic
//! families; the [`FontOrigin`] says why, for the one startup log line.

use std::path::PathBuf;

use assets::{AssetSet, Lookup};
use toolkit::fonts::Role;
use toolkit::{FontSet, IconFont};

/// The set's role name for each toolkit role.
pub const ROLES: [(Role, &str); 5] = [
    (Role::Sans, "sans"),
    (Role::Mono, "mono"),
    (Role::Serif, "serif"),
    (Role::Display, "display"),
    (Role::Emoji, "emoji"),
];

/// The set's role whose font and `.codepoints` catalogue make the icon font.
pub const ICON_ROLE: &str = "icons";

/// Where the font sources came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FontOrigin {
    /// The activated set with this ID; `missing` lists the toolkit roles it
    /// does not provide (`"icons"` for a set without an icon font).
    Set { id: String, missing: Vec<&'static str> },
    /// No root has an activated set; the roots that were searched.
    NoSet { roots: Vec<PathBuf> },
    /// A root has an activated set that failed its checks.
    Unusable(String),
}

/// A [`FontSet`] and [`IconFont`] for `toolkit::fonts::install`, with the
/// origin to log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontSources {
    pub set: FontSet,
    pub icons: Option<IconFont>,
    pub origin: FontOrigin,
}

impl FontSources {
    /// Empty sources and the reason.
    pub fn none(origin: FontOrigin) -> Self {
        Self {
            set: FontSet::new(),
            icons: None,
            origin,
        }
    }

    /// A line for the caller's log when the look is not the pinned one:
    /// `None` when the set provided every role.
    pub fn warning(&self) -> Option<String> {
        match &self.origin {
            FontOrigin::Set { missing, .. } if missing.is_empty() => None,
            FontOrigin::Set { id, missing } => Some(format!(
                "asset set {id} has no {} font; iced's generic families fill in",
                missing.join(", ")
            )),
            FontOrigin::NoSet { roots } => Some(format!(
                "no asset set activated (none of {}); using iced's generic families",
                roots
                    .iter()
                    .map(|root| root.join(assets::CURRENT_LINK).display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            FontOrigin::Unusable(error) => Some(format!(
                "the asset set is unusable ({error}); using iced's generic families"
            )),
        }
    }
}

/// The sources from the activated MixOS set (`assets::mixos::select`: layout,
/// manifest and sizes checked, the installer's hashes trusted).
pub fn fonts() -> FontSources {
    fonts_in(&assets::mixos::lookup())
}

/// The sources from the first root in `lookup` with an activated set.
pub fn fonts_in(lookup: &Lookup) -> FontSources {
    match lookup.select() {
        Ok(Some(set)) => fonts_of(&set),
        Ok(None) => FontSources::none(FontOrigin::NoSet {
            roots: lookup.roots().to_vec(),
        }),
        Err(error) => FontSources::none(FontOrigin::Unusable(error.to_string())),
    }
}

/// The sources of one opened set.
pub fn fonts_of(set: &AssetSet) -> FontSources {
    let mut fonts = FontSet::new();
    let mut missing = Vec::new();
    for (role, name) in ROLES {
        match set.font_path(name) {
            Some(path) => {
                fonts = match role {
                    Role::Sans => fonts.sans(path),
                    Role::Mono => fonts.mono(path),
                    Role::Serif => fonts.serif(path),
                    Role::Display => fonts.display(path),
                    Role::Emoji => fonts.emoji(path),
                };
            }
            None => missing.push(name),
        }
    }
    let icons = set
        .font_path(ICON_ROLE)
        .map(|path| IconFont::new(path, set.icons().clone()));
    if icons.is_none() {
        missing.push(ICON_ROLE);
    }
    FontSources {
        set: fonts,
        icons,
        origin: FontOrigin::Set {
            id: set.set_id().to_owned(),
            missing,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_lookup_names_what_it_searched() {
        let lookup: Lookup = vec![PathBuf::from("/nonexistent/appearance-test")]
            .into_iter()
            .collect();
        let sources = fonts_in(&lookup);
        assert!(sources.set.is_empty());
        assert_eq!(sources.icons, None);
        assert_eq!(
            sources.origin,
            FontOrigin::NoSet {
                roots: vec![PathBuf::from("/nonexistent/appearance-test")]
            }
        );
        assert_eq!(
            sources.warning().unwrap(),
            "no asset set activated (none of /nonexistent/appearance-test/current); using iced's generic families"
        );
        assert_eq!(FontSources::none(FontOrigin::Unusable("x".into())).set, FontSet::new());
        assert!(
            FontSources::none(FontOrigin::Set {
                id: "s".into(),
                missing: Vec::new()
            })
            .warning()
            .is_none()
        );
    }

    #[test]
    fn every_toolkit_role_has_a_set_role() {
        assert_eq!(ROLES.map(|(role, _)| role), Role::ALL);
        assert_eq!(ROLES.map(|(role, name)| role.name() == name), [true; 5]);
    }
}

// SPDX-License-Identifier: MIT OR Apache-2.0
//! Adapt the verified installed asset set to caller-supplied toolkit fonts.
use assets::AssetSet;
use std::sync::OnceLock;
use toolkit::fonts::{FontSet, FontSource, IconFont, Role};

static INSTALLED: OnceLock<Result<Option<AssetSet>, String>> = OnceLock::new();

pub fn register_installed() -> Result<Option<&'static AssetSet>, &'static str> {
    match INSTALLED.get_or_init(|| {
        let Some(set) = assets::mixos::discover().map_err(|error| error.to_string())? else {
            return Ok(None);
        };
        let mut fonts = FontSet::new();
        let mut assigned = Vec::new();
        for role in Role::ALL {
            if let Some(path) = set.font_path(role.name()) {
                assigned.push(path.clone());
                let source = Some(FontSource::Path(path));
                match role {
                    Role::Sans => fonts.sans = source,
                    Role::Mono => fonts.mono = source,
                    Role::Serif => fonts.serif = source,
                    Role::Display => fonts.display = source,
                    Role::Emoji => fonts.emoji = source,
                }
            }
        }
        let icon = set.font_path("icons").map(|path| {
            assigned.push(path.clone());
            IconFont::new(path, set.icons().clone())
        });
        for path in set.font_paths() {
            if !assigned.contains(&path) {
                fonts = fonts.additional(path);
            }
        }
        let installed = toolkit::fonts::install(fonts, icon).map_err(|error| error.to_string())?;
        for role in Role::ALL {
            if let Some(expected) = set.family(role.name()) {
                let actual = installed.family(role).unwrap_or_default();
                if !actual.eq_ignore_ascii_case(expected) {
                    return Err(format!(
                        "{} font family: expected {expected:?}, got {actual:?}",
                        role.name()
                    ));
                }
            }
        }
        Ok(Some(set))
    }) {
        Ok(set) => Ok(set.as_ref()),
        Err(error) => Err(error.as_str()),
    }
}

pub fn material_icon(name: &str) -> Result<Option<(char, iced_core::Font)>, &'static str> {
    register_installed()?;
    Ok(toolkit::fonts::icon(name))
}

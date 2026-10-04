// SPDX-License-Identifier: MIT OR Apache-2.0
//! The font sources against a published set in a temporary directory, found
//! through the MixOS search path, and against no set at all.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use appearance::{FontOrigin, fonts_in, fonts_of};
use assets::{AssetSet, Lookup, MANIFEST_FILE, SCHEMA, XdgData};
use sha2::Digest;
use toolkit::fonts::Role;
use toolkit::FontSource;

/// A set `sets/<id>` under `root` with the given font roles (the file bytes
/// are stand-ins: the manifest is what the sources are built from) and,
/// when `icons` is set, an icon font with a two-entry catalogue.
fn fixture(root: &Path, id: &str, roles: &[&str], icons: bool) -> PathBuf {
    let dir = root.join("sets").join(id);
    fs::create_dir_all(dir.join("fonts")).unwrap();
    fs::create_dir_all(dir.join("icons")).unwrap();
    let mut files: Vec<(String, Vec<u8>)> = roles
        .iter()
        .map(|role| (format!("fonts/{role}.ttf"), format!("{role} bytes").into_bytes()))
        .collect();
    if icons {
        files.push(("icons/Symbols.ttf".into(), b"icon font".to_vec()));
        files.push((
            "icons/Symbols.codepoints".into(),
            b"delete e872\nfolder e2c7\n".to_vec(),
        ));
    }
    let mut entries = Vec::new();
    for (path, bytes) in &files {
        fs::write(dir.join(path), bytes).unwrap();
        entries.push(serde_json::json!({
            "path": path, "bytes": bytes.len(),
            "url": "https://example.org/font", "upstream": "https://example.org/",
            "revision": "pinned", "licence": "OFL-1.1",
            "sha256": hex::encode(sha2::Sha256::digest(bytes)),
            "blake3": blake3::hash(bytes).to_hex().to_string()
        }));
    }
    let mut fonts = serde_json::Map::new();
    let mut families = serde_json::Map::new();
    for role in roles {
        fonts.insert((*role).to_owned(), format!("fonts/{role}.ttf").into());
        families.insert((*role).to_owned(), format!("Fixture {role}").into());
    }
    if icons {
        fonts.insert("icons".into(), "icons/Symbols.ttf".into());
        families.insert("icons".into(), "Fixture Symbols".into());
    }
    let manifest = serde_json::json!({
        "schema": SCHEMA, "set_id": id,
        "fonts": fonts, "font_families": families,
        "files": entries, "web_css": "/* fixture */\n"
    });
    let text = strict::encode_pretty(&strict::from_json(&manifest)).unwrap();
    fs::write(dir.join(MANIFEST_FILE), text).unwrap();
    fs::write(dir.join("fonts.css"), "/* fixture */\n").unwrap();
    dir
}

fn activate(root: &Path, target: &str) {
    let current = root.join(assets::CURRENT_LINK);
    if fs::symlink_metadata(&current).is_ok() {
        fs::remove_file(&current).unwrap();
    }
    symlink(target, current).unwrap();
}

#[test]
fn a_full_set_fills_every_role_and_the_icon_font() {
    let temp = tempfile::tempdir().unwrap();
    fixture(
        temp.path(),
        "full",
        &["sans", "mono", "serif", "display", "emoji", "mono_italic"],
        true,
    );
    let set = AssetSet::open(temp.path(), "full").unwrap();
    let sources = fonts_of(&set);
    assert_eq!(
        sources.origin,
        FontOrigin::Set {
            id: "full".into(),
            missing: Vec::new()
        }
    );
    assert_eq!(sources.warning(), None);
    for role in Role::ALL {
        assert_eq!(
            sources.set.get(role),
            Some(&FontSource::Path(set.root().join(format!("fonts/{}.ttf", role.name())))),
            "{}",
            role.name()
        );
    }
    let icons = sources.icons.unwrap();
    assert_eq!(icons.font, FontSource::Path(set.root().join("icons/Symbols.ttf")));
    assert_eq!(icons.glyph("delete"), Some('\u{e872}'));
    assert_eq!(icons.glyph("folder"), Some('\u{e2c7}'));
    assert_eq!(icons.codepoints.len(), 2);
}

#[test]
fn a_partial_set_reports_the_roles_it_lacks() {
    let temp = tempfile::tempdir().unwrap();
    fixture(temp.path(), "partial", &["sans"], false);
    let set = AssetSet::open(temp.path(), "partial").unwrap();
    let sources = fonts_of(&set);
    assert!(sources.set.sans.is_some());
    assert!(sources.set.mono.is_none() && sources.set.emoji.is_none());
    assert_eq!(sources.icons, None);
    assert_eq!(
        sources.origin,
        FontOrigin::Set {
            id: "partial".into(),
            missing: vec!["mono", "serif", "display", "emoji", "icons"]
        }
    );
    assert_eq!(
        sources.warning().unwrap(),
        "asset set partial has no mono, serif, display, emoji, icons font; iced's generic families fill in"
    );
}

/// The MixOS search path, with explicit inputs: the user's XDG data home
/// wins over the share directory; a root without `current` falls through;
/// a dangling `current` is reported, not skipped.
#[test]
fn the_search_path_selects_the_activated_set() {
    let temp = tempfile::tempdir().unwrap();
    let user = temp.path().join("user");
    let share = temp.path().join("share");
    let xdg = XdgData {
        data_home: Some(user.clone()),
        data_dirs: Some(std::ffi::OsString::from(temp.path().join("dist"))),
        home: Some(temp.path().join("home")),
    };
    let lookup = assets::mixos::lookup_in(&xdg, &share);

    let none = fonts_in(&lookup);
    assert!(none.set.is_empty() && none.icons.is_none());
    assert_eq!(
        none.origin,
        FontOrigin::NoSet {
            roots: lookup.roots().to_vec()
        }
    );
    assert!(none.warning().unwrap().contains("share/assets/current"));

    let system = share.join("assets");
    fixture(&system, "system", &["sans", "mono"], true);
    activate(&system, "sets/system");
    let found = fonts_in(&lookup);
    assert!(matches!(&found.origin, FontOrigin::Set { id, .. } if id == "system"));
    assert!(found.set.sans.is_some() && found.icons.is_some());

    let personal = user.join("mixos/assets");
    fixture(&personal, "personal", &["sans"], false);
    activate(&personal, "sets/personal");
    assert!(matches!(
        fonts_in(&lookup).origin,
        FontOrigin::Set { id, .. } if id == "personal"
    ));

    activate(&personal, "sets/gone");
    let broken = fonts_in(&lookup);
    assert!(broken.set.is_empty());
    assert!(
        matches!(&broken.origin, FontOrigin::Unusable(error) if error.contains("gone")),
        "{:?}",
        broken.origin
    );
    assert!(broken.warning().unwrap().starts_with("the asset set is unusable"));

    let explicit: Lookup = vec![system].into_iter().collect();
    assert!(matches!(
        fonts_in(&explicit).origin,
        FontOrigin::Set { id, .. } if id == "system"
    ));
}

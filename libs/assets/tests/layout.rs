// SPDX-License-Identifier: MIT OR Apache-2.0
#![cfg(feature = "mixos")]

//! The installed layout, as `share/assets/install.mix` publishes it: the
//! roles, families, file paths and stylesheet of the `2026-10-04-core-3`
//! set, rebuilt in a temporary share directory with stand-in bytes, and
//! resolved through the MixOS search path. A second test opens the real
//! installation when this machine has one.

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use assets::{AssetSet, XdgData};
use sha2::Digest;

const SET_ID: &str = "2026-10-04-core-3";

/// Role → locked font file, as the lock names them.
const FONTS: &[(&str, &str)] = &[
    ("sans", "fonts/InterVariable.ttf"),
    ("serif", "fonts/NotoSerif.ttf"),
    ("mono", "fonts/JetBrainsMono.ttf"),
    ("icons", "icons/MaterialSymbolsRounded.ttf"),
    ("emoji", "emoji/NotoColorEmoji.ttf"),
    ("serif_italic", "fonts/NotoSerif-Italic.ttf"),
    ("mono_italic", "fonts/JetBrainsMono-Italic.ttf"),
    ("display", "fonts/Quicksand.ttf"),
];

/// Role → family name.
const FAMILIES: &[(&str, &str)] = &[
    ("sans", "Inter Variable"),
    ("serif", "Noto Serif"),
    ("serif_italic", "Noto Serif"),
    ("mono", "JetBrains Mono"),
    ("mono_italic", "JetBrains Mono"),
    ("icons", "Material Symbols Rounded"),
    ("emoji", "Noto Color Emoji"),
    ("display", "Quicksand"),
];

/// Every locked path with its upstream project and licence: the fonts,
/// their web forms, the icon catalogue and the licence texts.
const FILES: &[(&str, &str, &str)] = &[
    ("fonts/InterVariable.ttf", "https://github.com/rsms/inter", "OFL-1.1"),
    ("web/InterVariable.woff2", "https://github.com/rsms/inter", "OFL-1.1"),
    ("licences/Inter-OFL.txt", "https://github.com/rsms/inter", "OFL-1.1"),
    ("fonts/JetBrainsMono.ttf", "https://github.com/JetBrains/JetBrainsMono", "OFL-1.1"),
    ("fonts/JetBrainsMono-Italic.ttf", "https://github.com/JetBrains/JetBrainsMono", "OFL-1.1"),
    ("licences/JetBrainsMono-OFL.txt", "https://github.com/JetBrains/JetBrainsMono", "OFL-1.1"),
    ("fonts/NotoSerif.ttf", "https://github.com/google/fonts", "OFL-1.1"),
    ("fonts/NotoSerif-Italic.ttf", "https://github.com/google/fonts", "OFL-1.1"),
    ("licences/NotoSerif-OFL.txt", "https://github.com/google/fonts", "OFL-1.1"),
    ("icons/MaterialSymbolsRounded.ttf", "https://github.com/google/material-design-icons", "Apache-2.0"),
    ("web/MaterialSymbolsRounded.woff2", "https://github.com/google/material-design-icons", "Apache-2.0"),
    ("icons/MaterialSymbolsRounded.codepoints", "https://github.com/google/material-design-icons", "Apache-2.0"),
    ("licences/MaterialSymbols-Apache.txt", "https://github.com/google/material-design-icons", "Apache-2.0"),
    ("emoji/NotoColorEmoji.ttf", "https://github.com/googlefonts/noto-emoji", "OFL-1.1"),
    ("licences/NotoColorEmoji-OFL.txt", "https://github.com/googlefonts/noto-emoji", "OFL-1.1"),
    ("fonts/Quicksand.ttf", "https://github.com/google/fonts", "OFL-1.1"),
    ("licences/Quicksand-OFL.txt", "https://github.com/google/fonts", "OFL-1.1"),
    ("web/NotoColorEmoji-COLRv1.ttf", "https://github.com/googlefonts/noto-emoji", "OFL-1.1"),
];

const WEB_CSS: &str = "/* Relative URLs keep this stylesheet bound to its immutable asset set. */\n\
@font-face { font-family: \"MixOS Sans\"; src: url(\"web/InterVariable.woff2\") format(\"woff2\"); font-style: normal; font-weight: 100 900; font-display: swap; }\n\
@font-face { font-family: \"MixOS Display\"; src: url(\"fonts/Quicksand.ttf\") format(\"truetype\"); font-style: normal; font-weight: 300 700; font-display: swap; }\n\
@font-face { font-family: \"MixOS Emoji\"; src: url(\"web/NotoColorEmoji-COLRv1.ttf\") format(\"truetype\"); font-display: swap; }\n\
.mixos-symbol { font-family: \"MixOS Symbols Rounded\"; font-weight: 400; font-style: normal; font-size: 24px; line-height: 1; display: inline-block; white-space: nowrap; font-feature-settings: \"liga\"; font-variation-settings: \"FILL\" 0, \"wght\" 400, \"GRAD\" 0, \"opsz\" 24; }\n";

/// Publish the core-3 layout under `share/assets` with stand-in bytes and
/// the manifest written as the installer writes it (JSON syntax, which is
/// strict data with quoted keys).
fn publish(share: &Path) -> PathBuf {
    let root = share.join("assets");
    let dir = root.join("sets").join(SET_ID);
    let mut entries = Vec::new();
    for (path, upstream, licence) in FILES {
        let full = dir.join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        let bytes: Vec<u8> = if path.ends_with(".codepoints") {
            b"delete e872\nfolder e2c7\nhome e88a\n".to_vec()
        } else {
            format!("stand-in bytes for {path}\n").into_bytes()
        };
        fs::write(&full, &bytes).unwrap();
        entries.push(serde_json::json!({
            "path": path,
            "url": format!("{upstream}/raw/pinned/{}", path.rsplit('/').next().unwrap()),
            "revision": "9710da1eacb3be272583c3224dcb70f9da6eadbb",
            "upstream": upstream,
            "licence": licence,
            "bytes": bytes.len(),
            "sha256": hex::encode(sha2::Sha256::digest(&bytes)),
            "blake3": blake3::hash(&bytes).to_hex().to_string(),
        }));
    }
    let fonts: serde_json::Map<String, serde_json::Value> = FONTS
        .iter()
        .map(|(role, path)| ((*role).to_owned(), serde_json::json!(path)))
        .collect();
    let families: serde_json::Map<String, serde_json::Value> = FAMILIES
        .iter()
        .map(|(role, family)| ((*role).to_owned(), serde_json::json!(family)))
        .collect();
    let manifest = serde_json::json!({
        "schema": assets::SCHEMA,
        "set_id": SET_ID,
        "fonts": fonts,
        "files": entries,
        "web_css": WEB_CSS,
        "font_families": families,
    });
    fs::write(
        dir.join(assets::MANIFEST_FILE),
        serde_json::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
    fs::write(dir.join(assets::STYLESHEET_FILE), WEB_CSS).unwrap();
    symlink(format!("sets/{SET_ID}"), root.join(assets::CURRENT_LINK)).unwrap();
    root
}

#[test]
fn an_unprivileged_reader_resolves_the_installed_layout() {
    let temp = tempfile::tempdir().unwrap();
    let share = temp.path().join("opt/mixos/share");
    let root = publish(&share);
    // Nothing in the XDG directories: the share root is the one that answers.
    let xdg = XdgData {
        data_home: Some(temp.path().join("home/.local/share")),
        data_dirs: Some(OsString::from(temp.path().join("usr/share"))),
        home: Some(temp.path().join("home")),
    };
    let lookup = assets::mixos::lookup_in(&xdg, &share);
    assert_eq!(
        lookup.roots(),
        [
            temp.path().join("home/.local/share/mixos/assets"),
            temp.path().join("usr/share/mixos/assets"),
            root.clone(),
        ]
    );
    let set = lookup.discover().unwrap().expect("the activated set");
    assert_eq!(set.set_id(), SET_ID);
    assert_eq!(set.assets_root(), fs::canonicalize(&root).unwrap());
    assert_eq!(set.root(), set.assets_root().join("sets").join(SET_ID));
    assert_eq!(set.manifest().files.len(), FILES.len());

    let mut roles: Vec<&str> = FONTS.iter().map(|(role, _)| *role).collect();
    roles.sort_unstable();
    assert_eq!(set.roles().collect::<Vec<_>>(), roles);
    for (role, path) in FONTS {
        assert_eq!(set.font_path(role), Some(set.root().join(path)), "{role}");
    }
    for (role, family) in FAMILIES {
        assert_eq!(set.family(role), Some(*family), "{role}");
    }
    assert_eq!(set.font_paths().len(), FONTS.len());
    assert_eq!(set.icon("home"), Some('\u{e88a}'));
    assert_eq!(set.icon("delete"), Some('\u{e872}'));
    assert!(
        set.file_path("web/NotoColorEmoji-COLRv1.ttf")
            .unwrap()
            .unwrap()
            .is_file()
    );
    assert!(set.file_path("licences/Quicksand-OFL.txt").unwrap().is_some());
    assert_eq!(set.manifest().web_css, WEB_CSS);
    set.verify().unwrap();

    // A user override takes precedence once it exists, and the pinned
    // selection does not move.
    let user_root = temp.path().join("home/.local/share/mixos/assets");
    let user_set = publish(&temp.path().join("home/.local/share/mixos"));
    assert_eq!(user_set, user_root);
    assert_eq!(
        lookup.discover().unwrap().unwrap().assets_root(),
        fs::canonicalize(&user_root).unwrap()
    );
    assert_eq!(set.assets_root(), fs::canonicalize(&root).unwrap());
}

/// The real installation, when this machine has one. Skipped quietly
/// otherwise: the layout test above is the gate that always runs.
#[test]
fn the_real_installation_opens_and_verifies_when_present() {
    let root = config::path(config::Dir::Share).join(assets::mixos::SHARE_SUBDIR);
    if fs::symlink_metadata(root.join(assets::CURRENT_LINK)).is_err() {
        return;
    }
    let set = AssetSet::current(&root).unwrap().expect("an activated set");
    assert!(set.font_path("sans").is_some());
    assert!(set.font_path("icons").is_some());
    assert!(set.font_path("emoji").is_some());
    assert!(set.icon("home").is_some());
    set.verify().unwrap();
}

// SPDX-License-Identifier: MIT OR Apache-2.0

//! The public behaviour of a set: opening, pinning, lookup order, and the
//! refusals (tampering, symlinks, escapes, bad manifests).

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use assets::{AssetSet, Lookup, MANIFEST_FILE, SCHEMA, XdgData};
use sha2::Digest;

/// A small set `sets/<id>` under `root`: one sans font, one icon font with
/// a two-entry catalogue, the manifest as strict data and the stylesheet.
fn fixture(root: &Path, id: &str) -> PathBuf {
    let dir = root.join("sets").join(id);
    fs::create_dir_all(dir.join("fonts")).unwrap();
    fs::create_dir_all(dir.join("icons")).unwrap();
    let files = [
        ("fonts/Sans.ttf", b"font bytes".as_slice()),
        ("icons/Symbols.ttf", b"icon font".as_slice()),
        (
            "icons/Symbols.codepoints",
            b"delete e872\nfolder e2c7\n".as_slice(),
        ),
    ];
    let mut entries = Vec::new();
    for (path, bytes) in files {
        fs::write(dir.join(path), bytes).unwrap();
        entries.push(serde_json::json!({
            "path": path, "bytes": bytes.len(),
            "url": "https://example.org/font", "upstream": "https://example.org/",
            "revision": "pinned", "licence": "OFL-1.1",
            "sha256": hex::encode(sha2::Sha256::digest(bytes)),
            "blake3": blake3::hash(bytes).to_hex().to_string()
        }));
    }
    write_manifest(
        &dir,
        &serde_json::json!({
            "schema": SCHEMA, "set_id": id,
            "fonts": { "sans": "fonts/Sans.ttf", "icons": "icons/Symbols.ttf" },
            "font_families": { "sans": "Fixture Sans", "icons": "Fixture Symbols" },
            "files": entries, "web_css": "/* fixture */\n"
        }),
    );
    fs::write(dir.join("fonts.css"), "/* fixture */\n").unwrap();
    dir
}

/// Write a JSON tree as strict data, the way an installer would.
fn write_manifest(dir: &Path, json: &serde_json::Value) {
    let text = strict::encode_pretty(&strict::from_json(json)).unwrap();
    fs::write(dir.join(MANIFEST_FILE), text).unwrap();
}

fn activate(root: &Path, id: &str) {
    let current = root.join("current");
    if fs::symlink_metadata(&current).is_ok() {
        fs::remove_file(&current).unwrap();
    }
    symlink(format!("sets/{id}"), current).unwrap();
}

#[test]
fn published_roles_icons_and_allowlist_are_shared() {
    let temp = tempfile::tempdir().unwrap();
    let directory = fixture(temp.path(), "one");
    let set = AssetSet::open(temp.path(), "one").unwrap();
    set.verify().unwrap();
    assert_eq!(set.root(), fs::canonicalize(&directory).unwrap());
    assert_eq!(
        set.font_path("sans"),
        Some(set.root().join("fonts/Sans.ttf"))
    );
    assert_eq!(set.font_path("missing"), None);
    assert_eq!(set.family("sans"), Some("Fixture Sans"));
    assert_eq!(set.font_paths().len(), 2);
    assert_eq!(set.roles().collect::<Vec<_>>(), ["icons", "sans"]);
    assert_eq!(set.icon("delete"), Some('\u{e872}'));
    assert_eq!(set.icon("unknown"), None);
    assert_eq!(set.icons().len(), 2);
    assert!(set.file_path("fonts.css").unwrap().is_some());
    assert!(set.file_path(MANIFEST_FILE).unwrap().is_some());
    fs::write(directory.join("unlocked.txt"), "private").unwrap();
    assert!(set.file_path("unlocked.txt").unwrap().is_none());
    assert!(set.file_path("../unlocked.txt").is_err());
}

#[test]
fn selection_survives_current_activation_swap() {
    let temp = tempfile::tempdir().unwrap();
    let one = fixture(temp.path(), "one");
    fixture(temp.path(), "two");
    activate(temp.path(), "one");
    let selected = AssetSet::current(temp.path()).unwrap().unwrap();
    activate(temp.path(), "two");
    assert_eq!(selected.root(), fs::canonicalize(&one).unwrap());
    assert_eq!(selected.set_id(), "one");
    assert_eq!(
        AssetSet::current(temp.path()).unwrap().unwrap().set_id(),
        "two"
    );
    selected.verify().unwrap();
}

#[test]
fn lookup_respects_xdg_order_and_ignores_relative_entries() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let user = temp.path().join("user");
    let dist = temp.path().join("dist");
    let share = temp.path().join("share");
    let xdg = XdgData {
        data_home: Some(user.clone()),
        data_dirs: Some(std::env::join_paths([Path::new("relative"), dist.as_path()]).unwrap()),
        home: Some(home.clone()),
    };
    let lookup = Lookup::new()
        .xdg_in("example/assets", &xdg)
        .root(share.join("assets"));
    assert_eq!(
        lookup.roots(),
        [
            user.join("example/assets"),
            dist.join("example/assets"),
            share.join("assets")
        ]
    );
    assert!(lookup.discover().unwrap().is_none());
    fixture(&share.join("assets"), "system");
    activate(&share.join("assets"), "system");
    assert_eq!(lookup.discover().unwrap().unwrap().set_id(), "system");
    fixture(&dist.join("example/assets"), "distribution");
    activate(&dist.join("example/assets"), "distribution");
    assert_eq!(lookup.discover().unwrap().unwrap().set_id(), "distribution");
    fixture(&user.join("example/assets"), "personal");
    activate(&user.join("example/assets"), "personal");
    assert_eq!(lookup.discover().unwrap().unwrap().set_id(), "personal");

    let relative = XdgData {
        data_home: Some(PathBuf::from("relative")),
        ..xdg.clone()
    };
    assert_eq!(
        Lookup::new().xdg_in("example/assets", &relative).roots()[0],
        home.join(".local/share/example/assets")
    );
    let empty = XdgData {
        data_dirs: Some(OsString::new()),
        ..xdg
    };
    assert!(
        Lookup::new()
            .xdg_in("example/assets", &empty)
            .roots()
            .contains(&PathBuf::from("/usr/share/example/assets"))
    );
}

#[test]
fn malformed_or_dangling_override_does_not_fall_through() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("user");
    let root = data.join("example/assets");
    let share = temp.path().join("shared");
    fixture(&root, "broken");
    activate(&root, "broken");
    fs::write(root.join("sets/broken").join(MANIFEST_FILE), "{bad: true}").unwrap();
    fixture(&share.join("assets"), "valid");
    activate(&share.join("assets"), "valid");
    let xdg = XdgData {
        data_home: Some(data),
        data_dirs: Some(OsString::from("relative")),
        home: None,
    };
    let lookup = Lookup::new()
        .xdg_in("example/assets", &xdg)
        .root(share.join("assets"));
    assert!(lookup.discover().is_err());
    activate(&root, "absent");
    assert!(lookup.discover().is_err());
    // A manifest that is not strict data at all is an error too, not a
    // fall-through.
    fs::remove_dir_all(root.join("sets/broken")).unwrap();
    fixture(&root, "broken");
    activate(&root, "broken");
    fs::write(
        root.join("sets/broken").join(MANIFEST_FILE),
        "schema: $(cat /etc/passwd)\n",
    )
    .unwrap();
    let error = lookup.discover().unwrap_err();
    assert!(matches!(error, assets::Error::Manifest { .. }), "{error}");
}

#[test]
fn payload_tampering_and_stylesheet_changes_are_refused() {
    let temp = tempfile::tempdir().unwrap();
    let dir = fixture(temp.path(), "one");
    let set = AssetSet::open(temp.path(), "one").unwrap();
    fs::write(dir.join("fonts/Sans.ttf"), b"tampered!!").unwrap();
    assert!(set.verify().unwrap_err().to_string().contains("SHA-256"));
    fs::write(dir.join("fonts/Sans.ttf"), b"tampered!!!").unwrap();
    assert!(set.verify().unwrap_err().to_string().contains("size"));
    fs::write(dir.join("fonts.css"), "/* altered */\n").unwrap();
    assert!(AssetSet::open(temp.path(), "one").is_err());
}

#[test]
fn symlink_payloads_directories_and_activation_escapes_are_refused() {
    let temp = tempfile::tempdir().unwrap();
    let dir = fixture(temp.path(), "one");
    let outside = temp.path().join("outside.ttf");
    fs::write(&outside, b"font bytes").unwrap();
    let font = dir.join("fonts/Sans.ttf");
    fs::remove_file(&font).unwrap();
    symlink(&outside, &font).unwrap();
    assert!(AssetSet::open(temp.path(), "one").is_err());
    fs::remove_file(&font).unwrap();
    fs::write(&font, b"font bytes").unwrap();
    let fonts = dir.join("fonts");
    let renamed = dir.join("original-fonts");
    fs::rename(&fonts, &renamed).unwrap();
    symlink(&renamed, &fonts).unwrap();
    assert!(AssetSet::open(temp.path(), "one").is_err());
    symlink("../outside", temp.path().join("current")).unwrap();
    assert!(AssetSet::current(temp.path()).is_err());
    // A plain directory named `current` is not an activation link.
    fs::remove_file(temp.path().join("current")).unwrap();
    fs::create_dir(temp.path().join("current")).unwrap();
    assert!(AssetSet::current(temp.path()).is_err());
}

#[test]
fn schemas_duplicates_and_unlocked_roles_are_refused() {
    let temp = tempfile::tempdir().unwrap();
    let dir = fixture(temp.path(), "one");
    let original = strict::to_json(&strict::parse_file(&dir.join(MANIFEST_FILE)).unwrap());
    for bad in [
        "unknown-field",
        "duplicate",
        "unlocked-role",
        "escape",
        "wrong-id",
        "old-schema",
        "http-url",
    ] {
        let mut json = original.clone();
        match bad {
            "unknown-field" => json["unexpected"] = serde_json::json!(true),
            "duplicate" => {
                let duplicate = json["files"][0].clone();
                json["files"].as_array_mut().unwrap().push(duplicate);
            }
            "unlocked-role" => json["fonts"]["sans"] = serde_json::json!("fonts/Absent.ttf"),
            "escape" => json["files"][0]["path"] = serde_json::json!("../outside.ttf"),
            "old-schema" => json["schema"] = serde_json::json!("other.static-assets.v1"),
            "http-url" => json["files"][0]["url"] = serde_json::json!("http://example.org/font"),
            _ => json["set_id"] = serde_json::json!("other"),
        }
        write_manifest(&dir, &json);
        assert!(AssetSet::open(temp.path(), "one").is_err(), "{bad}");
    }
    let mut legacy = original;
    legacy.as_object_mut().unwrap().remove("font_families");
    write_manifest(&dir, &legacy);
    assert_eq!(
        AssetSet::open(temp.path(), "one").unwrap().family("sans"),
        None
    );
}

#[test]
fn select_skips_hashing_but_keeps_every_other_check() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("assets");
    fixture(&root, "core");
    activate(&root, "core");
    let lookup: Lookup = vec![root.clone()].into_iter().collect();
    assert_eq!(lookup.select().unwrap().unwrap().set_id(), "core");
    // Same size, different bytes: select accepts (the installer verified),
    // discover refuses.
    fs::write(root.join("sets/core/fonts/Sans.ttf"), b"font byteZ").unwrap();
    assert_eq!(lookup.select().unwrap().unwrap().set_id(), "core");
    assert!(matches!(lookup.discover().unwrap_err(), assets::Error::Mismatch(_)));
    // A size change fails both.
    fs::write(root.join("sets/core/fonts/Sans.ttf"), b"font bytes grown").unwrap();
    assert!(matches!(lookup.select().unwrap_err(), assets::Error::Mismatch(_)));
    assert!(matches!(lookup.discover().unwrap_err(), assets::Error::Mismatch(_)));
    // No activated set falls through to None either way.
    let empty: Lookup = vec![temp.path().join("nothing")].into_iter().collect();
    assert!(empty.select().unwrap().is_none());
    assert!(empty.discover().unwrap().is_none());
}

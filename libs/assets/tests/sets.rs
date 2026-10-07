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

/// One locked-file entry for the manifest, with the bytes written.
fn write_file(dir: &Path, relative: &str, bytes: &[u8]) -> serde_json::Value {
    let path = dir.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, bytes).unwrap();
    serde_json::json!({
        "path": relative, "bytes": bytes.len(),
        "url": "https://example.org/font", "upstream": "https://example.org/",
        "revision": "pinned", "licence": "OFL-1.1",
        "sha256": hex::encode(sha2::Sha256::digest(bytes)),
        "blake3": blake3::hash(bytes).to_hex().to_string(),
    })
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
    assert!(matches!(
        lookup.discover().unwrap_err(),
        assets::Error::Mismatch(_)
    ));
    // A size change fails both.
    fs::write(root.join("sets/core/fonts/Sans.ttf"), b"font bytes grown").unwrap();
    assert!(matches!(
        lookup.select().unwrap_err(),
        assets::Error::Mismatch(_)
    ));
    assert!(matches!(
        lookup.discover().unwrap_err(),
        assets::Error::Mismatch(_)
    ));
    // No activated set falls through to None either way.
    let empty: Lookup = vec![temp.path().join("nothing")].into_iter().collect();
    assert!(empty.select().unwrap().is_none());
    assert!(empty.discover().unwrap().is_none());
}

/// A v2 set: one sans font, two icon catalogues (rounded default, plus
/// outlined) with distinct codepoints files, and one symbolic SVG asset.
fn fixture_v2(root: &Path, id: &str) -> PathBuf {
    let dir = root.join("sets").join(id);
    let mut entries = Vec::new();
    for (path, bytes) in [
        ("fonts/Sans.ttf", b"font bytes".as_slice()),
        ("icons/Rounded.ttf", b"rounded font".as_slice()),
        (
            "icons/Rounded.codepoints",
            b"delete e872\nfolder e2c7\n".as_slice(),
        ),
        ("icons/Outlined.ttf", b"outlined font".as_slice()),
        ("icons/Outlined.codepoints", b"delete e900\n".as_slice()),
        ("icons/mark.svg", b"<svg/>".as_slice()),
    ] {
        fs::create_dir_all(dir.join(path).parent().unwrap()).unwrap();
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
            "schema": assets::SCHEMA_V2, "set_id": id,
            "fonts": { "sans": "fonts/Sans.ttf" },
            "font_families": { "sans": "Fixture Sans" },
            "files": entries, "web_css": "/* fixture */\n",
            "icon_default": { "family": "Fixture Symbols", "style": "rounded", "weight": 400 },
            "icon_catalogues": [
                { "family": "Fixture Symbols", "style": "rounded",
                  "font": "icons/Rounded.ttf", "face_index": 0,
                  "codepoints": "icons/Rounded.codepoints" },
                { "family": "Fixture Symbols", "style": "outlined",
                  "font": "icons/Outlined.ttf", "face_index": 1,
                  "codepoints": "icons/Outlined.codepoints" }
            ],
            "icon_assets": [
                { "name": "mark", "style": "rounded",
                  "path": "icons/mark.svg", "symbolic": true }
            ]
        }),
    );
    fs::write(dir.join("fonts.css"), "/* fixture */\n").unwrap();
    dir
}

#[test]
fn v2_declares_real_icon_metadata_and_selection() {
    let temp = tempfile::tempdir().unwrap();
    fixture_v2(temp.path(), "two");
    let set = AssetSet::open(temp.path(), "two").unwrap();
    set.verify().unwrap();
    // The legacy v1 accessors read the shared fields unchanged.
    assert_eq!(set.set_id(), "two");
    assert_eq!(set.family("sans"), Some("Fixture Sans"));
    assert_eq!(
        set.font_path("sans"),
        Some(set.root().join("fonts/Sans.ttf"))
    );
    assert_eq!(set.manifest().schema, assets::SCHEMA_V2);
    assert_eq!(set.manifest().files.len(), 6);
    assert_eq!(set.manifest().web_css, "/* fixture */\n");
    // The declared default: an omitted icon request uses this.
    let default = set.icon_default().unwrap();
    assert_eq!(
        (
            default.family.as_str(),
            default.style.as_str(),
            default.weight
        ),
        ("Fixture Symbols", "rounded", 400)
    );
    // The catalogues carry the exact font/face_index/codepoints mapping.
    let catalogues = set.icon_catalogues();
    assert_eq!(catalogues.len(), 2);
    assert_eq!(catalogues[0].family, "Fixture Symbols");
    assert_eq!(catalogues[0].style, "rounded");
    assert_eq!(catalogues[0].font, "icons/Rounded.ttf");
    assert_eq!(catalogues[0].face_index, 0);
    assert_eq!(catalogues[0].codepoints, "icons/Rounded.codepoints");
    assert_eq!(catalogues[1].style, "outlined");
    assert_eq!(catalogues[1].face_index, 1);
    // Explicit selection: declared pairs resolve, absent pairs are None.
    assert!(
        set.icon_catalogue("Fixture Symbols", "rounded")
            .unwrap()
            .is_some()
    );
    assert_eq!(
        set.icon_catalogue("Fixture Symbols", "outlined")
            .unwrap()
            .unwrap()
            .face_index,
        1
    );
    assert!(
        set.icon_catalogue("Fixture Symbols", "filled")
            .unwrap()
            .is_none()
    );
    assert!(set.icon_catalogue("Other", "rounded").unwrap().is_none());
    // The declared non-font assets.
    let assets = set.icon_assets();
    assert_eq!(assets.len(), 1);
    assert_eq!(assets[0].name, "mark");
    assert_eq!(assets[0].path, "icons/mark.svg");
    assert!(assets[0].symbolic);
    // The legacy icon table is the default catalogue.
    assert_eq!(set.icon("delete"), Some('\u{e872}'));
    assert_eq!(set.icon("folder"), Some('\u{e2c7}'));
    assert_eq!(set.icons().len(), 2);
}

#[test]
fn v1_derives_default_icon_metadata_and_refuses_nondefault_styles() {
    let temp = tempfile::tempdir().unwrap();
    fixture(temp.path(), "one");
    let set = AssetSet::open(temp.path(), "one").unwrap();
    // The v1 set derives its one default catalogue from the icons role.
    let default = set.icon_default().unwrap();
    assert_eq!(default.family, "Fixture Symbols");
    assert_eq!(default.style, assets::DEFAULT_ICON_STYLE);
    assert_eq!(default.weight, 400);
    let catalogues = set.icon_catalogues();
    assert_eq!(catalogues.len(), 1);
    assert_eq!(catalogues[0].font, "icons/Symbols.ttf");
    assert_eq!(catalogues[0].face_index, 0);
    assert_eq!(catalogues[0].codepoints, "icons/Symbols.codepoints");
    assert!(set.icon_assets().is_empty());
    // The default style selects the one catalogue.
    assert!(
        set.icon_catalogue("Fixture Symbols", "default")
            .unwrap()
            .is_some()
    );
    assert!(set.icon_catalogue("Other", "default").unwrap().is_none());
    // A nondefault style request is refused, not silently mapped.
    let error = set
        .icon_catalogue("Fixture Symbols", "rounded")
        .unwrap_err();
    assert!(matches!(error, assets::Error::Invalid(_)));
    assert!(error.to_string().contains("rounded"), "{error}");
    // The legacy table and roles are untouched.
    assert_eq!(set.icon("folder"), Some('\u{e2c7}'));
    assert_eq!(set.roles().collect::<Vec<_>>(), ["icons", "sans"]);
}

#[test]
fn v1_without_family_claims_labels_the_default_but_matches_no_family() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("sets/one");
    let entries = vec![
        write_file(&dir, "fonts/Sans.ttf", b"font bytes"),
        write_file(&dir, "icons/Symbols.ttf", b"icon font"),
        write_file(&dir, "icons/Symbols.codepoints", b"delete e872\n"),
    ];
    write_manifest(
        &dir,
        &serde_json::json!({
            "schema": SCHEMA, "set_id": "one",
            "fonts": { "sans": "fonts/Sans.ttf", "icons": "icons/Symbols.ttf" },
            "files": entries, "web_css": "/* fixture */\n"
        }),
    );
    fs::write(dir.join("fonts.css"), "/* fixture */\n").unwrap();
    let set = AssetSet::open(temp.path(), "one").unwrap();
    // v1 records no family claim: the default stays usable, but a family
    // request cannot be confirmed against metadata.
    let default = set.icon_default().unwrap();
    assert_eq!(default.family, "");
    assert_eq!(default.style, "default");
    assert!(
        set.icon_catalogue("Material Symbols", "default")
            .unwrap()
            .is_none()
    );
    assert!(set.icon_catalogue("", "default").unwrap().is_some());
}

#[test]
fn v2_refuses_incomplete_duplicate_and_unknown_icon_metadata() {
    let temp = tempfile::tempdir().unwrap();
    fixture_v2(temp.path(), "two");
    let dir = temp.path().join("sets/two");
    let original = strict::to_json(&strict::parse_file(&dir.join(MANIFEST_FILE)).unwrap());
    for bad in [
        "missing-default",
        "empty-catalogues",
        "duplicate-pair",
        "default-not-declared",
        "weight-zero",
        "weight-too-big",
        "face-too-big",
        "unlocked-font",
        "unlocked-codepoints",
        "unknown-default-field",
        "unknown-catalogue-field",
        "unknown-asset-field",
        "unknown-top-field",
        "duplicate-asset",
        "unlocked-asset",
        "bad-asset-name",
        "too-many-catalogues",
    ] {
        let mut json = original.clone();
        match bad {
            "missing-default" => {
                json.as_object_mut().unwrap().remove("icon_default");
            }
            "empty-catalogues" => json["icon_catalogues"] = serde_json::json!([]),
            "duplicate-pair" => {
                let duplicate = json["icon_catalogues"][0].clone();
                json["icon_catalogues"]
                    .as_array_mut()
                    .unwrap()
                    .push(duplicate);
            }
            "default-not-declared" => {
                json["icon_default"]["style"] = serde_json::json!("filled");
            }
            "weight-zero" => json["icon_default"]["weight"] = serde_json::json!(0),
            "weight-too-big" => json["icon_default"]["weight"] = serde_json::json!(1001),
            "face-too-big" => {
                json["icon_catalogues"][0]["face_index"] = serde_json::json!(65536);
            }
            "unlocked-font" => {
                json["icon_catalogues"][0]["font"] = serde_json::json!("fonts/Absent.ttf");
            }
            "unlocked-codepoints" => {
                json["icon_catalogues"][0]["codepoints"] =
                    serde_json::json!("icons/Absent.codepoints");
            }
            "unknown-default-field" => {
                json["icon_default"]["fill_axis"] = serde_json::json!(true);
            }
            "unknown-catalogue-field" => {
                json["icon_catalogues"][0]["fill_axis"] = serde_json::json!(true);
            }
            "unknown-asset-field" => {
                json["icon_assets"][0]["tint"] = serde_json::json!(true);
            }
            "unknown-top-field" => json["icon_extra"] = serde_json::json!(true),
            "duplicate-asset" => {
                let duplicate = json["icon_assets"][0].clone();
                json["icon_assets"].as_array_mut().unwrap().push(duplicate);
            }
            "unlocked-asset" => {
                json["icon_assets"][0]["path"] = serde_json::json!("icons/Absent.svg");
            }
            "bad-asset-name" => {
                json["icon_assets"][0]["name"] = serde_json::json!("bad name!");
            }
            "too-many-catalogues" => {
                let base = json["icon_catalogues"][0].clone();
                let many: Vec<serde_json::Value> = (0..33)
                    .map(|i| {
                        let mut catalogue = base.clone();
                        catalogue["style"] = serde_json::json!(format!("style{i}"));
                        catalogue
                    })
                    .collect();
                json["icon_catalogues"] = serde_json::json!(many);
            }
            _ => unreachable!("{bad}"),
        }
        write_manifest(&dir, &json);
        assert!(AssetSet::open(temp.path(), "two").is_err(), "{bad}");
    }
}

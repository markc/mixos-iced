// SPDX-License-Identifier: MIT OR Apache-2.0

//! `ui::font::default::base::install_in` against a fake asset set published
//! in a temporary directory, and without one. The set's font files are the
//! embedded Inter bytes under set paths, so every role declares the family
//! `Inter` and the manifest says so too.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use assets::AssetSet;
use iced_core::font::{Family as IcedFamily, Weight};
use iced_graphics::text::cosmic_text::fontdb::{Database, Family, Query, Source};
use sha2::Digest;
use ui::font::default::base::{FAMILY, INTER_FONT, Origin, install_in};

const SET_ID: &str = "2026-10-04-test-1";
const CSS: &str = "/* test set */\n";

/// Role → file inside the set. Every file is the embedded Inter.
const FONTS: &[(&str, &str)] = &[
    ("sans", "fonts/Sans.ttf"),
    ("mono", "fonts/Mono.ttf"),
    ("emoji", "emoji/Emoji.ttf"),
];

/// Publish the set under `<root>/sets/<id>` with `current` pointing at it,
/// the manifest as the installer writes it (JSON syntax, strict data).
fn publish(root: &Path) -> AssetSet {
    let dir = root.join("sets").join(SET_ID);
    let mut files = Vec::new();
    for (_, path) in FONTS {
        let full = dir.join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(&full, INTER_FONT).unwrap();
        files.push(serde_json::json!({
            "path": path,
            "url": "https://example.com/pinned/Inter.ttf",
            "revision": "66647c0bb",
            "upstream": "https://github.com/rsms/inter",
            "licence": "OFL-1.1",
            "bytes": INTER_FONT.len(),
            "sha256": hex::encode(sha2::Sha256::digest(INTER_FONT)),
            "blake3": blake3::hash(INTER_FONT).to_hex().to_string(),
        }));
    }
    let fonts: serde_json::Map<String, serde_json::Value> = FONTS
        .iter()
        .map(|(role, path)| ((*role).to_owned(), serde_json::json!(path)))
        .collect();
    let families: serde_json::Map<String, serde_json::Value> = FONTS
        .iter()
        .map(|(role, _)| ((*role).to_owned(), serde_json::json!(FAMILY)))
        .collect();
    let manifest = serde_json::json!({
        "schema": assets::SCHEMA,
        "set_id": SET_ID,
        "fonts": fonts,
        "font_families": families,
        "files": files,
        "web_css": CSS,
    });
    fs::write(dir.join(assets::MANIFEST_FILE), serde_json::to_string_pretty(&manifest).unwrap())
        .unwrap();
    fs::write(dir.join(assets::STYLESHEET_FILE), CSS).unwrap();
    symlink(format!("sets/{SET_ID}"), root.join(assets::CURRENT_LINK)).unwrap();
    let set = AssetSet::current(root).unwrap().expect("an activated set");
    set.verify().unwrap();
    set
}

/// The family and file of the face a generic family resolves to.
fn resolve(db: &Database, family: Family<'_>) -> Option<(String, Option<PathBuf>)> {
    let id = db.query(&Query {
        families: &[family],
        ..Query::default()
    })?;
    let face = db.face(id)?;
    let path = match &face.source {
        Source::File(path) | Source::SharedFile(path, _) => Some(path.clone()),
        Source::Binary(_) => None,
    };
    Some((face.families.first().map(|(name, _)| name.clone())?, path))
}

#[test]
fn a_set_supplies_the_generic_families() {
    let temp = tempfile::tempdir().unwrap();
    let set = publish(temp.path());
    let mut db = Database::new();

    let report = install_in(&mut db, Some(&set));

    assert_eq!(report.origin, Origin::Set(SET_ID.to_owned()));
    assert_eq!(
        report.fonts.iter().map(|font| (font.role.as_str(), font.family.as_str(), font.path.as_str())).collect::<Vec<_>>(),
        [("emoji", FAMILY, "emoji/Emoji.ttf"), ("mono", FAMILY, "fonts/Mono.ttf"), ("sans", FAMILY, "fonts/Sans.ttf")]
    );
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert!(report.mismatched.is_empty(), "{:?}", report.mismatched);
    assert_eq!(report.pruned, 0);
    for generic in [Family::SansSerif, Family::Monospace] {
        let (family, path) = resolve(&db, generic).expect("a face");
        assert_eq!(family, FAMILY);
        assert!(path.as_ref().is_some_and(|path| path.starts_with(set.root())), "{path:?} is not in the set");
    }
    // No serif role in this set: the generic serif stays unmapped and, with
    // no other faces, resolves to nothing. The report says so.
    assert_eq!(report.generic, [
        ("sans-serif", Some(FAMILY.to_owned())),
        ("monospace", Some(FAMILY.to_owned())),
        ("serif", None),
    ]);
    assert_eq!(report.unresolved().collect::<Vec<_>>(), ["serif"]);
    assert!(report.to_string().starts_with(&format!("set {SET_ID}: emoji={FAMILY} (emoji/Emoji.ttf)")), "{report}");
}

#[test]
fn host_copies_of_a_set_family_are_pruned() {
    let temp = tempfile::tempdir().unwrap();
    let set = publish(temp.path());
    // A "host-installed" copy of the same family outside the set, loaded
    // first as system fonts are.
    let host = temp.path().join("host-Inter.ttf");
    fs::write(&host, INTER_FONT).unwrap();
    let mut db = Database::new();
    db.load_font_file(&host).unwrap();
    // An embedded face of the same family (a binary source) is never pruned.
    db.load_font_data(INTER_FONT.to_vec());
    assert_eq!(db.len(), 2);

    let report = install_in(&mut db, Some(&set));

    assert_eq!(report.pruned, 1);
    assert_eq!(db.len(), 1 + FONTS.len());
    assert!(
        !db.faces().any(|face| matches!(&face.source, Source::File(p) | Source::SharedFile(p, _) if p == &host)),
        "the host copy survived"
    );
    let (_, path) = resolve(&db, Family::SansSerif).unwrap();
    assert!(path.is_some_and(|path| path.starts_with(set.root())));
}

#[test]
fn a_mismatched_family_is_reported_not_fatal() {
    let temp = tempfile::tempdir().unwrap();
    let set = publish(temp.path());
    // Rewrite the manifest's sans family to a name the file does not declare.
    let manifest_path = set.root().join(assets::MANIFEST_FILE);
    let text = fs::read_to_string(&manifest_path).unwrap().replace(
        &format!("\"sans\": \"{FAMILY}\""),
        "\"sans\": \"Not Inter\"",
    );
    fs::write(&manifest_path, text).unwrap();
    let set = AssetSet::current(temp.path()).unwrap().unwrap();
    assert_eq!(set.family("sans"), Some("Not Inter"));
    let mut db = Database::new();

    let report = install_in(&mut db, Some(&set));

    assert_eq!(report.mismatched, [("sans".to_owned(), "Not Inter".to_owned(), FAMILY.to_owned())]);
    // The file's real family is what the generic maps to, so text still renders.
    assert_eq!(resolve(&db, Family::SansSerif).unwrap().0, FAMILY);
}

#[test]
fn no_set_falls_back_to_the_embedded_inter() {
    let mut db = Database::new();

    let report = install_in(&mut db, None);

    assert_eq!(report.origin, Origin::Embedded);
    assert!(report.fonts.is_empty());
    let (family, path) = resolve(&db, Family::SansSerif).expect("a face");
    assert_eq!(family, FAMILY);
    assert_eq!(path, None, "the embedded copy is a binary source");
    assert!(report.to_string().starts_with(&format!("embedded {FAMILY} (")), "{report}");
}

#[test]
fn body_text_is_the_sans_default_at_light() {
    let body = ui::font::BODY;
    assert_eq!(body.weight, Weight::Light);
    assert!(matches!(body.family, IcedFamily::SansSerif));
}

// SPDX-License-Identifier: MIT OR Apache-2.0
#![cfg(feature = "verified")]

//! Immutable verified byte reads: the owned bytes, the identity pinning
//! the exact manifest bytes, and the refusals (symlink substitution and
//! escapes, replacement after the descriptor open, length/digest and
//! staging-limit violations, malformed manifests and catalogues).

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use assets::{
    AssetSet, ExplicitRequest, MANIFEST_FILE, ReadLimits, SCHEMA, SCHEMA_V2, STYLESHEET_FILE,
    VerifiedFile, VerifiedSet,
};
use sha2::Digest;

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

/// Write a JSON tree as strict data, the way an installer would.
fn write_manifest(dir: &Path, json: &serde_json::Value) {
    let text = strict::encode_pretty(&strict::from_json(json)).unwrap();
    fs::write(dir.join(MANIFEST_FILE), text).unwrap();
}

/// `sets/<id>` under `root`: a sans font, an icons font with a catalogue,
/// the manifest and the stylesheet, all from the given bytes.
fn publish(root: &Path, id: &str, sans: &[u8], icon_font: &[u8], catalogue: &[u8]) -> PathBuf {
    let dir = root.join("sets").join(id);
    let entries = vec![
        write_file(&dir, "fonts/Sans.ttf", sans),
        write_file(&dir, "icons/Symbols.ttf", icon_font),
        write_file(&dir, "icons/Symbols.codepoints", catalogue),
    ];
    write_manifest(
        &dir,
        &serde_json::json!({
            "schema": SCHEMA, "set_id": id,
            "fonts": { "sans": "fonts/Sans.ttf", "icons": "icons/Symbols.ttf" },
            "font_families": { "sans": "Fixture Sans", "icons": "Fixture Symbols" },
            "files": entries, "web_css": "/* fixture */\n"
        }),
    );
    fs::write(dir.join(STYLESHEET_FILE), "/* fixture */\n").unwrap();
    dir
}

fn fixture(root: &Path, id: &str) -> PathBuf {
    publish(
        root,
        id,
        b"font bytes",
        b"icon font",
        b"delete e872\nfolder e2c7\n",
    )
}

/// A v2 set: one sans font, two icon catalogues (rounded default, plus
/// outlined) with distinct codepoints files, and one symbolic SVG asset.
fn publish_v2(root: &Path, id: &str) -> PathBuf {
    let dir = root.join("sets").join(id);
    let entries = vec![
        write_file(&dir, "fonts/Sans.ttf", b"font bytes"),
        write_file(&dir, "icons/Rounded.ttf", b"rounded font"),
        write_file(&dir, "icons/Rounded.codepoints", b"delete e872\nfolder e2c7\n"),
        write_file(&dir, "icons/Outlined.ttf", b"outlined font"),
        write_file(&dir, "icons/Outlined.codepoints", b"delete e900\n"),
        write_file(&dir, "icons/mark.svg", b"<svg/>"),
    ];
    write_manifest(
        &dir,
        &serde_json::json!({
            "schema": SCHEMA_V2, "set_id": id,
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
    fs::write(dir.join(STYLESHEET_FILE), "/* fixture */\n").unwrap();
    dir
}

fn verified(root: &Path, id: &str) -> VerifiedSet {
    AssetSet::open(root, id)
        .unwrap()
        .read_verified(ReadLimits::default())
        .unwrap()
}

#[test]
fn read_verified_captures_owned_bytes_identity_and_icons() {
    let temp = tempfile::tempdir().unwrap();
    let dir = fixture(temp.path(), "one");
    let set = verified(temp.path(), "one");
    assert_eq!(set.set_id(), "one");
    assert_eq!(set.identity().set_id(), "one");
    // The identity's digest covers the exact manifest bytes.
    let manifest_bytes = fs::read(dir.join(MANIFEST_FILE)).unwrap();
    assert_eq!(set.manifest_bytes(), manifest_bytes);
    assert_eq!(
        set.identity().manifest_blake3(),
        *blake3::hash(&manifest_bytes).as_bytes()
    );
    assert_eq!(set.identity().manifest_blake3_hex().len(), 64);
    // The owned font bytes carry the digests the manifest locked.
    let sans = set.font("sans").unwrap();
    assert_eq!(sans.path(), "fonts/Sans.ttf");
    assert_eq!(sans.bytes(), b"font bytes");
    assert_eq!(
        sans.sha256(),
        hex::encode(sha2::Sha256::digest(b"font bytes"))
    );
    assert_eq!(
        sans.blake3(),
        blake3::hash(b"font bytes").to_hex().to_string()
    );
    assert_eq!(
        set.file("fonts/Sans.ttf").map(VerifiedFile::bytes),
        Some(b"font bytes".as_slice())
    );
    assert!(set.font("missing").is_none());
    assert!(set.file("unlocked.txt").is_none());
    assert_eq!(set.files().count(), 3);
    // The parsed icons and the captured stylesheet.
    assert_eq!(set.icon("delete"), Some('\u{e872}'));
    assert_eq!(set.icon("folder"), Some('\u{e2c7}'));
    assert_eq!(set.icon("unknown"), None);
    assert_eq!(set.icons().len(), 2);
    assert_eq!(set.stylesheet(), "/* fixture */\n");
    assert_eq!(set.manifest().files.len(), 3);
}

#[test]
fn captured_bytes_survive_path_replacement_and_removal() {
    let temp = tempfile::tempdir().unwrap();
    fixture(temp.path(), "one");
    let set = verified(temp.path(), "one");
    let identity = set.identity().manifest_blake3();
    fs::remove_dir_all(temp.path().join("sets/one")).unwrap();
    publish(
        temp.path(),
        "one",
        b"replaced font!",
        b"icon font",
        b"delete e872\nfolder e2c7\n",
    );
    // The captured set never touches the path again: same bytes, digests,
    // icons and identity after the directory was removed and replaced.
    assert_eq!(set.font("sans").unwrap().bytes(), b"font bytes");
    assert_eq!(
        set.font("sans").unwrap().sha256(),
        hex::encode(sha2::Sha256::digest(b"font bytes"))
    );
    assert_eq!(set.identity().manifest_blake3(), identity);
    assert_eq!(set.icon("folder"), Some('\u{e2c7}'));
    assert_eq!(set.stylesheet(), "/* fixture */\n");
    // The same path now answers with different bytes: same ID, different
    // manifest bytes, so a different identity.
    let again = verified(temp.path(), "one");
    assert_eq!(again.set_id(), set.set_id());
    assert_eq!(again.font("sans").unwrap().bytes(), b"replaced font!");
    assert_ne!(again.identity().manifest_blake3(), identity);
}

#[test]
fn replacement_after_descriptor_open_reads_the_pinned_inode() {
    let temp = tempfile::tempdir().unwrap();
    let dir = fixture(temp.path(), "one");
    let original_manifest = fs::read(dir.join(MANIFEST_FILE)).unwrap();
    let directory = config::atomic::open_directory(&dir).unwrap();
    // The pathname is swapped for different content after the descriptor
    // is open: the directory is renamed aside (its children stay
    // reachable through the held descriptor) and replaced at the old
    // path. The verified read is bound to the pinned inode, not the path.
    fs::rename(&dir, temp.path().join("sets/original")).unwrap();
    publish(
        temp.path(),
        "one",
        b"substituted!",
        b"new icons",
        b"new e123\n",
    );
    let set = VerifiedSet::read_in(directory, "one", ReadLimits::default()).unwrap();
    assert_eq!(set.font("sans").unwrap().bytes(), b"font bytes");
    assert_eq!(set.manifest_bytes(), original_manifest);
    assert_eq!(
        set.identity().manifest_blake3(),
        *blake3::hash(&original_manifest).as_bytes()
    );
    assert_eq!(set.icon("delete"), Some('\u{e872}'));
    // A path-based read now sees the substitution.
    assert_eq!(
        verified(temp.path(), "one").font("sans").unwrap().bytes(),
        b"substituted!"
    );
}

#[test]
fn symlinked_locked_file_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    let dir = fixture(temp.path(), "one");
    let set = AssetSet::open(temp.path(), "one").unwrap();
    let outside = temp.path().join("outside.ttf");
    fs::write(&outside, b"font bytes").unwrap();
    let font = dir.join("fonts/Sans.ttf");
    fs::remove_file(&font).unwrap();
    symlink(&outside, &font).unwrap();
    // `open` had checked the layout; the verified read re-walks with
    // descriptors and refuses the planted symlink itself.
    let error = set.read_verified(ReadLimits::default()).unwrap_err();
    assert!(matches!(error, assets::Error::Io { .. }), "{error}");
}

#[test]
fn symlinked_intermediate_directory_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    let dir = fixture(temp.path(), "one");
    let set = AssetSet::open(temp.path(), "one").unwrap();
    let fonts = dir.join("fonts");
    let renamed = dir.join("original-fonts");
    fs::rename(&fonts, &renamed).unwrap();
    symlink(&renamed, &fonts).unwrap();
    let error = set.read_verified(ReadLimits::default()).unwrap_err();
    assert!(matches!(error, assets::Error::Io { .. }), "{error}");
}

#[test]
fn symlinked_set_directory_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    fixture(temp.path(), "one");
    let set = AssetSet::open(temp.path(), "one").unwrap();
    let dir = temp.path().join("sets/one");
    fs::rename(&dir, temp.path().join("sets/original")).unwrap();
    let outside = temp.path().join("elsewhere");
    fs::create_dir(&outside).unwrap();
    symlink(&outside, &dir).unwrap();
    // The set directory itself swapped for a symlink escape.
    let error = set.read_verified(ReadLimits::default()).unwrap_err();
    assert!(matches!(error, assets::Error::Io { .. }), "{error}");
}

#[test]
fn read_limits_refuse_files_manifests_and_totals() {
    let temp = tempfile::tempdir().unwrap();
    fixture(temp.path(), "one");
    let set = AssetSet::open(temp.path(), "one").unwrap();
    let error = set
        .read_verified(ReadLimits {
            max_file_bytes: 9,
            ..ReadLimits::default()
        })
        .unwrap_err();
    assert!(matches!(error, assets::Error::Invalid(_)));
    assert!(error.to_string().contains("per-file"), "{error}");
    let error = set
        .read_verified(ReadLimits {
            max_total_bytes: 24,
            ..ReadLimits::default()
        })
        .unwrap_err();
    assert!(matches!(error, assets::Error::Invalid(_)));
    assert!(error.to_string().contains("total"), "{error}");
    let error = set
        .read_verified(ReadLimits {
            max_manifest_bytes: 64,
            ..ReadLimits::default()
        })
        .unwrap_err();
    assert!(matches!(error, assets::Error::Invalid(_)));
    assert!(error.to_string().contains("exceeds"), "{error}");
    // The hard caps refuse an oversized request outright.
    let error = set
        .read_verified(ReadLimits {
            max_file_bytes: ReadLimits::MAX_FILE_BYTES + 1,
            ..ReadLimits::default()
        })
        .unwrap_err();
    assert!(error.to_string().contains("hard cap"), "{error}");
    let error = set
        .read_verified(ReadLimits {
            max_total_bytes: ReadLimits::MAX_TOTAL_BYTES + 1,
            ..ReadLimits::default()
        })
        .unwrap_err();
    assert!(error.to_string().contains("hard cap"), "{error}");
}

#[test]
fn total_allowance_bounds_the_manifest_and_stylesheet_reads() {
    let temp = tempfile::tempdir().unwrap();
    let dir = fixture(temp.path(), "one");
    let set = AssetSet::open(temp.path(), "one").unwrap();
    let manifest = fs::read(dir.join(MANIFEST_FILE)).unwrap();
    let stylesheet = fs::read(dir.join(STYLESHEET_FILE)).unwrap();
    let both = (manifest.len() + stylesheet.len()) as u64;
    // Exactly the manifest+stylesheet budget: both are admitted against
    // the total, and the first locked payload trips the total fault.
    let error = set
        .read_verified(ReadLimits {
            max_total_bytes: both,
            ..ReadLimits::default()
        })
        .unwrap_err();
    assert!(matches!(error, assets::Error::Invalid(_)));
    assert!(error.to_string().contains("total"), "{error}");
    // One byte under: the stylesheet read itself trips the total fault,
    // before its bytes are fully allocated.
    let error = set
        .read_verified(ReadLimits {
            max_total_bytes: both - 1,
            ..ReadLimits::default()
        })
        .unwrap_err();
    assert!(matches!(error, assets::Error::Invalid(_)));
    assert!(error.to_string().contains("total"), "{error}");
    assert!(error.to_string().contains("fonts.css"), "{error}");
}

#[test]
fn total_allowance_bounds_the_complete_set() {
    let temp = tempfile::tempdir().unwrap();
    let dir = fixture(temp.path(), "one");
    let set = AssetSet::open(temp.path(), "one").unwrap();
    let captured: u64 = [
        dir.join(MANIFEST_FILE),
        dir.join(STYLESHEET_FILE),
        dir.join("fonts/Sans.ttf"),
        dir.join("icons/Symbols.ttf"),
        dir.join("icons/Symbols.codepoints"),
    ]
    .iter()
    .map(|path| fs::metadata(path).unwrap().len())
    .sum();
    // Exactly the complete captured set: the read succeeds.
    let captured_set = set
        .read_verified(ReadLimits {
            max_total_bytes: captured,
            ..ReadLimits::default()
        })
        .unwrap();
    assert_eq!(captured_set.files().count(), 3);
    assert_eq!(captured_set.icon("folder"), Some('\u{e2c7}'));
    // One byte under the complete set: the last staged file trips the
    // total fault.
    let error = set
        .read_verified(ReadLimits {
            max_total_bytes: captured - 1,
            ..ReadLimits::default()
        })
        .unwrap_err();
    assert!(matches!(error, assets::Error::Invalid(_)));
    assert!(error.to_string().contains("total"), "{error}");
}

#[test]
fn length_growth_truncation_and_digest_mismatches_are_refused() {
    let temp = tempfile::tempdir().unwrap();
    let dir = fixture(temp.path(), "one");
    let set = AssetSet::open(temp.path(), "one").unwrap();
    let font = dir.join("fonts/Sans.ttf");
    // Same length, different bytes: the SHA-256 refuses it.
    fs::write(&font, b"font byteZ").unwrap();
    let error = set.read_verified(ReadLimits::default()).unwrap_err();
    assert!(matches!(error, assets::Error::Mismatch(_)));
    assert!(error.to_string().contains("SHA-256"), "{error}");
    // Grown beyond the locked length.
    fs::write(&font, b"font bytes grown").unwrap();
    let error = set.read_verified(ReadLimits::default()).unwrap_err();
    assert!(matches!(error, assets::Error::Mismatch(_)));
    assert!(error.to_string().contains("size"), "{error}");
    // Truncated below it.
    fs::write(&font, b"short").unwrap();
    let error = set.read_verified(ReadLimits::default()).unwrap_err();
    assert!(matches!(error, assets::Error::Mismatch(_)));
    assert!(error.to_string().contains("size"), "{error}");
    // The stylesheet must still be the locked web_css.
    fs::write(&font, b"font bytes").unwrap();
    fs::write(dir.join(STYLESHEET_FILE), "/* altered */\n").unwrap();
    let error = set.read_verified(ReadLimits::default()).unwrap_err();
    assert!(matches!(error, assets::Error::Mismatch(_)));
    assert!(error.to_string().contains("fonts.css"), "{error}");
}

#[test]
fn malformed_manifests_keep_their_error_kind() {
    let temp = tempfile::tempdir().unwrap();
    let dir = fixture(temp.path(), "one");
    let set = AssetSet::open(temp.path(), "one").unwrap();
    // The verified read re-parses the manifest itself; a violation the
    // handle never saw is still a manifest error, not a fall-through.
    fs::write(dir.join(MANIFEST_FILE), "schema: $(cat /etc/passwd)\n").unwrap();
    let error = set.read_verified(ReadLimits::default()).unwrap_err();
    assert!(matches!(error, assets::Error::Manifest { .. }), "{error}");
    // Bytes that are not UTF-8 cannot be strict data.
    fs::write(dir.join(MANIFEST_FILE), b"\x80\x81 not utf-8\n").unwrap();
    let error = set.read_verified(ReadLimits::default()).unwrap_err();
    assert!(matches!(error, assets::Error::Invalid(_)));
    assert!(error.to_string().contains("UTF-8"), "{error}");
}

#[test]
fn malformed_or_unlocked_catalogues_keep_their_error_kind() {
    let temp = tempfile::tempdir().unwrap();
    // A catalogue row with a trailing column is an invalid icon error.
    let dir = publish(
        temp.path(),
        "one",
        b"font bytes",
        b"icon font",
        b"delete e872 extra\n",
    );
    let directory = config::atomic::open_directory(&dir).unwrap();
    let error = VerifiedSet::read_in(directory, "one", ReadLimits::default()).unwrap_err();
    assert!(matches!(error, assets::Error::Invalid(_)));
    assert!(error.to_string().contains("catalogue"), "{error}");
    // An icons role whose `.codepoints` sibling is not locked.
    fs::remove_dir_all(&dir).unwrap();
    let entries = vec![
        write_file(&dir, "fonts/Sans.ttf", b"font bytes"),
        write_file(&dir, "icons/Symbols.ttf", b"icon font"),
    ];
    fs::write(dir.join("icons/Symbols.codepoints"), b"delete e872\n").unwrap();
    write_manifest(
        &dir,
        &serde_json::json!({
            "schema": SCHEMA, "set_id": "one",
            "fonts": { "sans": "fonts/Sans.ttf", "icons": "icons/Symbols.ttf" },
            "files": entries, "web_css": "/* fixture */\n"
        }),
    );
    fs::write(dir.join(STYLESHEET_FILE), "/* fixture */\n").unwrap();
    let directory = config::atomic::open_directory(&dir).unwrap();
    let error = VerifiedSet::read_in(directory, "one", ReadLimits::default()).unwrap_err();
    assert!(matches!(error, assets::Error::Invalid(_)));
    assert!(error.to_string().contains("not locked"), "{error}");
    // A locked catalogue beyond the 1 MiB catalogue limit.
    fs::remove_dir_all(&dir).unwrap();
    let oversized = vec![b'a'; 1024 * 1024 + 1];
    publish(temp.path(), "one", b"font bytes", b"icon font", &oversized);
    let directory = config::atomic::open_directory(&dir).unwrap();
    let error = VerifiedSet::read_in(directory, "one", ReadLimits::default()).unwrap_err();
    assert!(matches!(error, assets::Error::Invalid(_)));
    assert!(error.to_string().contains("exceeds"), "{error}");
}

#[test]
fn shared_bytes_share_the_verified_allocation() {
    let temp = tempfile::tempdir().unwrap();
    fixture(temp.path(), "one");
    let set = verified(temp.path(), "one");
    let sans = set.font("sans").unwrap();
    let shared = sans.shared_bytes();
    assert_eq!(&*shared, b"font bytes");
    // The same allocation the digests were computed over, not a copy.
    assert_eq!(Arc::as_ptr(&shared), sans.bytes().as_ptr());
    // Cloning the Arc shares the allocation too.
    assert!(Arc::ptr_eq(&shared, &sans.shared_bytes()));
}

#[test]
fn v1_verified_sets_derive_the_default_catalogue() {
    let temp = tempfile::tempdir().unwrap();
    fixture(temp.path(), "one");
    let set = verified(temp.path(), "one");
    let default = set.icon_default().unwrap();
    assert_eq!(
        (default.family.as_str(), default.style.as_str(), default.weight),
        ("Fixture Symbols", "default", 400)
    );
    let catalogues = set.icon_catalogues();
    assert_eq!(catalogues.len(), 1);
    assert_eq!(catalogues[0].catalogue.font, "icons/Symbols.ttf");
    assert_eq!(catalogues[0].catalogue.face_index, 0);
    assert_eq!(catalogues[0].catalogue.codepoints, "icons/Symbols.codepoints");
    assert_eq!(catalogues[0].glyphs["delete"], '\u{e872}');
    assert!(set.icon_assets().is_empty());
    // The legacy table is the derived default catalogue.
    assert_eq!(set.icon("folder"), Some('\u{e2c7}'));
    assert_eq!(set.icons().len(), 2);
    // The v1 nondefault-style refusal holds on the verified side too.
    let error = set.icon_catalogue("Fixture Symbols", "rounded").unwrap_err();
    assert!(matches!(error, assets::Error::Invalid(_)));
    assert!(error.to_string().contains("rounded"), "{error}");
    assert!(set.icon_catalogue("Fixture Symbols", "default").unwrap().is_some());
}

#[test]
fn v2_verified_sets_parse_every_catalogue_and_expose_owned_bytes() {
    let temp = tempfile::tempdir().unwrap();
    publish_v2(temp.path(), "two");
    let set = verified(temp.path(), "two");
    let default = set.icon_default().unwrap();
    assert_eq!(
        (default.family.as_str(), default.style.as_str(), default.weight),
        ("Fixture Symbols", "rounded", 400)
    );
    let rounded = set
        .icon_catalogue("Fixture Symbols", "rounded")
        .unwrap()
        .unwrap();
    assert_eq!(rounded.catalogue.face_index, 0);
    assert_eq!(rounded.glyphs["delete"], '\u{e872}');
    let outlined = set
        .icon_catalogue("Fixture Symbols", "outlined")
        .unwrap()
        .unwrap();
    assert_eq!(outlined.catalogue.face_index, 1);
    assert_eq!(outlined.glyphs["delete"], '\u{e900}');
    assert!(set.icon_catalogue("Fixture Symbols", "filled").unwrap().is_none());
    // The legacy table is the declared default catalogue.
    assert_eq!(set.icon("delete"), Some('\u{e872}'));
    assert_eq!(set.icons().len(), 2);
    // Owned font bytes per catalogue, without a copy.
    let font = set.icon_catalogue_font(rounded).unwrap();
    assert_eq!(font.bytes(), b"rounded font");
    assert_eq!(font.shared_bytes().as_ref(), b"rounded font");
    assert_eq!(
        set.icon_catalogue_font(outlined).unwrap().bytes(),
        b"outlined font"
    );
    // Owned bytes for the declared non-font asset.
    let assets = set.icon_assets();
    assert_eq!(assets.len(), 1);
    let mark = set.icon_asset_file(&assets[0]).unwrap();
    assert_eq!(mark.bytes(), b"<svg/>");
    assert_eq!(mark.shared_bytes().as_ref(), b"<svg/>");
}

#[test]
fn explicit_requests_resolve_by_descriptor_id_and_digest_never_current() {
    let temp = tempfile::tempdir().unwrap();
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    let dir = fixture(&first, "one");
    let dir2 = publish(
        &second,
        "one",
        b"replaced font!",
        b"icon font",
        b"delete e872\nfolder e2c7\n",
    );
    let digest: [u8; 32] =
        blake3::hash(&fs::read(dir.join(MANIFEST_FILE)).unwrap()).into();
    let second_digest: [u8; 32] =
        blake3::hash(&fs::read(dir2.join(MANIFEST_FILE)).unwrap()).into();
    let held = [
        config::atomic::open_directory(&first).unwrap(),
        config::atomic::open_directory(&second).unwrap(),
    ];
    // The first root holds the requested identity and answers.
    let request = ExplicitRequest {
        set_id: "one",
        manifest_blake3: Some(digest),
    };
    let set = VerifiedSet::read_explicit(&held, &request, ReadLimits::default())
        .unwrap()
        .unwrap();
    assert_eq!(set.set_id(), "one");
    assert_eq!(set.identity().manifest_blake3(), digest);
    assert_eq!(set.font("sans").unwrap().bytes(), b"font bytes");
    // A wrong digest is a diagnostic, not a fall-through to the second
    // root's copy with the same ID.
    let wrong = ExplicitRequest {
        set_id: "one",
        manifest_blake3: Some([0u8; 32]),
    };
    let error =
        VerifiedSet::read_explicit(&held, &wrong, ReadLimits::default()).unwrap_err();
    assert!(matches!(error, assets::Error::Mismatch(_)), "{error}");
    assert!(error.to_string().contains("digest"), "{error}");
    // The second root's digest selects the second root's copy.
    let request = ExplicitRequest {
        set_id: "one",
        manifest_blake3: Some(second_digest),
    };
    let set = VerifiedSet::read_explicit(&held, &request, ReadLimits::default())
        .unwrap()
        .unwrap();
    assert_eq!(set.font("sans").unwrap().bytes(), b"replaced font!");
    // Without a digest the search order decides.
    let request = ExplicitRequest {
        set_id: "one",
        manifest_blake3: None,
    };
    let set = VerifiedSet::read_explicit(&held, &request, ReadLimits::default())
        .unwrap()
        .unwrap();
    assert_eq!(set.font("sans").unwrap().bytes(), b"font bytes");
    // The activation link is never consulted: a dangling `current` does
    // not affect the explicit read.
    symlink("sets/absent", first.join("current")).unwrap();
    let set = VerifiedSet::read_explicit(&held, &request, ReadLimits::default())
        .unwrap()
        .unwrap();
    assert_eq!(set.set_id(), "one");
    // An absent root or set falls through; absent everywhere is None.
    let empty = temp.path().join("empty");
    fs::create_dir(&empty).unwrap();
    let held_empty = config::atomic::open_directory(&empty).unwrap();
    assert!(
        VerifiedSet::read_at(&held_empty, &request, ReadLimits::default())
            .unwrap()
            .is_none()
    );
    assert!(
        VerifiedSet::read_explicit(&[held_empty], &request, ReadLimits::default())
            .unwrap()
            .is_none()
    );
    // An invalid set ID is refused outright, never opened.
    let bad = ExplicitRequest {
        set_id: "../evil",
        manifest_blake3: None,
    };
    assert!(VerifiedSet::read_explicit(&held, &bad, ReadLimits::default()).is_err());
}

#[test]
fn explicit_requests_are_pinned_to_the_descriptor_and_refuse_substitution() {
    let temp = tempfile::tempdir().unwrap();
    let dir = fixture(temp.path(), "one");
    let root = config::atomic::open_directory(temp.path()).unwrap();
    let digest: [u8; 32] =
        blake3::hash(&fs::read(dir.join(MANIFEST_FILE)).unwrap()).into();
    let request = ExplicitRequest {
        set_id: "one",
        manifest_blake3: Some(digest),
    };
    let set = VerifiedSet::read_at(&root, &request, ReadLimits::default())
        .unwrap()
        .unwrap();
    assert_eq!(set.font("sans").unwrap().bytes(), b"font bytes");
    // The set directory is renamed aside and replaced at the old path
    // with different bytes: the same ID now carries a different manifest.
    fs::rename(&dir, temp.path().join("sets/original")).unwrap();
    publish(
        temp.path(),
        "one",
        b"substituted!",
        b"new icons",
        b"new e123\n",
    );
    // The digest-pinned request is refused: a set with the same ID and a
    // changed manifest is not the requested set.
    let error = VerifiedSet::read_at(&root, &request, ReadLimits::default()).unwrap_err();
    assert!(matches!(error, assets::Error::Mismatch(_)), "{error}");
    assert!(error.to_string().contains("digest"), "{error}");
    // Without a digest the descriptor resolves the current directory:
    // the substitution is visible, nothing is cached.
    let request = ExplicitRequest {
        set_id: "one",
        manifest_blake3: None,
    };
    let set = VerifiedSet::read_at(&root, &request, ReadLimits::default())
        .unwrap()
        .unwrap();
    assert_eq!(set.font("sans").unwrap().bytes(), b"substituted!");
    // A symlinked `sets` directory is an error, not a fall-through.
    fs::remove_dir_all(temp.path().join("sets")).unwrap();
    let outside = temp.path().join("elsewhere");
    fs::create_dir_all(outside.join("one")).unwrap();
    symlink(&outside, temp.path().join("sets")).unwrap();
    let error = VerifiedSet::read_at(&root, &request, ReadLimits::default()).unwrap_err();
    assert!(matches!(error, assets::Error::Io { .. }), "{error}");
    fs::remove_file(temp.path().join("sets")).unwrap();
    fs::create_dir(temp.path().join("sets")).unwrap();
    // A symlinked set directory is an error too.
    symlink(&outside, temp.path().join("sets/one")).unwrap();
    let error = VerifiedSet::read_at(&root, &request, ReadLimits::default()).unwrap_err();
    assert!(matches!(error, assets::Error::Io { .. }), "{error}");
    fs::remove_file(temp.path().join("sets/one")).unwrap();
    // A regular file where the set belongs is refused.
    fs::write(temp.path().join("sets/one"), b"not a directory").unwrap();
    let error = VerifiedSet::read_at(&root, &request, ReadLimits::default()).unwrap_err();
    assert!(matches!(error, assets::Error::Io { .. }), "{error}");
}

#[test]
fn explicit_requests_keep_the_read_caps() {
    let temp = tempfile::tempdir().unwrap();
    fixture(temp.path(), "one");
    let root = config::atomic::open_directory(temp.path()).unwrap();
    let request = ExplicitRequest {
        set_id: "one",
        manifest_blake3: None,
    };
    let error = VerifiedSet::read_at(
        &root,
        &request,
        ReadLimits {
            max_file_bytes: 9,
            ..ReadLimits::default()
        },
    )
    .unwrap()
    .unwrap_err();
    assert!(matches!(error, assets::Error::Invalid(_)));
    assert!(error.to_string().contains("per-file"), "{error}");
}

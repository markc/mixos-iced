// SPDX-License-Identifier: MIT OR Apache-2.0
//! The opt-in font registration guard suite.
//!
//! This target owns the executable guards for the font registration seam the
//! toolkit font registry consumes:
//!
//! - the cosmic registration and variable-weight guard sources, compiled
//!   from their single vendored owner (see its patch notes) instead of a
//!   drifting copy; and
//! - the iced wrapper assertions, exercised through the public font system,
//!   registration, version and paragraph APIs.
//!
//! Enable with
//! `cargo test -p toolkit --features font-registration-guards,tiny-skia --test font_registration`.
//! The feature selects no production code; this target is test-only.

#[path = "../../../vendor/cosmic-text/tests/registration_seam.rs"]
mod registration_seam;

#[path = "../../../vendor/cosmic-text/tests/variable_font_weight.rs"]
mod variable_font_weight;

use std::sync::{Arc, Mutex, MutexGuard};

use cosmic_text::fontdb::{self, Database, Source};
use cosmic_text::{FontRegistration, PinnedFaceRef, PinnedFontPolicy, SwashCache};
use iced_core::alignment;
use iced_core::font::{Family, Weight};
use iced_core::text::{
    Alignment, Difference, Ellipsis, LineHeight, Paragraph as _, Shaping, Text, Wrapping,
};
use iced_core::{Font, Pixels, Size};
use iced_graphics::text::{Paragraph, font_system};

/// The packaged Noto Sans fixture: real Latin coverage for the paragraph
/// guard, unlike an icon-only face.
const NOTO_SANS: &[u8] = include_bytes!("../../../vendor/cosmic-text/fonts/NotoSans-Regular.ttf");

/// The wrapper guards mutate iced's one global font system; serialise them
/// so each scenario compares against the version and live faces it just
/// produced. The cosmic guard sources build independent systems and never
/// take this lock.
fn wrapper_lock() -> MutexGuard<'static, ()> {
    static WRAPPER_GUARDS: Mutex<()> = Mutex::new(());
    WRAPPER_GUARDS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Parse font bytes into a binary `FaceInfo`; the scratch database supplies
/// the metadata, the caller's `Arc` owns the bytes.
fn binary_face(bytes: &'static [u8]) -> fontdb::FaceInfo {
    let mut scratch = Database::new();
    let _ = scratch.load_font_source(Source::Binary(Arc::new(bytes.to_vec())));
    let mut face = scratch.faces().next().expect("font has a face").clone();
    face.source = Source::Binary(Arc::new(bytes.to_vec()));
    face
}

#[test]
fn named_and_numeric_weights_keep_their_exact_values() {
    assert_eq!(Weight::Thin.value(), 100);
    assert_eq!(Weight::ExtraLight.value(), 200);
    assert_eq!(Weight::Light.value(), 300);
    assert_eq!(Weight::Normal.value(), 400);
    assert_eq!(Weight::Medium.value(), 500);
    assert_eq!(Weight::Semibold.value(), 600);
    assert_eq!(Weight::Bold.value(), 700);
    assert_eq!(Weight::ExtraBold.value(), 800);
    assert_eq!(Weight::Black.value(), 900);

    assert_eq!(Weight::Numeric(1).value(), 1);
    assert_eq!(Weight::Numeric(350).value(), 350);
    assert_eq!(Weight::Numeric(650).value(), 650);
    assert_eq!(Weight::Numeric(1000).value(), 1000);
}

#[test]
fn registration_changes_version_and_noops_stay_stable() {
    let _serial = wrapper_lock();
    let alias: &'static str = Box::leak(
        "toolkit-font-registration-guards-version-alias"
            .to_owned()
            .into_boxed_str(),
    );

    let mut system = font_system().write().expect("font system");
    let unchanged = system.version();
    let face_count = system.raw().db().len();
    system.load_font(std::borrow::Cow::Owned(b"malformed font".to_vec()));
    assert_eq!(system.version(), unchanged);
    assert_eq!(system.raw().db().len(), face_count);
    system.load_font(std::borrow::Cow::Owned(NOTO_SANS.to_vec()));
    assert_eq!(system.version().value(), unchanged.value() + 1);
    assert_eq!(system.raw().db().len(), face_count + 1);
    let before = system.version();
    let face = binary_face(NOTO_SANS);
    let weight = face.weight;
    let first = system
        .register_fonts(FontRegistration {
            faces: vec![face],
            policies: vec![PinnedFontPolicy {
                alias: alias.to_owned(),
                groups: vec![vec![PinnedFaceRef::Added(0)]],
                weight,
            }],
        })
        .expect("first registration succeeds");
    assert_eq!(first.added_faces.len(), 1);
    assert_eq!(first.policies_added, 1);
    assert_eq!(
        system.version().value(),
        before.value() + 1,
        "a real registration bumps the version by exactly one"
    );

    // Re-registering the identical resolved policy with no new faces is a
    // no-op: no retained growth, no version change.
    let once = system.version();
    let added_id = first.added_faces[0];
    let noop = system
        .register_fonts(FontRegistration {
            faces: Vec::new(),
            policies: vec![PinnedFontPolicy {
                alias: alias.to_owned(),
                groups: vec![vec![PinnedFaceRef::Existing(added_id)]],
                weight,
            }],
        })
        .expect("duplicate policy is a no-op");
    assert!(noop.added_faces.is_empty());
    assert_eq!(noop.policies_added, 0);
    assert_eq!(system.version(), once, "identical policy must not bump");

    // An empty transaction changes nothing either.
    let empty = system
        .register_fonts(FontRegistration {
            faces: Vec::new(),
            policies: Vec::new(),
        })
        .expect("empty transaction");
    assert!(empty.added_faces.is_empty());
    assert_eq!(empty.policies_added, 0);
    assert_eq!(system.version(), once, "empty transaction must not bump");

    // A conflicting rebind fails and leaves the version and the live font
    // facts untouched. Noto Sans is static at its parsed weight, so a
    // different sealed weight cannot be provided.
    let faces_before = system.raw().db().len();
    let aliases_before: Vec<String> = system.raw().pinned_aliases().map(str::to_owned).collect();
    let rebound = if weight.0 == 900 {
        fontdb::Weight(100)
    } else {
        fontdb::Weight::BLACK
    };
    let conflict = system.register_fonts(FontRegistration {
        faces: Vec::new(),
        policies: vec![PinnedFontPolicy {
            alias: alias.to_owned(),
            groups: vec![vec![PinnedFaceRef::Existing(added_id)]],
            weight: rebound,
        }],
    });
    assert!(conflict.is_err(), "rebinding an alias must fail");
    assert_eq!(system.version(), once, "failed registration must not bump");
    assert_eq!(
        system.raw().db().len(),
        faces_before,
        "failed registration must not add faces"
    );
    assert_eq!(
        system
            .raw()
            .pinned_aliases()
            .map(str::to_owned)
            .collect::<Vec<_>>(),
        aliases_before,
        "failed registration must not change the policy set"
    );
}

#[test]
fn one_transaction_with_many_faces_activates_once() {
    let _serial = wrapper_lock();

    let face_a = binary_face(NOTO_SANS);
    let face_b = binary_face(NOTO_SANS);
    let face_c = binary_face(NOTO_SANS);
    let weight = face_a.weight;

    let mut system = font_system().write().expect("font system");
    let before = system.version();
    let result = system
        .register_fonts(FontRegistration {
            faces: vec![face_a, face_b, face_c],
            policies: vec![
                PinnedFontPolicy {
                    alias: "toolkit-font-registration-guards-multi-a".to_owned(),
                    groups: vec![vec![PinnedFaceRef::Added(0)]],
                    weight,
                },
                PinnedFontPolicy {
                    alias: "toolkit-font-registration-guards-multi-b".to_owned(),
                    groups: vec![vec![PinnedFaceRef::Added(1)]],
                    weight,
                },
            ],
        })
        .expect("multi-face transaction succeeds");
    assert_eq!(result.added_faces.len(), 3);
    assert_eq!(result.policies_added, 2);
    assert_eq!(
        system.version().value(),
        before.value() + 1,
        "exactly one bump for the whole transaction"
    );

    // The single activation is observable: an identical no-op transaction
    // leaves the version where the transaction left it.
    let after = system.version();
    let noop = system
        .register_fonts(FontRegistration {
            faces: Vec::new(),
            policies: vec![PinnedFontPolicy {
                alias: "toolkit-font-registration-guards-multi-a".to_owned(),
                groups: vec![vec![PinnedFaceRef::Existing(result.added_faces[0])]],
                weight,
            }],
        })
        .expect("no-op succeeds");
    assert!(noop.added_faces.is_empty());
    assert_eq!(noop.policies_added, 0);
    assert_eq!(system.version(), after);
}

#[test]
fn retained_paragraph_stays_pinned_across_registration_version() {
    let _serial = wrapper_lock();
    let alias: &'static str = Box::leak(
        "toolkit-font-registration-guards-paragraph-alias"
            .to_owned()
            .into_boxed_str(),
    );

    // Register the pinned face in the shared font system and remember its
    // ID; the registration bumps the global version.
    let (face_id, weight) = {
        let mut system = font_system().write().expect("font system");
        let face = binary_face(NOTO_SANS);
        let weight = face.weight;
        let result = system
            .register_fonts(FontRegistration {
                faces: vec![face],
                policies: vec![PinnedFontPolicy {
                    alias: alias.to_owned(),
                    groups: vec![vec![PinnedFaceRef::Added(0)]],
                    weight,
                }],
            })
            .expect("registration");
        (result.added_faces[0], weight)
    };

    let font = Font {
        family: Family::Name(alias),
        ..Font::DEFAULT
    };
    let text = Text {
        content: "Hello world",
        bounds: Size::new(300.0, 100.0),
        size: Pixels(16.0),
        line_height: LineHeight::Absolute(Pixels(20.0)),
        font,
        align_x: Alignment::Left,
        align_y: alignment::Vertical::Top,
        shaping: Shaping::Advanced,
        wrapping: Wrapping::None,
        ellipsis: Ellipsis::None,
        hint_factor: None,
    };

    // The paragraph is retained across the second registration below.
    let paragraph = Paragraph::with_text(text);

    let glyphs = |paragraph: &Paragraph| {
        paragraph
            .buffer()
            .layout_runs()
            .flat_map(|run| run.glyphs.iter())
            .map(|g| (g.font_id, g.font_weight, g.glyph_id, g.w))
            .collect::<Vec<_>>()
    };
    let before = glyphs(&paragraph);
    assert!(!before.is_empty());
    assert!(
        before
            .iter()
            .all(|(id, _, glyph_id, _)| *id == face_id && *glyph_id != 0)
    );

    // A second registration bumps the version again; the retained paragraph
    // must report a Shape difference through iced's paragraph-comparison
    // path, which is what triggers a re-shape.
    {
        let mut system = font_system().write().expect("font system");
        system
            .register_fonts(FontRegistration {
                faces: vec![binary_face(NOTO_SANS)],
                policies: vec![PinnedFontPolicy {
                    alias: "toolkit-font-registration-guards-paragraph-alias-2".to_owned(),
                    groups: vec![vec![PinnedFaceRef::Added(0)]],
                    weight,
                }],
            })
            .expect("registration");
    }
    assert_eq!(
        paragraph.compare(text.with_content(())),
        Difference::Shape,
        "a version bump must mark the retained paragraph for re-shaping"
    );

    // Re-shaping the same text keeps the original face and metrics.
    let reshaped = Paragraph::with_text(text);
    let after = glyphs(&reshaped);
    assert_eq!(before, after, "reshaped paragraph must stay pinned");

    // Raster evidence: the first glyph is an 'H' that Noto Sans covers, and
    // the reshaped paragraph must still rasterise non-empty ink from the
    // pinned face.
    let cache_key = reshaped
        .buffer()
        .layout_runs()
        .flat_map(|run| run.glyphs.iter())
        .next()
        .expect("laid out glyph")
        .physical((0.0, 0.0), 1.0)
        .cache_key;
    let image = {
        let mut system = font_system().write().expect("font system");
        let mut cache = SwashCache::new();
        cache
            .get_image(system.raw(), cache_key)
            .as_ref()
            .map(|image| image.data.clone())
    };
    let image = image.expect("retained paragraph must still rasterise from its pinned face");
    assert!(!image.is_empty(), "raster data must be non-empty");
    assert!(
        image.iter().any(|&byte| byte != 0),
        "raster data must contain ink for a covered glyph"
    );
}

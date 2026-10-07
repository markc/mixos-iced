// SPDX-License-Identifier: MIT OR Apache-2.0
//! Guard tests for the pinned-shaping and atomic-registration seam.
//!
//! These use only the fonts packaged under `fonts/`:
//! - `NotoSans-Regular.ttf` covers Latin but neither Arabic nor Hebrew;
//! - `NotoSansArabic.ttf` and `NotoSansHebrew.ttf` cover those scripts;
//! - `InterVariable-Italic.ttf` has a variable `wght` axis. The upstream
//!   archive's `InterVariable.ttf` fixture is deliberately not carried in
//!   this tree (see `PATCHES.md`); both face family and style are discovered
//!   from the parsed bytes, never assumed from a hard-coded string;
//! - `FiraMono-Medium.ttf` is monospaced.
//!
//! Every registration failure below asserts that the database, the installed
//! policy set and the derived indexes are left untouched.

use std::{path::PathBuf, sync::Arc};

use cosmic_text::fontdb::{self, Database, Source};
use cosmic_text::{
    Attrs, Buffer, Family, FontRegistration, FontRegistrationError, FontSystem, Metrics,
    PinnedFaceRef, PinnedFontPolicy, Shaping, SwashCache, Weight,
};

/// The packaged font bytes, compiled in from the archived `fonts/` fixture
/// files beside this test. The paths are source-relative, so the file builds
/// unchanged from its own test target or when the root-owned guard target
/// path-includes it; there is no manifest-directory lookup, no host-font
/// path and no environment override. Unknown names fail loudly.
fn repo_font(name: &str) -> Vec<u8> {
    match name {
        "NotoSans-Regular.ttf" => include_bytes!("../fonts/NotoSans-Regular.ttf").to_vec(),
        "NotoSansArabic.ttf" => include_bytes!("../fonts/NotoSansArabic.ttf").to_vec(),
        "NotoSansHebrew.ttf" => include_bytes!("../fonts/NotoSansHebrew.ttf").to_vec(),
        "InterVariable-Italic.ttf" => include_bytes!("../fonts/InterVariable-Italic.ttf").to_vec(),
        "FiraMono-Medium.ttf" => include_bytes!("../fonts/FiraMono-Medium.ttf").to_vec(),
        "Inter-Regular.ttf" => include_bytes!("../fonts/Inter-Regular.ttf").to_vec(),
        unknown => panic!("unknown packaged font: {unknown}"),
    }
}

/// Parse the packaged font bytes into a binary `FaceInfo`: a scratch database
/// supplies the metadata, the caller's `Arc` owns the bytes. The scratch ID
/// is replaced by `register_fonts` when the face is committed.
fn binary_face(bytes: &[u8]) -> fontdb::FaceInfo {
    let mut scratch = Database::new();
    let _ = scratch.load_font_source(Source::Binary(Arc::new(bytes.to_vec())));
    let mut face = scratch.faces().next().expect("font has a face").clone();
    face.source = Source::Binary(Arc::new(bytes.to_vec()));
    face
}

/// A font system holding only the packaged fonts, with the generic families
/// bound to families that exist in it.
fn fonts() -> FontSystem {
    let mut db = Database::new();
    db.set_monospace_family("Fira Mono");
    db.set_sans_serif_family("Noto Sans");
    db.set_serif_family("Noto Sans");
    for name in [
        "NotoSans-Regular.ttf",
        "NotoSansArabic.ttf",
        "NotoSansHebrew.ttf",
        "InterVariable-Italic.ttf",
        "FiraMono-Medium.ttf",
        "Inter-Regular.ttf",
    ] {
        let _ = db.load_font_source(Source::Binary(Arc::new(repo_font(name))));
    }
    FontSystem::new_with_locale_and_db("en-US".into(), db)
}

/// Shape `text` with the given attrs and return `(font_id, font_weight,
/// glyph_id, width)` for every laid out glyph.
fn shape(
    font_system: &mut FontSystem,
    text: &str,
    attrs: &Attrs<'_>,
    shaping: Shaping,
) -> Vec<(fontdb::ID, Weight, u16, f32)> {
    let metrics = Metrics::new(16.0, 20.0);
    let mut buffer = Buffer::new(font_system, metrics);
    {
        let mut buffer = buffer.borrow_with(font_system);
        buffer.set_size(Some(300.0), Some(100.0));
        buffer.set_text(text, attrs, shaping, None);
        buffer.shape_until_scroll(true);
        buffer
            .layout_runs()
            .flat_map(|run| run.glyphs.iter())
            .map(|glyph| (glyph.font_id, glyph.font_weight, glyph.glyph_id, glyph.w))
            .collect::<Vec<_>>()
    }
}

fn face_id(font_system: &FontSystem, family: &str) -> fontdb::ID {
    font_system
        .db()
        .faces()
        .find(|face| face.families.iter().any(|(name, _)| name == family))
        .expect("family in database")
        .id
}

/// The ID of a face whose family name — as discovered from the parsed bytes
/// by the database, not assumed — contains `fragment`.
fn face_with_family_containing(font_system: &FontSystem, fragment: &str) -> fontdb::ID {
    font_system
        .db()
        .faces()
        .find(|face| {
            face.families
                .iter()
                .any(|(name, _)| name.contains(fragment))
        })
        .expect("face family in database")
        .id
}

/// Every laid out glyph of an already shaped buffer.
fn laid_out(buffer: &Buffer) -> Vec<cosmic_text::LayoutGlyph> {
    buffer
        .layout_runs()
        .flat_map(|run| run.glyphs.iter().cloned())
        .collect()
}

/// The raw source bytes of a binary face.
fn source_bytes(font_system: &FontSystem, id: fontdb::ID) -> Vec<u8> {
    font_system
        .db()
        .with_face_data(id, |data, _| Some(data.to_vec()))
        .expect("face in database")
        .expect("binary source")
}

/// Copy font bytes with one table-directory tag renamed, making that table
/// unreadable to the parser while every other table stays valid. This is a
/// deterministic mutation of font table metadata: no new fixture bytes.
fn with_renamed_table(bytes: &[u8], old_tag: &[u8; 4], new_tag: &[u8; 4]) -> Vec<u8> {
    let mut out = bytes.to_vec();
    let num_tables = u16::from_be_bytes([out[4], out[5]]) as usize;
    for record in 0..num_tables {
        let offset = 12 + record * 16;
        if &out[offset..offset + 4] == old_tag {
            out[offset..offset + 4].copy_from_slice(new_tag);
            break;
        }
    }
    out
}

fn snapshot(font_system: &FontSystem) -> (usize, Vec<String>, bool) {
    (
        font_system.db().len(),
        font_system.pinned_aliases().map(str::to_owned).collect(),
        font_system.is_monospace(face_id(font_system, "Fira Mono")),
    )
}

#[test]
fn declared_fallback_renders_and_global_cover_does_not() {
    let mut font_system = fonts();
    let noto = face_id(&font_system, "Noto Sans");
    let global_arabic = face_id(&font_system, "Noto Sans Arabic");

    // A lacks Arabic, declared B covers it, and the global database also has
    // C (another NotoSansArabic copy) covering it.
    let b_face = binary_face(&repo_font("NotoSansArabic.ttf"));
    let b_id = font_system
        .register_fonts(FontRegistration {
            faces: vec![b_face],
            policies: vec![PinnedFontPolicy {
                alias: "pinned-arabic".into(),
                groups: vec![
                    vec![PinnedFaceRef::Existing(noto)],
                    vec![PinnedFaceRef::Added(0)],
                ],
                weight: fontdb::Weight::NORMAL,
            }],
        })
        .expect("registration")
        .added_faces[0];

    let attrs = Attrs::new().family(Family::Name("pinned-arabic"));
    for shaping in [Shaping::Basic, Shaping::Advanced] {
        let glyphs = shape(&mut font_system, "مرحبا", &attrs, shaping);
        assert!(!glyphs.is_empty(), "{shaping:?}: no glyphs");
        for (id, _, glyph_id, _) in &glyphs {
            assert_eq!(*id, b_id, "{shaping:?}: only declared B may render");
            assert_ne!(*id, global_arabic, "{shaping:?}: global C leaked in");
            assert_ne!(*glyph_id, 0, "{shaping:?}: B covers the codepoint");
        }
    }

    // Without B, the exhausted pinned coverage must leave zero glyphs even
    // though global C covers the script.
    font_system
        .register_fonts(FontRegistration {
            faces: Vec::new(),
            policies: vec![PinnedFontPolicy {
                alias: "pinned-arabic-nofallback".into(),
                groups: vec![vec![PinnedFaceRef::Existing(noto)]],
                weight: fontdb::Weight::NORMAL,
            }],
        })
        .expect("registration");
    let attrs = Attrs::new().family(Family::Name("pinned-arabic-nofallback"));
    for shaping in [Shaping::Basic, Shaping::Advanced] {
        let glyphs = shape(&mut font_system, "مرحبا", &attrs, shaping);
        assert!(!glyphs.is_empty(), "{shaping:?}: no glyphs");
        for (_, _, glyph_id, _) in &glyphs {
            assert_eq!(*glyph_id, 0, "{shaping:?}: zero glyph without B");
        }
    }
}

#[test]
fn declared_group_order_beats_global_tables() {
    let mut font_system = fonts();
    let noto = face_id(&font_system, "Noto Sans");

    // Two copies of the Arabic font: b_first has the lower ID, so the global
    // fallback tables would prefer it. The policy declares b_later first in
    // the group, and declared order must win.
    let b_first = binary_face(&repo_font("NotoSansArabic.ttf"));
    let b_later = binary_face(&repo_font("NotoSansArabic.ttf"));
    let weight = b_first.weight;

    let added = font_system
        .register_fonts(FontRegistration {
            faces: vec![b_first, b_later],
            policies: vec![PinnedFontPolicy {
                alias: "pinned-arabic-order".into(),
                groups: vec![
                    vec![PinnedFaceRef::Existing(noto)],
                    vec![PinnedFaceRef::Added(1), PinnedFaceRef::Added(0)],
                ],
                weight,
            }],
        })
        .expect("registration")
        .added_faces;
    let b_first_id = added[0];
    let b_later_id = added[1];
    assert!(b_first_id < b_later_id, "IDs increase in insertion order");

    let attrs = Attrs::new().family(Family::Name("pinned-arabic-order"));
    for shaping in [Shaping::Basic, Shaping::Advanced] {
        let glyphs = shape(&mut font_system, "مرحبا", &attrs, shaping);
        assert!(!glyphs.is_empty(), "{shaping:?}: no glyphs");
        for (id, _, glyph_id, _) in &glyphs {
            assert_eq!(*id, b_later_id, "{shaping:?}: declared order must win");
            assert_ne!(*glyph_id, 0, "{shaping:?}: covered codepoint");
        }
    }
}

#[test]
fn duplicate_policy_and_empty_transaction_are_noops() {
    let mut font_system = fonts();
    let noto = face_id(&font_system, "Noto Sans");
    let before = snapshot(&font_system);

    let added_id = font_system
        .register_fonts(FontRegistration {
            faces: Vec::new(),
            policies: vec![PinnedFontPolicy {
                alias: "pinned-noop".into(),
                groups: vec![vec![PinnedFaceRef::Existing(noto)]],
                weight: fontdb::Weight::NORMAL,
            }],
        })
        .expect("registration")
        .added_faces;
    assert!(added_id.is_empty());

    let after_first = snapshot(&font_system);
    assert_eq!(after_first.0, before.0, "no face growth");
    assert_ne!(after_first.1, before.1, "the policy was installed");

    // Identical resolved policy: no new faces, no new policies, no mutation.
    let result = font_system
        .register_fonts(FontRegistration {
            faces: Vec::new(),
            policies: vec![PinnedFontPolicy {
                alias: "pinned-noop".into(),
                groups: vec![vec![PinnedFaceRef::Existing(noto)]],
                weight: fontdb::Weight::NORMAL,
            }],
        })
        .expect("identical policy is a no-op");
    assert!(result.added_faces.is_empty());
    assert_eq!(result.policies_added, 0);
    assert_eq!(snapshot(&font_system), after_first);

    // Empty transaction: no mutation at all.
    let result = font_system
        .register_fonts(FontRegistration {
            faces: Vec::new(),
            policies: Vec::new(),
        })
        .expect("empty transaction");
    assert!(result.added_faces.is_empty());
    assert_eq!(result.policies_added, 0);
    assert_eq!(snapshot(&font_system), after_first);
}

#[test]
fn failed_registrations_leave_the_system_unchanged() {
    let mut font_system = fonts();
    let noto = face_id(&font_system, "Noto Sans");
    let inter = face_with_family_containing(&font_system, "Inter");

    // Baseline: one valid policy already installed, to prove the policy set
    // survives failed attempts.
    font_system
        .register_fonts(FontRegistration {
            faces: Vec::new(),
            policies: vec![PinnedFontPolicy {
                alias: "pinned-baseline".into(),
                groups: vec![vec![PinnedFaceRef::Existing(noto)]],
                weight: fontdb::Weight::NORMAL,
            }],
        })
        .expect("baseline registration");
    let before = snapshot(&font_system);

    let attempts: Vec<FontRegistration> = vec![
        // Rebinding a pinned alias with a different resolved policy.
        FontRegistration {
            faces: Vec::new(),
            policies: vec![PinnedFontPolicy {
                alias: "pinned-baseline".into(),
                groups: vec![vec![PinnedFaceRef::Existing(inter)]],
                weight: fontdb::Weight::NORMAL,
            }],
        },
        // Unsupported static weight (Noto Sans has no 800 face or wght axis).
        FontRegistration {
            faces: Vec::new(),
            policies: vec![PinnedFontPolicy {
                alias: "pinned-bad-weight".into(),
                groups: vec![vec![PinnedFaceRef::Existing(noto)]],
                weight: fontdb::Weight(800),
            }],
        },
        // Invalid weights.
        FontRegistration {
            faces: Vec::new(),
            policies: vec![PinnedFontPolicy {
                alias: "pinned-weight-zero".into(),
                groups: vec![vec![PinnedFaceRef::Existing(noto)]],
                weight: fontdb::Weight(0),
            }],
        },
        FontRegistration {
            faces: Vec::new(),
            policies: vec![PinnedFontPolicy {
                alias: "pinned-weight-over".into(),
                groups: vec![vec![PinnedFaceRef::Existing(noto)]],
                weight: fontdb::Weight(1001),
            }],
        },
        // Out-of-range added reference.
        FontRegistration {
            faces: vec![binary_face(&repo_font("NotoSans-Regular.ttf"))],
            policies: vec![PinnedFontPolicy {
                alias: "pinned-bad-ref".into(),
                groups: vec![vec![PinnedFaceRef::Added(5)]],
                weight: fontdb::Weight::NORMAL,
            }],
        },
        // Non-binary source.
        {
            let mut face = binary_face(&repo_font("NotoSans-Regular.ttf"));
            face.source = Source::File(PathBuf::from("not-a-real-path.ttf"));
            FontRegistration {
                faces: vec![face],
                policies: vec![PinnedFontPolicy {
                    alias: "pinned-bad-source".into(),
                    groups: vec![vec![PinnedFaceRef::Added(0)]],
                    weight: fontdb::Weight::NORMAL,
                }],
            }
        },
        // Unconstructible face: parseable metadata but garbage bytes.
        {
            let mut face = binary_face(&repo_font("NotoSans-Regular.ttf"));
            face.source = Source::Binary(Arc::new(vec![0u8; 16]));
            FontRegistration {
                faces: vec![face],
                policies: vec![PinnedFontPolicy {
                    alias: "pinned-bad-bytes".into(),
                    groups: vec![vec![PinnedFaceRef::Added(0)]],
                    weight: fontdb::Weight::NORMAL,
                }],
            }
        },
        // Empty policy, empty group, duplicate alias, alias vs family name.
        FontRegistration {
            faces: Vec::new(),
            policies: vec![PinnedFontPolicy {
                alias: "pinned-empty".into(),
                groups: Vec::new(),
                weight: fontdb::Weight::NORMAL,
            }],
        },
        FontRegistration {
            faces: Vec::new(),
            policies: vec![PinnedFontPolicy {
                alias: "pinned-empty-group".into(),
                groups: vec![Vec::new()],
                weight: fontdb::Weight::NORMAL,
            }],
        },
        FontRegistration {
            faces: Vec::new(),
            policies: vec![
                PinnedFontPolicy {
                    alias: "pinned-dup".into(),
                    groups: vec![vec![PinnedFaceRef::Existing(noto)]],
                    weight: fontdb::Weight::NORMAL,
                },
                PinnedFontPolicy {
                    alias: "pinned-dup".into(),
                    groups: vec![vec![PinnedFaceRef::Existing(noto)]],
                    weight: fontdb::Weight::NORMAL,
                },
            ],
        },
        FontRegistration {
            faces: Vec::new(),
            policies: vec![PinnedFontPolicy {
                alias: "noto sans".into(),
                groups: vec![vec![PinnedFaceRef::Existing(noto)]],
                weight: fontdb::Weight::NORMAL,
            }],
        },
        // Too many face references for one policy.
        FontRegistration {
            faces: Vec::new(),
            policies: vec![PinnedFontPolicy {
                alias: "pinned-too-many-refs".into(),
                groups: vec![vec![PinnedFaceRef::Existing(noto); 65]],
                weight: fontdb::Weight::NORMAL,
            }],
        },
        // Too long an alias.
        FontRegistration {
            faces: Vec::new(),
            policies: vec![PinnedFontPolicy {
                alias: "x".repeat(300),
                groups: vec![vec![PinnedFaceRef::Existing(noto)]],
                weight: fontdb::Weight::NORMAL,
            }],
        },
    ];

    for registration in attempts {
        let error = font_system
            .register_fonts(registration)
            .expect_err("attempt must fail");
        assert!(
            matches!(
                error,
                FontRegistrationError::ConflictingAlias(_)
                    | FontRegistrationError::UnsupportedWeight { .. }
                    | FontRegistrationError::InvalidWeight { .. }
                    | FontRegistrationError::OutOfRangeAddedFace { .. }
                    | FontRegistrationError::NonBinarySource { .. }
                    | FontRegistrationError::UnconstructibleFace { .. }
                    | FontRegistrationError::EmptyPolicy { .. }
                    | FontRegistrationError::EmptyGroup { .. }
                    | FontRegistrationError::DuplicateAlias(_)
                    | FontRegistrationError::TooManyFaceRefs { .. }
                    | FontRegistrationError::InvalidAlias { .. }
            ),
            "unexpected error variant: {error:?}"
        );
        assert_eq!(
            snapshot(&font_system),
            before,
            "failed registration mutated the system: {error:?}"
        );
    }
}

#[test]
fn missing_existing_face_is_rejected() {
    let mut font_system = fonts();

    // Remove a face through the legacy escape hatch, then reference it.
    let removed = face_id(&font_system, "Noto Sans Hebrew");
    font_system.db_mut().remove_face(removed);
    font_system.refresh_database();
    let before = snapshot(&font_system);

    let error = font_system
        .register_fonts(FontRegistration {
            faces: Vec::new(),
            policies: vec![PinnedFontPolicy {
                alias: "pinned-missing".into(),
                groups: vec![vec![PinnedFaceRef::Existing(removed)]],
                weight: fontdb::Weight::NORMAL,
            }],
        })
        .expect_err("missing reference must fail");
    assert!(matches!(
        error,
        FontRegistrationError::MissingExistingFace { id, .. } if id == removed
    ));
    assert_eq!(snapshot(&font_system), before);
}

#[test]
fn old_paragraph_survives_same_named_registration() {
    let mut font_system = fonts();

    let added_id = font_system
        .register_fonts(FontRegistration {
            faces: vec![binary_face(&repo_font("NotoSans-Regular.ttf"))],
            policies: vec![PinnedFontPolicy {
                alias: "pinned-old".into(),
                groups: vec![vec![PinnedFaceRef::Added(0)]],
                weight: fontdb::Weight::NORMAL,
            }],
        })
        .expect("registration")
        .added_faces[0];

    // One live paragraph buffer, shaped before the second registration and
    // kept alive across it.
    let attrs = Attrs::new().family(Family::Name("pinned-old"));
    let metrics = Metrics::new(16.0, 20.0);
    let mut buffer = Buffer::new(&mut font_system, metrics);
    {
        let mut buffer = buffer.borrow_with(&mut font_system);
        buffer.set_size(Some(300.0), Some(100.0));
        buffer.set_text("Hello world", &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(true);
    }
    let before = laid_out(&buffer);
    assert!(!before.is_empty());
    assert!(
        before
            .iter()
            .all(|glyph| glyph.font_id == added_id && glyph.glyph_id != 0)
    );

    // A second collection with the SAME public family name but truly
    // different bytes: a controlled metadata clone of the live "Noto Sans"
    // face whose source is the Hebrew font data. Test-only construction of
    // caller-supplied metadata; production callers own their FaceInfo.
    let mut impostor = font_system
        .db()
        .faces()
        .find(|face| face.families.iter().any(|(name, _)| name == "Noto Sans"))
        .expect("Noto Sans face")
        .clone();
    impostor.source = Source::Binary(Arc::new(repo_font("NotoSansHebrew.ttf")));
    let impostor_id = font_system
        .register_fonts(FontRegistration {
            faces: vec![impostor],
            policies: vec![PinnedFontPolicy {
                alias: "pinned-new".into(),
                groups: vec![vec![PinnedFaceRef::Added(0)]],
                weight: fontdb::Weight::NORMAL,
            }],
        })
        .expect("registration")
        .added_faces[0];

    // The two faces share the public family name but hold distinct bytes.
    for id in [added_id, impostor_id] {
        assert!(
            font_system
                .db()
                .face(id)
                .expect("face")
                .families
                .iter()
                .any(|(name, _)| name == "Noto Sans")
        );
    }
    assert_eq!(
        source_bytes(&font_system, added_id),
        repo_font("NotoSans-Regular.ttf")
    );
    assert_eq!(
        source_bytes(&font_system, impostor_id),
        repo_font("NotoSansHebrew.ttf")
    );
    assert_ne!(impostor_id, added_id);

    // The same retained buffer, re-shaped after the registration, keeps its
    // original IDs and metrics.
    {
        let mut buffer = buffer.borrow_with(&mut font_system);
        buffer.set_text("Hello world", &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(true);
    }
    let after = laid_out(&buffer);
    assert_eq!(
        before
            .iter()
            .map(|g| (g.font_id, g.font_weight, g.glyph_id, g.w))
            .collect::<Vec<_>>(),
        after
            .iter()
            .map(|g| (g.font_id, g.font_weight, g.glyph_id, g.w))
            .collect::<Vec<_>>(),
        "old paragraph must stay pinned"
    );

    // The retained paragraph still rasterises from the original face.
    let cache_key = after
        .first()
        .expect("glyph")
        .physical((0.0, 0.0), 1.0)
        .cache_key;
    let mut cache = SwashCache::new();
    assert!(
        cache.get_image(&mut font_system, cache_key).is_some(),
        "retained paragraph must still rasterise"
    );

    // The impostor's same-named family resolves its own alias to its own ID,
    // and its actual bytes render Hebrew.
    let new_attrs = Attrs::new().family(Family::Name("pinned-new"));
    let new_glyphs = shape(&mut font_system, "שלום", &new_attrs, Shaping::Advanced);
    assert!(!new_glyphs.is_empty());
    assert!(
        new_glyphs
            .iter()
            .all(|(id, _, glyph_id, _)| *id == impostor_id && *glyph_id != 0)
    );
}

#[test]
fn sealed_weight_ignores_later_attribute_edits() {
    let mut font_system = fonts();

    let inter_id = font_system
        .register_fonts(FontRegistration {
            faces: vec![binary_face(&repo_font("InterVariable-Italic.ttf"))],
            policies: vec![PinnedFontPolicy {
                alias: "pinned-inter".into(),
                groups: vec![vec![PinnedFaceRef::Added(0)]],
                weight: fontdb::Weight::NORMAL,
            }],
        })
        .expect("registration")
        .added_faces[0];

    // Requesting 900 through attributes must not escape the sealed 400: no
    // new (id, weight) instance, and every glyph keeps the policy weight.
    let attrs = Attrs::new()
        .family(Family::Name("pinned-inter"))
        .weight(Weight(900));
    for shaping in [Shaping::Basic, Shaping::Advanced] {
        let glyphs = shape(&mut font_system, "Hello", &attrs, shaping);
        assert!(!glyphs.is_empty());
        for (id, weight, glyph_id, _) in &glyphs {
            assert_eq!(*id, inter_id);
            assert_eq!(*weight, fontdb::Weight::NORMAL, "sealed weight wins");
            assert_ne!(*glyph_id, 0);
        }
    }
}

#[test]
fn variable_policy_weights_change_basic_advanced_and_raster() {
    let mut font_system = fonts();

    let inter_id = font_system
        .register_fonts(FontRegistration {
            faces: vec![binary_face(&repo_font("InterVariable-Italic.ttf"))],
            policies: vec![PinnedFontPolicy {
                alias: "pinned-inter-350".into(),
                groups: vec![vec![PinnedFaceRef::Added(0)]],
                weight: fontdb::Weight(350),
            }],
        })
        .expect("registration")
        .added_faces[0];

    // Second policy, same face, different sealed weight.
    font_system
        .register_fonts(FontRegistration {
            faces: Vec::new(),
            policies: vec![PinnedFontPolicy {
                alias: "pinned-inter-650".into(),
                groups: vec![vec![PinnedFaceRef::Existing(inter_id)]],
                weight: fontdb::Weight(650),
            }],
        })
        .expect("registration");

    for shaping in [Shaping::Basic, Shaping::Advanced] {
        let attrs_350 = Attrs::new().family(Family::Name("pinned-inter-350"));
        let attrs_650 = Attrs::new().family(Family::Name("pinned-inter-650"));
        let glyphs_350 = shape(&mut font_system, "Hello", &attrs_350, shaping);
        let glyphs_650 = shape(&mut font_system, "Hello", &attrs_650, shaping);

        assert_eq!(glyphs_350.len(), glyphs_650.len());
        assert!(
            glyphs_350
                .iter()
                .all(|(_, weight, _, _)| *weight == fontdb::Weight(350))
        );
        assert!(
            glyphs_650
                .iter()
                .all(|(_, weight, _, _)| *weight == fontdb::Weight(650))
        );

        let width_350: f32 = glyphs_350.iter().map(|(_, _, _, w)| w).sum();
        let width_650: f32 = glyphs_650.iter().map(|(_, _, _, w)| w).sum();
        assert!(
            width_350 < width_650,
            "{shaping:?}: heavier weight must advance wider ({width_350} >= {width_650})"
        );
    }

    // Raster output differs between the two sealed weights of the same face.
    let raster = |font_system: &mut FontSystem, alias: &str| -> Option<Vec<u8>> {
        let attrs = Attrs::new().family(Family::Name(alias));
        let metrics = Metrics::new(16.0, 20.0);
        let mut buffer = Buffer::new(font_system, metrics);
        let cache_key = {
            let mut buffer = buffer.borrow_with(font_system);
            buffer.set_size(Some(300.0), Some(100.0));
            buffer.set_text("H", &attrs, Shaping::Advanced, None);
            buffer.shape_until_scroll(true);
            buffer
                .layout_runs()
                .flat_map(|run| run.glyphs.iter())
                .next()
                .expect("laid out glyph")
                .physical((0.0, 0.0), 1.0)
                .cache_key
        };
        let mut cache = SwashCache::new();
        cache
            .get_image(font_system, cache_key)
            .as_ref()
            .map(|image| image.data.clone())
    };

    let image_350 = raster(&mut font_system, "pinned-inter-350").expect("raster");
    let image_650 = raster(&mut font_system, "pinned-inter-650").expect("raster");
    assert_ne!(image_350, image_650, "raster must follow the sealed weight");
}

#[test]
fn registered_monospace_faces_reach_derived_indexes() {
    let mut font_system = fonts();
    let before = font_system.db().len();

    let added_id = font_system
        .register_fonts(FontRegistration {
            faces: vec![binary_face(&repo_font("FiraMono-Medium.ttf"))],
            policies: Vec::new(),
        })
        .expect("registration")
        .added_faces[0];

    assert_eq!(font_system.db().len(), before + 1);
    assert!(font_system.is_monospace(added_id));

    if cfg!(feature = "monospace_fallback") {
        // Derive the scripts from the face's own GPOS and GSUB tables,
        // independently of the implementation, and require the index to hold
        // the new face for every script the face declares.
        use cosmic_text::skrifa;
        use skrifa::raw::TableProvider as _;
        let bytes = repo_font("FiraMono-Medium.ttf");
        let font_ref = skrifa::FontRef::from_index(&bytes, 0).expect("parse");
        let mut expected = Vec::new();
        if let Some(gpos) = font_ref
            .gpos()
            .ok()
            .and_then(|table| table.script_list().ok())
        {
            expected.extend(
                gpos.script_records()
                    .iter()
                    .map(|script| script.script_tag().into_bytes()),
            );
        }
        if let Some(gsub) = font_ref
            .gsub()
            .ok()
            .and_then(|table| table.script_list().ok())
        {
            expected.extend(
                gsub.script_records()
                    .iter()
                    .map(|script| script.script_tag().into_bytes()),
            );
        }
        if !expected.is_empty() {
            let indexed = font_system.get_monospace_ids_for_scripts(expected.iter().copied());
            assert!(
                indexed.contains(&added_id),
                "registered mono face must appear in per-script indexes"
            );
        }
    }
}

#[test]
fn registered_bytes_survive_source_drop() {
    let mut font_system = fonts();
    let bytes = repo_font("Inter-Regular.ttf");

    let added_id = font_system
        .register_fonts(FontRegistration {
            faces: vec![binary_face(&bytes)],
            policies: vec![PinnedFontPolicy {
                alias: "pinned-bytes".into(),
                groups: vec![vec![PinnedFaceRef::Added(0)]],
                weight: fontdb::Weight::NORMAL,
            }],
        })
        .expect("registration")
        .added_faces[0];

    drop(bytes);

    let attrs = Attrs::new().family(Family::Name("pinned-bytes"));
    let glyphs = shape(&mut font_system, "Hello", &attrs, Shaping::Advanced);
    assert!(
        glyphs
            .iter()
            .all(|(id, _, glyph_id, _)| { *id == added_id && *glyph_id != 0 })
    );
}

#[test]
fn unpinned_families_keep_legacy_behaviour() {
    let mut font_system = fonts();
    let noto = face_id(&font_system, "Noto Sans");

    // Install a pinned alias on the side.
    font_system
        .register_fonts(FontRegistration {
            faces: Vec::new(),
            policies: vec![PinnedFontPolicy {
                alias: "pinned-side".into(),
                groups: vec![vec![PinnedFaceRef::Existing(noto)]],
                weight: fontdb::Weight::NORMAL,
            }],
        })
        .expect("registration");

    for family in [
        Family::Name("Noto Sans"),
        Family::SansSerif,
        Family::Monospace,
    ] {
        let attrs = Attrs::new().family(family);
        for shaping in [Shaping::Basic, Shaping::Advanced] {
            let glyphs = shape(&mut font_system, "Hello", &attrs, shaping);
            assert!(!glyphs.is_empty(), "{family:?}/{shaping:?}: no glyphs");
            assert!(
                glyphs.iter().all(|(_, _, glyph_id, _)| *glyph_id != 0),
                "{family:?}/{shaping:?}: legacy fallback broke"
            );
        }
    }
}

#[test]
fn policy_total_reaches_boundary_then_rejects() {
    // MAX_PINNED_POLICIES in src/font/system.rs: a total held limit, not a
    // per-batch limit.
    const LIMIT: usize = 1024;

    let mut font_system = fonts();
    let noto = face_id(&font_system, "Noto Sans");

    // One batch fills the policy map to its limit; every policy shares the
    // same existing face, so the staged instance is validated once.
    let policies = (0..LIMIT)
        .map(|i| PinnedFontPolicy {
            alias: format!("pinned-boundary-{i}"),
            groups: vec![vec![PinnedFaceRef::Existing(noto)]],
            weight: fontdb::Weight::NORMAL,
        })
        .collect();
    let result = font_system
        .register_fonts(FontRegistration {
            faces: Vec::new(),
            policies,
        })
        .expect("boundary batch");
    assert_eq!(result.policies_added, LIMIT);
    assert_eq!(font_system.pinned_aliases().count(), LIMIT);

    let before = snapshot(&font_system);

    // One more genuinely new policy crosses the total limit and is rejected
    // atomically.
    let error = font_system
        .register_fonts(FontRegistration {
            faces: Vec::new(),
            policies: vec![PinnedFontPolicy {
                alias: "pinned-over-boundary".into(),
                groups: vec![vec![PinnedFaceRef::Existing(noto)]],
                weight: fontdb::Weight::NORMAL,
            }],
        })
        .expect_err("crossing the total limit must fail");
    assert!(matches!(
        error,
        FontRegistrationError::TooManyPolicies { limit } if limit == LIMIT
    ));
    assert_eq!(snapshot(&font_system), before);
    assert_eq!(font_system.pinned_aliases().count(), LIMIT);

    // Re-submitting an installed alias at the boundary stays a no-op.
    let noop = font_system
        .register_fonts(FontRegistration {
            faces: Vec::new(),
            policies: vec![PinnedFontPolicy {
                alias: "pinned-boundary-0".into(),
                groups: vec![vec![PinnedFaceRef::Existing(noto)]],
                weight: fontdb::Weight::NORMAL,
            }],
        })
        .expect("no-op at the boundary");
    assert_eq!(noop.policies_added, 0);
    assert_eq!(font_system.pinned_aliases().count(), LIMIT);
}

#[test]
fn staged_family_capture_is_rejected() {
    let mut font_system = fonts();
    let before = snapshot(&font_system);

    // A face added by this very transaction declares a family that equals
    // the new alias, case-insensitively; the pinned policy would capture
    // public selections of that family. The family name is test-only
    // caller-supplied metadata, distinct from every family already in the
    // database, so only the staged check can reject it.
    let mut face = binary_face(&repo_font("NotoSans-Regular.ttf"));
    face.families[0].0 = "staged-capture-family".to_owned();
    let error = font_system
        .register_fonts(FontRegistration {
            faces: vec![face],
            policies: vec![PinnedFontPolicy {
                alias: "STAGED-CAPTURE-FAMILY".to_owned(),
                groups: vec![vec![PinnedFaceRef::Added(0)]],
                weight: fontdb::Weight::NORMAL,
            }],
        })
        .expect_err("staged family capture must fail");
    assert!(matches!(error, FontRegistrationError::ConflictingAlias(_)));
    assert_eq!(snapshot(&font_system), before);
}

#[test]
fn single_layout_table_still_reaches_per_script_indexes() {
    if !cfg!(feature = "monospace_fallback") {
        return;
    }
    use cosmic_text::skrifa;
    use skrifa::raw::TableProvider as _;

    let mut font_system = fonts();

    // Rename the GSUB directory tag so only GPOS stays readable: the old
    // chained `gpos()?`/`gsub()?` extraction drops every script when one
    // table is unreadable, while the independent merge must keep the GPOS
    // scripts. Derived deterministically from the packaged Fira Mono bytes.
    let mutated = with_renamed_table(&repo_font("FiraMono-Medium.ttf"), b"GSUB", b"xxxx");

    // Expected scripts come from the mutated bytes' own GPOS table, read
    // independently of the implementation under test.
    let font_ref = skrifa::FontRef::from_index(&mutated, 0).expect("parse");
    assert!(
        font_ref.gsub().is_err(),
        "GSUB must be unreadable after the rename"
    );
    let mut expected = Vec::new();
    if let Some(gpos) = font_ref
        .gpos()
        .ok()
        .and_then(|table| table.script_list().ok())
    {
        expected.extend(
            gpos.script_records()
                .iter()
                .map(|script| script.script_tag().into_bytes()),
        );
    }
    assert!(
        !expected.is_empty(),
        "the Fira Mono fixture must declare GPOS scripts for this guard to be meaningful"
    );

    let added_id = font_system
        .register_fonts(FontRegistration {
            faces: vec![binary_face(&mutated)],
            policies: Vec::new(),
        })
        .expect("registration")
        .added_faces[0];

    assert!(font_system.is_monospace(added_id));
    let indexed = font_system.get_monospace_ids_for_scripts(expected.iter().copied());
    assert!(
        indexed.contains(&added_id),
        "GPOS scripts must reach the per-script index while GSUB is unreadable"
    );
}

#[test]
fn ttc_collection_faces_register_and_malformed_ttc_is_rejected() {
    let mut font_system = fonts();
    let before = snapshot(&font_system);

    // TTC table directory offsets are absolute from the collection start,
    // unlike the standalone fonts. Relocate every table and align the second
    // face; concatenating unmodified TTF bytes produces an invalid collection.
    let relocate = |mut font: Vec<u8>, base: u32| {
        let tables = usize::from(u16::from_be_bytes([font[4], font[5]]));
        for table in 0..tables {
            let start = 12 + table * 16 + 8;
            let offset = u32::from_be_bytes(font[start..start + 4].try_into().unwrap());
            font[start..start + 4]
                .copy_from_slice(&offset.checked_add(base).unwrap().to_be_bytes());
        }
        font
    };
    let header_len = 12u32 + 2 * 4;
    let arabic = relocate(repo_font("NotoSansArabic.ttf"), header_len);
    let hebrew_offset = header_len + (arabic.len() as u32).div_ceil(4) * 4;
    let hebrew = relocate(repo_font("NotoSansHebrew.ttf"), hebrew_offset);
    let mut ttc = Vec::new();
    ttc.extend_from_slice(b"ttcf");
    ttc.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    ttc.extend_from_slice(&2u32.to_be_bytes());
    ttc.extend_from_slice(&header_len.to_be_bytes());
    ttc.extend_from_slice(&hebrew_offset.to_be_bytes());
    ttc.extend_from_slice(&arabic);
    ttc.resize(hebrew_offset as usize, 0);
    ttc.extend_from_slice(&hebrew);

    // The declared collection parses into two faces; registering the second
    // (Hebrew) face by index works and shapes Hebrew from its actual bytes.
    let mut scratch = Database::new();
    let _ = scratch.load_font_source(Source::Binary(Arc::new(ttc.clone())));
    let faces: Vec<_> = scratch.faces().cloned().collect();
    assert_eq!(
        faces.len(),
        2,
        "the declared TTC count must parse into two faces"
    );
    let mut hebrew_face = faces[1].clone();
    hebrew_face.source = Source::Binary(Arc::new(ttc.clone()));
    let added_id = font_system
        .register_fonts(FontRegistration {
            faces: vec![hebrew_face],
            policies: vec![PinnedFontPolicy {
                alias: "pinned-ttc".into(),
                groups: vec![vec![PinnedFaceRef::Added(0)]],
                weight: fontdb::Weight::NORMAL,
            }],
        })
        .expect("TTC face registers")
        .added_faces[0];

    let attrs = Attrs::new().family(Family::Name("pinned-ttc"));
    let glyphs = shape(&mut font_system, "שלום", &attrs, Shaping::Advanced);
    assert!(
        glyphs
            .iter()
            .all(|(id, _, glyph_id, _)| *id == added_id && *glyph_id != 0)
    );

    let state = snapshot(&font_system);

    // A face index beyond the declared count is rejected without mutation.
    let mut out_of_range = faces[0].clone();
    out_of_range.source = Source::Binary(Arc::new(ttc.clone()));
    out_of_range.index = 2;
    let error = font_system
        .register_fonts(FontRegistration {
            faces: vec![out_of_range],
            policies: vec![PinnedFontPolicy {
                alias: "pinned-ttc-bad-index".into(),
                groups: vec![vec![PinnedFaceRef::Added(0)]],
                weight: fontdb::Weight::NORMAL,
            }],
        })
        .expect_err("a face index past the declared TTC count must fail");
    assert!(matches!(
        error,
        FontRegistrationError::UnconstructibleFace { .. }
    ));
    assert_eq!(snapshot(&font_system), state);

    // A truncated TTC whose header declares faces it does not hold is
    // rejected without mutation.
    let mut truncated = Vec::new();
    truncated.extend_from_slice(b"ttcf");
    truncated.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    truncated.extend_from_slice(&2u32.to_be_bytes());
    let mut truncated_face = faces[0].clone();
    truncated_face.source = Source::Binary(Arc::new(truncated));
    let error = font_system
        .register_fonts(FontRegistration {
            faces: vec![truncated_face],
            policies: vec![PinnedFontPolicy {
                alias: "pinned-ttc-truncated".into(),
                groups: vec![vec![PinnedFaceRef::Added(0)]],
                weight: fontdb::Weight::NORMAL,
            }],
        })
        .expect_err("a truncated declared TTC must fail");
    assert!(matches!(
        error,
        FontRegistrationError::UnconstructibleFace { .. }
    ));
    assert_eq!(snapshot(&font_system), state);
    assert_eq!(
        snapshot(&font_system).0,
        before.0 + 1,
        "only the valid TTC face was added"
    );
}

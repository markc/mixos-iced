// SPDX-License-Identifier: MIT OR Apache-2.0
//! Production-path scenarios for `fonts::registry`: the process singleton,
//! the real iced font-system wrapper and the shared global database.
//!
//! One test function runs every scenario serially so that version and usage
//! deltas are attributable. Renderer-version assertions read the evidence
//! captured inside `register_batch` (under the renderer write lock), which
//! is exact; registry usage is exact because this binary is the only
//! registry user in its process.
//!
//! Fonts are upstream licensed test files already in the tree: iced's
//! embedded Fira Sans, `vendor/font`'s Inter variable font and the
//! cosmic-text vendor's Noto/Inter files.

use std::{collections::BTreeMap, sync::Arc};

use toolkit::core::font::{Family, Stretch, Style, Weight};
use toolkit::fonts::registry::{
    self, FamilyGroup, FontBlob, FontCollection, IconCatalogue, IconSelectionRequest,
    RegistrationBatch, RegistrationError, Resource, Selection, SelectionRequest, SourceFace,
    WeightPolicy,
};
use toolkit::graphics::text::{self, cosmic_text};

const INTER_VARIABLE: &[u8] =
    include_bytes!("../../../vendor/font/Inter-VariableFont_opsz,wght.ttf").as_slice();
const FIRA: &[u8] = toolkit::graphics::text::FIRA_SANS_REGULAR;
const NOTO_SANS: &[u8] =
    include_bytes!("../../../vendor/cosmic-text/fonts/NotoSans-Regular.ttf").as_slice();
const NOTO_ARABIC: &[u8] =
    include_bytes!("../../../vendor/cosmic-text/fonts/NotoSansArabic.ttf").as_slice();
const INTER_REGULAR: &[u8] =
    include_bytes!("../../../vendor/cosmic-text/fonts/Inter-Regular.ttf").as_slice();

fn blob(bytes: &'static [u8]) -> FontBlob {
    FontBlob {
        bytes: Arc::from(bytes),
    }
}

fn group(name: &str) -> FamilyGroup {
    FamilyGroup {
        name: name.into(),
        faces: vec![SourceFace {
            source: 0,
            index: 0,
        }],
    }
}

fn collection(bytes: &'static [u8], family: &str, role: &str) -> FontCollection {
    FontCollection {
        sources: vec![blob(bytes)],
        families: vec![group(family)],
        roles: BTreeMap::from([(role.to_owned(), vec![family.to_owned()])]),
        icons: Vec::new(),
    }
}

fn selection_request(key: &str, families: &[&str], weight: u16) -> SelectionRequest {
    SelectionRequest {
        key: key.into(),
        families: families.iter().map(|name| name.to_string()).collect(),
        requested_weight: weight,
        weight_policy: WeightPolicy::Exact,
        style: Style::Normal,
        stretch: Stretch::Normal,
    }
}

fn batch(bytes: &'static [u8], family: &str, role: &str, weight: u16) -> RegistrationBatch {
    RegistrationBatch {
        collection: collection(bytes, family, role),
        selections: vec![selection_request(role, &[family], weight)],
        icons: Vec::new(),
    }
}

fn alias_of(selection: &Selection) -> &'static str {
    match selection.font().family {
        Family::Name(name) => name,
        _ => panic!("registry selections must name an alias"),
    }
}

/// Shape text through the real global font system and return
/// `(font_id, glyph_id, advance)` for every laid out glyph.
fn shape(
    alias: &str,
    text: &str,
    shaping: cosmic_text::Shaping,
) -> Vec<(cosmic_text::fontdb::ID, u16, f32)> {
    let mut system = text::font_system().write().expect("font system");
    let raw = system.raw();
    let metrics = cosmic_text::Metrics::new(16.0, 20.0);
    let mut buffer = cosmic_text::Buffer::new(raw, metrics);
    let attrs = cosmic_text::Attrs::new().family(cosmic_text::Family::Name(alias));
    let glyphs = {
        let mut buffer = buffer.borrow_with(raw);
        buffer.set_size(Some(300.0), Some(100.0));
        buffer.set_text(text, &attrs, shaping, None);
        buffer.shape_until_scroll(true);
        buffer
            .layout_runs()
            .flat_map(|run| run.glyphs.iter())
            .map(|glyph| (glyph.font_id, glyph.glyph_id, glyph.w))
            .collect::<Vec<_>>()
    };
    glyphs
}

fn face_digest(id: cosmic_text::fontdb::ID) -> blake3::Hash {
    let mut system = text::font_system().write().expect("font system");
    let raw = system.raw();
    raw.db()
        .with_face_data(id, |data, _| blake3::hash(data))
        .expect("face data")
}

fn version() -> u32 {
    text::font_system()
        .read()
        .expect("font system")
        .version()
        .value()
}

#[test]
fn registry_end_to_end_scenarios() {
    let registry = registry::registry();

    // --- Registration, receipt and shaping provenance ----------------------
    let registration = registry
        .register_batch(batch(INTER_VARIABLE, "Inter", "ui", 400))
        .expect("first registration");
    let selection = registration.font("ui").expect("role receipt");
    assert_eq!(selection.font().weight, Weight::Numeric(400));
    let alias = alias_of(selection);
    assert!(alias.starts_with("mixos-pinned-"));
    let evidence = selection.evidence();
    assert_eq!(evidence.declared, vec!["Inter"]);
    assert_eq!(evidence.chosen_group, 0);
    assert_eq!(evidence.family, "Inter");
    assert_eq!(evidence.requested_weight, 400);
    assert_eq!(evidence.effective_weight, 400);
    assert!(evidence.substitution.is_none());
    let owned = selection.owned();
    assert_eq!(owned.effective_weight(), 400);
    assert_eq!(
        blake3::hash(&owned.groups()[0][0].bytes()),
        blake3::hash(INTER_VARIABLE),
        "owned bytes must be the registered source bytes"
    );
    let batch_evidence = registration.evidence();
    assert_eq!(batch_evidence.added_sources, 1);
    assert_eq!(batch_evidence.added_faces, 1);
    assert_eq!(batch_evidence.policies_added, 1);
    assert_ne!(
        batch_evidence.renderer_version_after,
        batch_evidence.renderer_version_before
    );
    assert_eq!(registration.collection_id().as_str().len(), 64);
    for shaping in [cosmic_text::Shaping::Basic, cosmic_text::Shaping::Advanced] {
        let glyphs = shape(alias, "Hello MixOS", shaping);
        assert!(!glyphs.is_empty());
        for (id, glyph_id, _) in &glyphs {
            assert_ne!(*glyph_id, 0);
            assert_eq!(
                face_digest(*id),
                blake3::hash(INTER_VARIABLE),
                "{shaping:?}: rendered bytes must be the registered bytes"
            );
        }
    }

    // --- Identical re-batch: zero growth, no version churn ----------------
    let before = registry.usage();
    let again = registry
        .register_batch(batch(INTER_VARIABLE, "Inter", "ui", 400))
        .expect("identical re-batch");
    let evidence = again.evidence();
    assert_eq!(evidence.added_sources, 0);
    assert_eq!(evidence.reused_sources, 1);
    assert_eq!(evidence.added_faces, 0);
    assert_eq!(evidence.policies_added, 0);
    assert_eq!(evidence.policies_reused, 1);
    assert_eq!(evidence.usage_before, before);
    assert_eq!(evidence.usage_after, before);
    assert_eq!(
        evidence.renderer_version_after,
        evidence.renderer_version_before
    );
    assert_eq!(again.font("ui").unwrap().font(), selection.font());

    // --- Two roles, one selection, one alias -------------------------------
    let shared = registry
        .register_batch(RegistrationBatch {
            collection: collection(FIRA, "Fira Sans", "body"),
            selections: vec![
                selection_request("body", &["Fira Sans"], 400),
                selection_request("copy", &["Fira Sans"], 400),
            ],
            icons: Vec::new(),
        })
        .expect("shared selection");
    assert_eq!(
        shared.font("body").unwrap().font(),
        shared.font("copy").unwrap().font()
    );
    assert_eq!(shared.evidence().policies_added, 1);

    // --- Nonbucket variable weights shape differently ----------------------
    let light = registry
        .register_batch(batch(INTER_VARIABLE, "Inter", "light", 350))
        .expect("light");
    let heavy = registry
        .register_batch(batch(INTER_VARIABLE, "Inter", "heavy", 650))
        .expect("heavy");
    let light_alias = alias_of(light.font("light").unwrap());
    let heavy_alias = alias_of(heavy.font("heavy").unwrap());
    assert_ne!(light_alias, heavy_alias);
    assert_eq!(
        light.font("light").unwrap().font().weight,
        Weight::Numeric(350)
    );
    let width = |alias: &str| {
        shape(alias, "Hello", cosmic_text::Shaping::Advanced)
            .iter()
            .map(|(_, _, w)| w)
            .sum::<f32>()
    };
    assert!(width(light_alias) < width(heavy_alias));

    // --- Exact static weights fail; explicit substitution succeeds ---------
    let before = registry.usage();
    let err = registry
        .register_batch(batch(FIRA, "Fira Sans", "bold", 700))
        .expect_err("static 400 face cannot provide 700 exactly");
    assert!(matches!(
        err,
        RegistrationError::UnsupportedWeight { key, family, weight: 700 }
            if key == "bold" && family == "Fira Sans"
    ));
    assert_eq!(registry.usage(), before);
    let mut substitute = batch(FIRA, "Fira Sans", "bold", 700);
    substitute.selections[0].weight_policy = WeightPolicy::Substitute {
        effective: 400,
        reason: "no bold face in packaged set".into(),
    };
    let substituted = registry.register_batch(substitute).expect("substitution");
    let evidence = substituted.font("bold").unwrap().evidence();
    assert_eq!(evidence.requested_weight, 700);
    assert_eq!(evidence.effective_weight, 400);
    assert_eq!(
        evidence.substitution.as_ref().unwrap().reason,
        "no bold face in packaged set"
    );

    // --- Declared fallback advances and reports the chosen group -----------
    let fallback = registry
        .register_batch(RegistrationBatch {
            collection: collection(INTER_VARIABLE, "Inter", "ui"),
            selections: vec![selection_request("ui", &["Missing", "Inter"], 400)],
            icons: Vec::new(),
        })
        .expect("declared fallback");
    let evidence = fallback.font("ui").unwrap().evidence();
    assert_eq!(evidence.chosen_group, 1);
    assert_eq!(evidence.family, "Inter");
    assert_eq!(
        evidence.groups.len(),
        1,
        "absent families leave no empty group"
    );

    // --- Same public family, different bytes: old paragraph stays bound ----
    let old_alias = alias.to_owned();
    let old_shape = shape(&old_alias, "Hello world", cosmic_text::Shaping::Advanced);
    let fresh = registry
        .register_batch(batch(INTER_REGULAR, "Inter", "body", 400))
        .expect("same family, different bytes");
    let fresh_alias = alias_of(fresh.font("body").unwrap()).to_owned();
    assert_ne!(old_alias, fresh_alias);
    assert_eq!(
        shape(&old_alias, "Hello world", cosmic_text::Shaping::Advanced),
        old_shape,
        "the old paragraph must stay bound to its original bytes"
    );
    for (id, glyph_id, _) in shape(&fresh_alias, "Hello world", cosmic_text::Shaping::Advanced) {
        assert_ne!(glyph_id, 0);
        assert_eq!(face_digest(id), blake3::hash(INTER_REGULAR));
    }

    // --- Declared-only fallback: B covers, nothing else may ----------------
    let arabic_collection = FontCollection {
        sources: vec![blob(NOTO_SANS), blob(NOTO_ARABIC)],
        families: vec![
            FamilyGroup {
                name: "Noto Sans".into(),
                faces: vec![SourceFace {
                    source: 0,
                    index: 0,
                }],
            },
            FamilyGroup {
                name: "Noto Sans Arabic".into(),
                faces: vec![SourceFace {
                    source: 1,
                    index: 0,
                }],
            },
        ],
        roles: BTreeMap::new(),
        icons: Vec::new(),
    };
    let with_b = registry
        .register_batch(RegistrationBatch {
            collection: arabic_collection.clone(),
            selections: vec![selection_request(
                "ar",
                &["Noto Sans", "Noto Sans Arabic"],
                400,
            )],
            icons: Vec::new(),
        })
        .expect("arabic fallback");
    let arabic_alias = alias_of(with_b.font("ar").unwrap());
    for shaping in [cosmic_text::Shaping::Basic, cosmic_text::Shaping::Advanced] {
        let glyphs = shape(arabic_alias, "مرحبا", shaping);
        assert!(!glyphs.is_empty());
        for (id, glyph_id, _) in &glyphs {
            assert_eq!(
                face_digest(*id),
                blake3::hash(NOTO_ARABIC),
                "{shaping:?}: only the declared fallback may cover"
            );
            assert_ne!(*glyph_id, 0);
        }
    }
    let owned = with_b.font("ar").unwrap().owned();
    assert_eq!(owned.effective_weight(), 400);
    assert_eq!(owned.groups().len(), 2, "declared order preserved");
    assert_eq!(
        blake3::hash(&owned.groups()[0][0].bytes()),
        blake3::hash(NOTO_SANS)
    );
    assert_eq!(
        blake3::hash(&owned.groups()[1][0].bytes()),
        blake3::hash(NOTO_ARABIC)
    );
    let latin_only = registry
        .register_batch(RegistrationBatch {
            collection: arabic_collection,
            selections: vec![selection_request("la", &["Noto Sans"], 400)],
            icons: Vec::new(),
        })
        .expect("latin only");
    let latin_alias = alias_of(latin_only.font("la").unwrap());
    for shaping in [cosmic_text::Shaping::Basic, cosmic_text::Shaping::Advanced] {
        let glyphs = shape(latin_alias, "مرحبا", shaping);
        assert!(!glyphs.is_empty());
        for (_, glyph_id, _) in &glyphs {
            assert_eq!(
                *glyph_id, 0,
                "{shaping:?}: exhausted pinned coverage must stay missing"
            );
        }
    }

    // --- Icons: named glyphs through the aliased font ----------------------
    let icon_collection = FontCollection {
        sources: vec![blob(FIRA)],
        families: vec![group("Fira Sans")],
        roles: BTreeMap::new(),
        icons: vec![IconCatalogue {
            family: "Fira Sans".into(),
            style: "default".into(),
            face: SourceFace {
                source: 0,
                index: 0,
            },
            glyphs: BTreeMap::from([("home".into(), 'a')]),
        }],
    };
    let icons = registry
        .register_batch(RegistrationBatch {
            collection: icon_collection.clone(),
            selections: Vec::new(),
            icons: vec![IconSelectionRequest {
                key: "icons".into(),
                family: "Fira Sans".into(),
                style: "default".into(),
                weight: 400,
                required_names: vec!["home".into()],
            }],
        })
        .expect("icons");
    let (glyph, font) = icons.icon("icons", "home").expect("named icon");
    assert_eq!(glyph, 'a');
    assert_eq!(font.weight, Weight::Numeric(400));
    assert_eq!(icons.icon("icons", "unknown"), None);
    assert_eq!(icons.icon("other", "home"), None);
    let icon_alias = match font.family {
        Family::Name(name) => name,
        _ => panic!("icon selections must name an alias"),
    };
    let glyphs = shape(icon_alias, "a", cosmic_text::Shaping::Basic);
    assert_eq!(glyphs.len(), 1);
    assert_ne!(glyphs[0].1, 0);
    assert_eq!(face_digest(glyphs[0].0), blake3::hash(FIRA));

    // --- Failures are atomic ------------------------------------------------
    let before = registry.usage();
    // Fast shape-validation failure: version reads bracket a microsecond
    // window; the registry usage assertion is exact either way.
    let version_before = version();
    let mut invalid = batch(INTER_VARIABLE, "Inter", "bad", 400);
    invalid.selections[0].requested_weight = 0;
    assert!(matches!(
        registry.register_batch(invalid),
        Err(RegistrationError::WeightOutOfRange { weight: 0 })
    ));
    assert_eq!(
        version(),
        version_before,
        "a failed preflight never touches the renderer"
    );
    // Late failure: valid role, then the final icon fails on its last name.
    let err = registry
        .register_batch(RegistrationBatch {
            collection: icon_collection.clone(),
            selections: vec![selection_request("ui", &["Fira Sans"], 400)],
            icons: vec![IconSelectionRequest {
                key: "icons".into(),
                family: "Fira Sans".into(),
                style: "default".into(),
                weight: 400,
                required_names: vec!["home".into(), "missing".into()],
            }],
        })
        .expect_err("last icon must fail the batch");
    assert!(matches!(
        err,
        RegistrationError::IconNameMissing { name, .. } if name == "missing"
    ));
    assert_eq!(registry.usage(), before);

    // --- Collection bookkeeping without version churn, then the cap --------
    // Earlier scenarios already retained some collections; the loop fills the
    // ledger up to the process cap of 64 and the next attempt must fail with
    // an honest Capacity error, leaving the usage unchanged.
    let mut markers = 0;
    loop {
        markers += 1;
        let marker = RegistrationBatch {
            collection: FontCollection {
                sources: vec![blob(INTER_VARIABLE)],
                families: vec![group("Inter")],
                roles: BTreeMap::from([(format!("marker-{markers}"), vec!["Inter".into()])]),
                icons: Vec::new(),
            },
            selections: Vec::new(),
            icons: Vec::new(),
        };
        match registry.register_batch(marker) {
            Ok(registration) => {
                let evidence = registration.evidence();
                assert_eq!(evidence.added_faces, 0);
                assert_eq!(evidence.policies_added, 0);
                assert_eq!(
                    evidence.renderer_version_after, evidence.renderer_version_before,
                    "collection bookkeeping alone must not bump the renderer version"
                );
                assert_eq!(
                    evidence.usage_after.collections,
                    evidence.usage_before.collections + 1
                );
            }
            Err(RegistrationError::Capacity {
                resource: Resource::Collections,
                ..
            }) => break,
            Err(other) => panic!("unexpected marker error: {other}"),
        }
        assert!(markers < 200, "collections cap never reached");
    }
    assert_eq!(registry.usage().collections, 64);
}

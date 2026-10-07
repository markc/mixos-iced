// SPDX-License-Identifier: MIT OR Apache-2.0
//! Guards for the owned font-policy route. All fixtures are licensed fonts
//! already vendored in this tree (cosmic-text's OFL set, `vendor/font`'s
//! Inter variable font) — no new binary blobs, no system discovery.
use super::unicode::Face;
use super::*;
use crate::config::Cursor;
use std::{path::Path, sync::Arc};
use swash::{FontRef, StringId};

fn fixture(relative: &str) -> Arc<[u8]> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../vendor")
        .join(relative);
    std::fs::read(&path)
        .unwrap_or_else(|error| panic!("term-core owned-font fixture {}: {error}", path.display()))
        .into()
}

fn fira_mono() -> OwnedFace {
    OwnedFace {
        bytes: fixture("cosmic-text/fonts/FiraMono-Medium.ttf"),
        index: 0,
    }
}

fn inter_variable() -> OwnedFace {
    OwnedFace {
        bytes: fixture("font/Inter-VariableFont_opsz,wght.ttf"),
        index: 0,
    }
}

fn inter_variable_italic() -> OwnedFace {
    OwnedFace {
        bytes: fixture("cosmic-text/fonts/InterVariable-Italic.ttf"),
        index: 0,
    }
}

fn inter_regular() -> OwnedFace {
    OwnedFace {
        bytes: fixture("cosmic-text/fonts/Inter-Regular.ttf"),
        index: 0,
    }
}

fn noto_sans_hebrew() -> OwnedFace {
    OwnedFace {
        bytes: fixture("cosmic-text/fonts/NotoSansHebrew.ttf"),
        index: 0,
    }
}

fn owned_raster(groups: Vec<Vec<OwnedFace>>, weight: u16) -> Raster {
    Raster::from_owned(OwnedFontPolicy { groups, weight }, 1.0, 13.0, Cursor::Block).unwrap()
}

fn render_char(raster: &mut Raster, c: char) -> Vec<u8> {
    let mut grid = super::tests::screen(3, 1, ' ');
    grid.cells[1].c = c;
    raster.render(&grid)
}

fn family(font: FontRef<'_>) -> String {
    let strings = font.localized_strings();
    strings
        .find_by_id(StringId::TypographicFamily, None)
        .or_else(|| strings.find_by_id(StringId::Family, None))
        .expect("fixture family name")
        .chars()
        .collect()
}

/// A codepoint the Inter fixture covers but the Fira Mono fixture does not,
/// found by scanning the actual cmaps — so a guard cannot silently shape the
/// wrong face. Fails loudly if the fixtures' coverage ever converges.
fn inter_only_codepoint() -> char {
    let inter_bytes = inter_variable().bytes;
    let fira_bytes = fira_mono().bytes;
    let inter = FontRef::from_index(&inter_bytes, 0).expect("Inter variable fixture");
    let fira = FontRef::from_index(&fira_bytes, 0).expect("Fira Mono fixture");
    let inter_charmap = inter.charmap();
    let fira_charmap = fira.charmap();
    let ranges = [
        0x0370..=0x03ff, // Greek and Coptic
        0x1d00..=0x1d7f, // Phonetic Extensions
        0x1e00..=0x1eff, // Latin Extended Additional
        0x2100..=0x214f, // Letterlike Symbols
        0x2190..=0x21ff, // Arrows
        0x2300..=0x23ff, // Miscellaneous Technical
        0x2460..=0x24ff, // Enclosed Alphanumerics
        0x2500..=0x257f, // Box Drawing
        0x2580..=0x259f, // Block Elements
        0x25a0..=0x25ff, // Geometric Shapes
        0x2600..=0x26ff, // Miscellaneous Symbols
        0x2700..=0x27bf, // Dingbats
        0x2b00..=0x2bff, // Miscellaneous Symbols and Arrows
        0x2e00..=0x2e7f, // Supplemental Punctuation
    ];
    ranges
        .into_iter()
        .flatten()
        .map(|cp| char::from_u32(cp).expect("scanned ranges exclude surrogates"))
        .find(|&c| inter_charmap.map(c) != 0 && fira_charmap.map(c) == 0)
        .expect("the Inter fixture must cover something Fira Mono does not")
}

#[test]
fn policy_validation_rejects_malformed_and_unbounded_input() {
    let fira = fira_mono();
    let mono = vec![vec![fira.clone()]];
    let cases: Vec<(&str, OwnedFontPolicy)> = vec![
        (
            "no groups",
            OwnedFontPolicy {
                groups: vec![],
                weight: 400,
            },
        ),
        (
            "empty group",
            OwnedFontPolicy {
                groups: vec![vec![]],
                weight: 400,
            },
        ),
        (
            "weight zero",
            OwnedFontPolicy {
                groups: mono.clone(),
                weight: 0,
            },
        ),
        (
            "weight above 1000",
            OwnedFontPolicy {
                groups: mono.clone(),
                weight: 1001,
            },
        ),
        (
            "empty bytes",
            OwnedFontPolicy {
                groups: vec![vec![OwnedFace {
                    bytes: Arc::from(&b""[..]),
                    index: 0,
                }]],
                weight: 400,
            },
        ),
        (
            "face index out of bounds",
            OwnedFontPolicy {
                groups: vec![vec![OwnedFace {
                    bytes: fira.bytes.clone(),
                    index: 5,
                }]],
                weight: 400,
            },
        ),
        (
            "garbage bytes",
            OwnedFontPolicy {
                groups: vec![vec![OwnedFace {
                    bytes: Arc::from(&b"not a font"[..]),
                    index: 0,
                }]],
                weight: 400,
            },
        ),
        (
            "truncated font",
            OwnedFontPolicy {
                groups: vec![vec![OwnedFace {
                    bytes: Arc::from(&fira.bytes[..100]),
                    index: 0,
                }]],
                weight: 400,
            },
        ),
        (
            "too many groups",
            OwnedFontPolicy {
                groups: (0..=super::owned::MAX_GROUPS)
                    .map(|_| vec![fira.clone()])
                    .collect(),
                weight: 400,
            },
        ),
        (
            "too many faces",
            OwnedFontPolicy {
                groups: vec![vec![fira.clone(); super::owned::MAX_FACES + 1]],
                weight: 400,
            },
        ),
        (
            "oversized source",
            OwnedFontPolicy {
                groups: vec![vec![OwnedFace {
                    bytes: vec![0u8; super::owned::MAX_FACE_BYTES + 1].into(),
                    index: 0,
                }]],
                weight: 400,
            },
        ),
    ];
    for (label, policy) in cases {
        let result = PreparedRaster::prepare(policy, 1.0, 13.0, Cursor::Block);
        assert!(result.is_err(), "{label}: accepted an invalid policy");
    }
}

#[test]
fn the_selected_primary_cannot_be_replaced_by_a_later_mono_group() {
    for policy in [
        OwnedFontPolicy {
            groups: vec![vec![inter_variable()]],
            weight: 400,
        },
        OwnedFontPolicy {
            groups: vec![vec![inter_regular()]],
            weight: 400,
        },
        OwnedFontPolicy {
            groups: vec![vec![inter_variable()], vec![fira_mono()]],
            weight: 500,
        },
    ] {
        let error = Raster::from_owned(policy, 1.0, 13.0, Cursor::Block)
            .err()
            .expect("proportional selected primary must be refused");
        assert!(error.contains("selected primary"), "{error}");
    }
}

#[test]
fn every_face_must_provide_the_exact_effective_weight() {
    let regular = owned_raster(vec![vec![fira_mono()]], 500);
    assert_eq!(regular.weight, 500);
    assert!(
        regular
            .unicode
            .fonts
            .primary
            .variations
            .iter()
            .all(|&coord| coord == 0)
    );
    assert_eq!(
        regular.unicode.fonts.primary.font().attributes().weight().0,
        500
    );
    for weight in [350, 400, 650] {
        let error = Raster::from_owned(
            OwnedFontPolicy {
                groups: vec![vec![fira_mono()]],
                weight,
            },
            1.0,
            13.0,
            Cursor::Block,
        )
        .err()
        .expect("static mismatch must be refused");
        assert!(error.contains("effective weight"), "{error}");
    }
    let error = Raster::from_owned(
        OwnedFontPolicy {
            groups: vec![vec![fira_mono()], vec![inter_regular()]],
            weight: 500,
        },
        1.0,
        13.0,
        Cursor::Block,
    )
    .err()
    .expect("a mismatched fallback must also be refused");
    assert!(error.contains("group 1 face 0"), "{error}");
    let error = Raster::from_owned(
        OwnedFontPolicy {
            groups: vec![vec![inter_variable()]],
            weight: 1000,
        },
        1.0,
        13.0,
        Cursor::Block,
    )
    .err()
    .expect("out-of-range variable weight must be refused before primary selection");
    assert!(error.contains("effective weight"), "{error}");
}

#[test]
fn variable_face_coordinates_drive_metrics_and_unicode_ink() {
    // These proportional variable faces cannot be Term primaries. Exercise
    // the shared face/shaper/scaler lane directly at its exact coordinates.
    let bytes = inter_variable().bytes;
    let light = Face::with_weight(bytes.clone(), 0, 350).unwrap();
    let bold = Face::with_weight(bytes, 0, 650).unwrap();
    assert_ne!(light.variations, bold.variations);
    let glyph = light.font().charmap().map('e');
    assert_ne!(
        light
            .font()
            .glyph_metrics(&light.variations)
            .advance_width(glyph),
        bold.font()
            .glyph_metrics(&bold.variations)
            .advance_width(glyph),
    );
    let paint = |face: Face| {
        let mut unicode = UnicodeRaster::new(Fonts::owned(face, Vec::new()));
        let image = unicode.image("e", 1, 32.0, (48, 64), 48);
        assert!(image.font.is_some() && !image.layers.is_empty());
        let mut pixels = vec![0; 48 * 64 * 4];
        super::paint_cluster(
            image,
            &mut pixels,
            48 * 4,
            0,
            0,
            48,
            64,
            [255, 255, 255],
            PixelFormat::Rgba,
        );
        pixels
    };
    let light = paint(light);
    let bold = paint(bold);
    assert!(light.iter().any(|&sample| sample != 0));
    assert_ne!(light, bold);
}

#[test]
fn distinct_fallback_sources_keep_the_declared_order() {
    let italic = inter_variable_italic();
    let upright = inter_variable();
    let italic_font = FontRef::from_index(&italic.bytes, 0).unwrap();
    let upright_font = FontRef::from_index(&upright.bytes, 0).unwrap();
    assert_ne!(family(italic_font), family(upright_font));
    let mono = fira_mono();
    let mono_font = FontRef::from_index(&mono.bytes, 0).unwrap();
    let c = (0x00a0..=0x3000)
        .filter_map(char::from_u32)
        .find(|&c| {
            mono_font.charmap().map(c) == 0
                && italic_font.charmap().map(c) != 0
                && upright_font.charmap().map(c) != 0
        })
        .expect("both declared fallback fixtures must share coverage absent from Fira");
    for sources in [vec![italic.clone(), upright.clone()], vec![upright, italic]] {
        let mut raster = owned_raster(vec![vec![mono.clone()], sources.clone()], 500);
        let coverage = raster.unicode.fonts.declared_coverage();
        assert_eq!(coverage.len(), 2);
        assert!(Arc::ptr_eq(&coverage[0].source(), &sources[0].bytes));
        assert!(Arc::ptr_eq(&coverage[1].source(), &sources[1].bytes));
        let expected = coverage[0].key();
        let image = raster.unicode.image(
            &c.to_string(),
            1,
            raster.px,
            (raster.width, raster.height),
            raster.baseline,
        );
        assert_eq!(image.font, Some(expected));
    }
}

#[test]
fn declared_unicode_coverage_does_not_discover_missing_fonts() {
    let mut raster = owned_raster(vec![vec![fira_mono()], vec![noto_sans_hebrew()]], 500);
    let expected = raster.unicode.fonts.declared_coverage()[0].key();
    let image = raster.unicode.image(
        "ש",
        1,
        raster.px,
        (raster.width, raster.height),
        raster.baseline,
    );
    assert_eq!(image.font, Some(expected));
    assert!(image.glyphs > 0);
    let mut bare = owned_raster(vec![vec![fira_mono()]], 500);
    for (text, span) in [("ש", 1), ("😀", 2)] {
        let image = bare.unicode.image(
            text,
            span,
            bare.px,
            (bare.width, bare.height),
            bare.baseline,
        );
        assert_eq!(image.font, None, "{text}");
        assert_eq!(image.glyphs, 0, "{text}");
    }
}

#[test]
fn resize_retains_the_original_source_handles_and_coordinates() {
    let policy = OwnedFontPolicy {
        groups: vec![
            vec![fira_mono()],
            vec![inter_variable()],
            vec![noto_sans_hebrew()],
        ],
        weight: 500,
    };
    let raster = Raster::from_owned(policy.clone(), 1.0, 13.0, Cursor::Block).unwrap();
    let resized = raster.resized(2.5, 11.0).unwrap();
    let fresh = Raster::from_owned(policy.clone(), 2.5, 11.0, Cursor::Block).unwrap();
    assert!(Arc::ptr_eq(&raster.unicode.fonts, &resized.unicode.fonts));
    assert!(Arc::ptr_eq(&raster.data, &resized.data));
    assert_eq!(resized.weight, 500);
    assert_eq!(
        (resized.width, resized.height, resized.baseline),
        (fresh.width, fresh.height, fresh.baseline)
    );
    for (original, retained) in raster
        .unicode
        .fonts
        .declared_coverage()
        .iter()
        .zip(resized.unicode.fonts.declared_coverage())
    {
        assert!(Arc::ptr_eq(&original.source(), &retained.source()));
        assert_eq!(original.key(), retained.key());
        assert_eq!(original.variations, retained.variations);
    }
    let prepared = PreparedRaster::prepare(policy, 1.0, 13.0, Cursor::Block).unwrap();
    let resized = prepared.resized(2.5, 11.0).unwrap();
    assert_eq!(resized.cell(), (fresh.width, fresh.height));
    assert_eq!(resized.baseline(), fresh.baseline);
    assert_eq!(prepared.cell(), (raster.width, raster.height));
}

#[test]
fn invalid_geometry_is_refused_at_prepare_and_every_resize_boundary() {
    let policy = OwnedFontPolicy {
        groups: vec![vec![fira_mono()]],
        weight: 500,
    };
    let prepared = PreparedRaster::prepare(policy.clone(), 1.0, 13.0, Cursor::Block).unwrap();
    let raster = prepared.clone().activate();
    for (scale, size) in [
        (f32::NAN, 13.0),
        (f32::INFINITY, 13.0),
        (0.0, 13.0),
        (-1.0, 13.0),
        (0.49, 13.0),
        (8.01, 13.0),
        (1.0, f32::NAN),
        (1.0, f32::INFINITY),
        (1.0, 0.0),
        (1.0, -13.0),
        (8.0, f32::MAX),
    ] {
        assert!(PreparedRaster::prepare(policy.clone(), scale, size, Cursor::Block).is_err());
        assert!(prepared.resized(scale, size).is_err());
        assert!(raster.resized(scale, size).is_err());
    }
    for scale in [0.5, 8.0] {
        assert!(prepared.resized(scale, 13.0).is_ok());
    }
}

#[test]
fn preparation_is_send_sync_and_activation_preserves_ink_and_retained_state() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<PreparedRaster>();
    let policy = OwnedFontPolicy {
        groups: vec![vec![fira_mono()], vec![inter_variable()]],
        weight: 500,
    };
    let prepared =
        PreparedRaster::prepare(policy.clone(), 1.25, 21.333, Cursor::Underline).unwrap();
    let mut activated = prepared.clone().activate();
    let mut direct = Raster::from_owned(policy, 1.25, 21.333, Cursor::Underline).unwrap();
    assert!(Arc::ptr_eq(
        &activated.unicode.fonts,
        &prepared.clone().activate().unicode.fonts
    ));
    assert_eq!(
        (
            activated.width,
            activated.height,
            activated.baseline,
            activated.weight
        ),
        (direct.width, direct.height, direct.baseline, direct.weight)
    );
    let c = inter_only_codepoint();
    assert_eq!(render_char(&mut activated, c), render_char(&mut direct, c));
    let before = render_char(&mut activated, 'M');
    let misses = activated.unicode.misses;
    let mut resized = activated.resized(2.0, 9.5).unwrap();
    let _ = render_char(&mut resized, c);
    assert_eq!(activated.unicode.misses, misses);
    assert_eq!(render_char(&mut activated, 'M'), before);
}

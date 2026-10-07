//! Legacy guard: variable-font weight matching.
//!
//! Uses the retained `InterVariable-Italic.ttf` fixture: the upstream
//! archive's `InterVariable.ttf` is deliberately not carried in this tree
//! (see `PATCHES.md`). The fixture bytes are compiled in source-relatively,
//! so the file builds unchanged from its own test target or when the
//! root-owned guard target path-includes it; no host font participates.

use std::sync::Arc;

use cosmic_text::fontdb::{self, Database, Source};
use cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Weight};

/// Variable fonts must be matched at all weights within their `wght` axis
/// range, not just the default weight they register at in fontdb, otherwise
/// they will fall back to a system font despite being able to provide the
/// requested weight.
///
/// The database is an explicit empty one holding only the fixture, so the
/// face's family, style and ID are discovered from the parsed bytes and no
/// host font can supply a same-named family.
#[test]
fn variable_font_all_weights_match() {
    let bytes: &'static [u8] = include_bytes!("../fonts/InterVariable-Italic.ttf");

    let mut db = Database::new();
    let loaded = db.load_font_source(Source::Binary(Arc::new(bytes.to_vec())));
    assert!(!loaded.is_empty(), "the variable fixture must parse");

    let mut font_system = FontSystem::new_with_locale_and_db("en-US".into(), db);
    let face = font_system
        .db()
        .faces()
        .next()
        .expect("the database holds only the fixture")
        .clone();
    let variable_family = face
        .families
        .first()
        .map(|(name, _)| name.clone())
        .expect("variable fixture family");
    let face_id = face.id;

    for w in [100, 200, 300, 400, 500, 600, 700, 800, 900] {
        let metrics = Metrics::new(16.0, 20.0);
        let mut buffer = Buffer::new(&mut font_system, metrics);

        let glyph_font_ids: Vec<fontdb::ID>;
        {
            let mut buffer = buffer.borrow_with(&mut font_system);
            let attrs = Attrs::new()
                .family(Family::Name(&variable_family))
                .style(face.style)
                .weight(Weight(w));
            buffer.set_size(Some(300.0), Some(100.0));
            buffer.set_text("Hello world", &attrs, Shaping::Advanced, None);
            buffer.shape_until_scroll(true);

            glyph_font_ids = buffer
                .layout_runs()
                .flat_map(|run| run.glyphs.iter().map(|g| g.font_id))
                .collect();
        }

        assert!(!glyph_font_ids.is_empty(), "Weight {w}: no glyphs produced");

        for id in &glyph_font_ids {
            assert_eq!(
                *id, face_id,
                "Weight {w}: expected the fixture face ID, got {id:?}"
            );
        }
    }
}

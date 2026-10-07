use std::path::PathBuf;

use cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Weight, fontdb};

/// Variable fonts must be matched at all weights within their `wght` axis
/// range, not just the default weight they register at in fontdb, otherwise
/// they will fall back to a system font despite being able to provide the
/// requested weight.
///
/// Uses the retained `InterVariable-Italic.ttf` fixture: the upstream
/// archive's `InterVariable.ttf` is deliberately not carried in this tree
/// (see `PATCHES.md`). The variable face's family name and style are
/// discovered from the parsed bytes, never assumed from a fixed string.
#[test]
fn variable_font_all_weights_match() {
    let repo_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let fonts_path = PathBuf::from(&repo_dir).join("fonts");

    let mut font_system = FontSystem::new();
    font_system
        .db_mut()
        .load_font_data(std::fs::read(fonts_path.join("InterVariable-Italic.ttf")).unwrap());

    // The fixture is loaded after the system fonts, so the last face whose
    // family mentions Inter is the one this test owns; its family name is
    // whatever the parsed bytes declare.
    let variable_family = font_system
        .db()
        .faces()
        .filter(|face| face.families.iter().any(|(name, _)| name.contains("Inter")))
        .last()
        .and_then(|face| face.families.first())
        .map(|(name, _)| name.clone())
        .expect("variable face family");

    for w in [100, 200, 300, 400, 500, 600, 700, 800, 900] {
        let metrics = Metrics::new(16.0, 20.0);
        let mut buffer = Buffer::new(&mut font_system, metrics);

        let glyph_font_ids: Vec<fontdb::ID>;
        {
            let mut buffer = buffer.borrow_with(&mut font_system);
            let attrs = Attrs::new()
                .family(Family::Name(&variable_family))
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
            let face = font_system.db().face(*id).unwrap();
            let family = &face.families[0].0;
            assert!(
                family.contains("Inter"),
                "Weight {w}: expected Inter, got \"{family}\""
            );
        }
    }
}

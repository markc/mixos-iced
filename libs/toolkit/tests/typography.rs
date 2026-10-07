// SPDX-License-Identifier: MIT OR Apache-2.0
use std::collections::BTreeMap;
use toolkit::core::{
    Font,
    font::{Family, Weight},
};
use toolkit::{
    FontSet,
    fonts::{self, FontChoice, Role},
    typography::{TextStyle, Typography},
};

#[test]
fn registered_selection_checks_resources_weight_and_explicit_fallback() {
    fonts::install(
        FontSet::new().sans(
            include_bytes!("../../../vendor/font/Inter-VariableFont_opsz,wght.ttf").as_slice(),
        ),
        None,
    )
    .unwrap();
    let light = fonts::try_font_for("Inter", &[], 300, false, None, false).unwrap();
    assert_eq!(light.font.weight, Weight::Light);
    assert_eq!(light.choice, FontChoice::Declared);
    let bold = fonts::try_font_for("missing", &[], 700, false, Some(Role::Sans), false).unwrap();
    assert_eq!(bold.font.weight, Weight::Bold);
    assert_eq!(bold.choice, FontChoice::InstalledRole);
    assert_eq!(bold.font.family, light.font.family);
    let folded = fonts::try_font_for("iNtEr", &[], 400, false, None, false).unwrap();
    assert_eq!(folded.font.family, Family::Name("Inter"));
    assert!(fonts::try_font_for("missing", &[], 400, false, None, false).is_err());
    assert_eq!(
        fonts::try_font_for("missing", &["Inter".into()], 400, false, None, false)
            .unwrap()
            .choice,
        FontChoice::DeclaredFallback
    );
    assert_eq!(
        fonts::try_font_for("missing", &[], 400, false, None, true)
            .unwrap()
            .choice,
        FontChoice::Generic
    );
    assert!(fonts::try_font_for("Inter", &[], 0, false, None, false).is_err());
    // Metadata can outlive a file. Presence in fontdb alone must not make a
    // usable stage; probe the face, then try the next declared family.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("missing.ttf");
    {
        let mut system = toolkit::graphics::text::font_system().write().unwrap();
        let db = system.raw().db_mut();
        let mut face = db
            .faces()
            .find(|face| face.families.iter().any(|(name, _)| name == "Inter"))
            .unwrap()
            .clone();
        face.id = toolkit::graphics::text::cosmic_text::fontdb::ID::dummy();
        face.families[0].0 = "MissingResourceFixture".into();
        face.source = toolkit::graphics::text::cosmic_text::fontdb::Source::File(path);
        db.push_face_info(face);
    }
    assert!(fonts::try_font_for("MissingResourceFixture", &[], 400, false, None, false).is_err());
    assert_eq!(
        fonts::try_font_for(
            "MissingResourceFixture",
            &["Inter".into()],
            400,
            false,
            None,
            false
        )
        .unwrap()
        .choice,
        FontChoice::DeclaredFallback
    );
}
#[test]
fn typography_rejects_invalid_sizes_without_mutable_record_access() {
    let style = TextStyle {
        font: Font::DEFAULT,
        size: 14.0,
        line_height: Some(18.0),
    };
    let valid = Typography::new(BTreeMap::from([("ui".into(), style)])).unwrap();
    assert_eq!(valid.get("ui"), Some(style));
    assert!(valid.get("missing").is_none());
    let mut copy = valid.records().clone();
    copy.get_mut("ui").unwrap().size = 28.0;
    assert_eq!(valid.get("ui").unwrap().size, 14.0);
    for size in [0.0, -1.0, f32::NAN, f32::INFINITY] {
        assert!(
            Typography::new(BTreeMap::from([("ui".into(), TextStyle { size, ..style })])).is_err()
        );
    }
}

// SPDX-License-Identifier: MIT OR Apache-2.0
//! Shared native-view test support. Resources must be installed by the owning
//! asset producer before the test; no generic font substitution is accepted.
pub use iced_test::*;

#[cfg(feature = "settings-native")]
pub fn desktop_presentation(text_scale: f64) -> appearance::settings::Prepared {
    let root = std::env::var_os("MIXOS_TEST_ASSETS")
        .expect("MIXOS_TEST_ASSETS must name the owning verified asset installation");
    let mut desktop = settings::Desktop::default();
    desktop.ui.text_scale = text_scale;
    let effective = settings::resolve(&desktop).expect("valid desktop presentation");
    let projection = appearance::settings::Projection::new(&effective["desktop"])
        .expect("valid desktop projection");
    let mut resources = appearance::resources::ResourceHost::new(
        assets::Lookup::new().root(std::path::PathBuf::from(root)),
    );
    let prepared = resources.prepare(
        projection, None, None,
        appearance::resources::ResourceRequirements::empty(), &mut || Ok(()),
    ).expect("complete native resources must prepare");
    assert!(prepared.resources().and_then(|resources| resources.binding()).is_some(),
        "a generic rescue cannot prove native view fit");
    prepared
}

pub fn assert_visible_bounds(bounds: iced::Rectangle, viewport: iced::Size) {
    assert!(bounds.width > 0.0 && bounds.height > 0.0
        && bounds.x >= 0.0 && bounds.y >= 0.0
        && bounds.x + bounds.width <= viewport.width
        && bounds.y + bounds.height <= viewport.height,
        "control {bounds:?} must fit inside {viewport:?}");
}

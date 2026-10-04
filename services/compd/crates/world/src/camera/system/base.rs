use slots::trait_::system::base::{System, WorldBuilder};
use crate::viewport::state::state::{OutputViews, OUTPUT_VIEWS};

// The camera is pinned to identity (`camera::pin`): nothing here moves it (no
// eased navigation, no scroll/pinch zoom, no touchpad momentum, edge or
// Hand-grab pan, no persisted viewport tree). What is left owns the viewport
// slot the renderer reads, which stays at its default (zoom 1, position 0)
// because nothing writes it.

/// Owns the per-world viewport slot (`OUTPUT_VIEWS`). Nothing else.
#[derive(Default)]
pub struct CameraSystem;

impl System for CameraSystem {
    fn name(&self) -> &'static str {
        "camera"
    }

    fn register(&mut self, builder: &mut WorldBuilder) {
        builder.storage.insert(&OUTPUT_VIEWS, OutputViews::default());
    }
}

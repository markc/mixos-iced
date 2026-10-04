use protocols::space::state::SpaceState;
use slots::storage::slot::base::Storage;
use slots::trait_::system::base::System;
use slots::world::host::base::World;
use crate::host::space::base::{SpaceHost, SPACE};

/// Build a SPATIAL world: it hosts a window `Space` (seeded empty; the output is
/// mapped in post-init) and implements `WindowHost` via the SPACE slice. One per
/// monitor. The feature systems are injected by the caller (the loader knows the
/// concrete set); this only stamps the world *kind*.
pub fn spatial(id: uuid::Uuid, name: &'static str, systems: Vec<Box<dyn System>>, kernel: &Storage) -> World {
    let mut world = World::build(id, name, systems, kernel);
    world
        .storage_mut()
        .insert(&SPACE, SpaceHost::new(SpaceState { state: smithay::desktop::Space::default() }));
    // The per-world draw-order authority (window/iced/bevy interleave).
    world.storage_mut().insert(
        &crate::order::track::base::DRAW_ORDER,
        crate::order::track::base::DrawOrder::new(),
    );
    world
}

/// Build an OVERLAY world: no `Space`, no `WindowHost` (lock, selection). It does
/// not manage client windows; the spatial world's spawn-target keeps them.
pub fn overlay(id: uuid::Uuid, name: &'static str, systems: Vec<Box<dyn System>>, kernel: &Storage) -> World {
    World::build(id, name, systems, kernel)
}

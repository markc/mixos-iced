use slots::storage::slot::base::Storage;
use slots::world::host::base::World;
use std::collections::HashMap;
use uuid::Uuid;

/// Fixed identities for the static worlds, stable across restarts so their saved
/// state reloads. Picker-created worlds get generated `Uuid::now_v7()` instead.
/// Defined one layer down (`system.world/world.identity`) so crates below the world
/// manager — the persist loader — can name them without depending back on it.
pub use slots::world::identity::base::{KERNEL, LOCK_WORLD, MAIN_WORLD, PICKER_WORLD};
pub const MAX_DESKTOP_WORLDS: usize = 8;

/// Owns every world, identified by UUID. Exactly one world is ACTIVE (receives
/// input, dispatch, update, draw). Worlds never close — switching disables the
/// outgoing world's systems and enables the incoming ones; state is kept.
pub struct WorldManager {
    worlds: Vec<World>,
    index: HashMap<Uuid, usize>,
    /// The KERNEL system host: the systems that run every frame regardless of
    /// which world is active (see `identity::KERNEL`). Not in `worlds` — it can
    /// be neither switched to nor looked up by id, and it owns no window Space —
    /// so it is ticked beside the active world by the frame driver, not instead.
    kernel: World,
    active: Uuid,
    /// The spatial world new client windows map into. Invariant: always a valid
    /// SPATIAL world, so there is always somewhere to spawn.
    spawn_target: Uuid,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_world_storage_survives_bounded_dormant_activation_without_reassignment() {
        let kernel = Storage::new();
        let main = crate::kind::build::base::desktop(MAIN_WORLD, "main", &kernel);
        let host = crate::kind::build::base::overlay(KERNEL, "kernel", vec![], &kernel);
        let mut worlds = WorldManager::new(main, host, &kernel);
        let second = Uuid::from_u128(2);
        worlds.add(crate::kind::build::base::desktop(
            second, "desktop", &kernel,
        ));
        assert_eq!(worlds.active_id(), MAIN_WORLD);
        assert_eq!(worlds.spawn_target(), MAIN_WORLD);
        assert!(
            worlds
                .get(second)
                .storage()
                .try_get(&crate::host::space::base::SPACE)
                .is_some()
        );
        worlds.switch(second, &kernel);
        worlds.set_spawn_target(second);
        assert_eq!(worlds.active_id(), second);
        assert_eq!(worlds.spawn_target(), second);
        worlds.switch(MAIN_WORLD, &kernel);
        worlds.set_spawn_target(MAIN_WORLD);
        assert!(worlds.contains(second));
        while worlds.can_add_desktop() {
            worlds.add(crate::kind::build::base::desktop(
                Uuid::now_v7(),
                "desktop",
                &kernel,
            ));
        }
        assert_eq!(worlds.ids().len(), MAX_DESKTOP_WORLDS);
        assert!(!worlds.can_add_desktop());
        assert!(!worlds.contains(KERNEL));
    }
}

impl WorldManager {
    pub fn can_add_desktop(&self) -> bool {
        self.worlds.len() < MAX_DESKTOP_WORLDS
    }
    /// Start with the initial (main, SPATIAL) world, already active and the
    /// spawn-target. Activation fires `on_enable`.
    pub fn new(mut main: World, mut host: World, kernel: &Storage) -> Self {
        host.enable(kernel);
        main.enable(kernel);
        let id = main.id;
        let mut index = HashMap::new();
        index.insert(id, 0);
        Self {
            worlds: vec![main],
            index,
            kernel: host,
            active: id,
            spawn_target: id,
        }
    }

    /// The kernel system host — ticked every frame, whatever world is active.
    pub fn kernel(&self) -> &World {
        &self.kernel
    }

    pub fn kernel_mut(&mut self) -> &mut World {
        &mut self.kernel
    }

    fn idx(&self, id: Uuid) -> usize {
        *self
            .index
            .get(&id)
            .unwrap_or_else(|| panic!("unknown world {id}"))
    }

    /// The spatial world new toplevels map into (the space-hosting world).
    pub fn spawn_target(&self) -> Uuid {
        self.spawn_target
    }

    /// Reassign the spawn-target spatial world. Caller ensures `id` is SPATIAL.
    /// Returns `true` if the target actually changed (the space the foreign mirror
    /// advertises is the spawn-target's — see `Orchestrator::space_state`).
    pub fn set_spawn_target(&mut self, id: Uuid) -> bool {
        assert!(
            self.index.contains_key(&id),
            "spawn-target to unknown world {id}"
        );
        if self.spawn_target == id {
            return false;
        }
        self.spawn_target = id;
        true
    }

    /// Add a dormant world (no enable). Returns its id.
    pub fn add(&mut self, world: World) -> Uuid {
        let id = world.id;
        self.index.insert(id, self.worlds.len());
        self.worlds.push(world);
        id
    }

    pub fn active_id(&self) -> Uuid {
        self.active
    }
    /// Every world id (insertion order); the loader prewarms each at startup.
    pub fn ids(&self) -> Vec<Uuid> {
        self.worlds.iter().map(|w| w.id).collect()
    }

    pub fn active(&self) -> &World {
        &self.worlds[self.idx(self.active)]
    }

    pub fn active_mut(&mut self) -> &mut World {
        let i = self.idx(self.active);
        &mut self.worlds[i]
    }

    pub fn get(&self, id: Uuid) -> &World {
        &self.worlds[self.idx(id)]
    }

    pub fn get_mut(&mut self, id: Uuid) -> &mut World {
        let i = self.idx(id);
        &mut self.worlds[i]
    }

    /// True if a world with this id exists.
    pub fn contains(&self, id: Uuid) -> bool {
        self.index.contains_key(&id)
    }

    /// Swap the active world: on_disable(outgoing) → on_enable(incoming). No-op if
    /// already active. Panics on an unknown id (wiring bug).
    pub fn switch(&mut self, id: Uuid, kernel: &Storage) {
        assert!(self.index.contains_key(&id), "switch to unknown world {id}");
        if id == self.active {
            return;
        }
        let (out, inc) = (self.idx(self.active), self.idx(id));
        self.worlds[out].disable(kernel);
        self.active = id;
        self.worlds[inc].enable(kernel);
    }
}

//! The Mix Scenes surfaces compd draws itself, as comp.props shows them.
//! Quoin's panels and dialog are layer surfaces, so `surfaces.*`, `stack`
//! and `focus.keyboard` carry them; compd's scene host draws its own
//! in-process, with no `wl_surface` and no registry record. Each scene
//! surface gets a numeric id reserved from the registry's own counter (stable
//! per scene for compd's lifetime; the wire's ids stay numbers) and a fresh
//! generation each time it maps.
//!
//! The owner of the scene host (compd's comp port) feeds this every pass;
//! the projection reads the rows.

use std::collections::BTreeMap;

use surfaces::SurfaceId;

/// Installed by the scene host above world. The point is output-local
/// physical hardware position, after clamp/teleport and before client routing.
pub type PointerHook = fn(&mut crate::state::Loop, Option<smithay::utils::Point<f64, smithay::utils::Physical>>);
/// Topmost scene hotspot route; true consumes this button event.
pub type ButtonHook = fn(&mut crate::state::Loop, u32, bool) -> bool;

thread_local! {
    static POINTER_HOOK: std::cell::Cell<Option<PointerHook>> = const { std::cell::Cell::new(None) };
    static BUTTON_HOOK: std::cell::Cell<Option<ButtonHook>> = const { std::cell::Cell::new(None) };
}

pub fn register_pointer_hook(hook: PointerHook) {
    POINTER_HOOK.set(Some(hook));
}

pub fn register_button_hook(hook: ButtonHook) {
    BUTTON_HOOK.set(Some(hook));
}

pub fn pointer_button(lp: &mut crate::state::Loop, button: u32, pressed: bool) -> bool {
    BUTTON_HOOK.get().is_some_and(|hook| hook(lp, button, pressed))
}

pub fn pointer_motion(lp: &mut crate::state::Loop, point: Option<smithay::utils::Point<f64, smithay::utils::Physical>>) {
    if let Some(hook) = POINTER_HOOK.get() {
        hook(lp, point);
    }
}

/// One scene surface as the scene host reports it this pass.
#[derive(Clone, Debug, PartialEq)]
pub struct SceneInput {
    /// The host's surface key (`scene:<name>`).
    pub key: String,
    /// The comp.props output key (`o_<slug>`).
    pub output: String,
    /// Global logical px.
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// `overlay` (the dialog, an undocked page) or `top` (a docked page).
    pub stratum: &'static str,
    /// `on_demand` or `none`.
    pub interactivity: &'static str,
    /// Logical px a docked page reserves, else 0.
    pub exclusive_zone: i32,
}

/// A scene surface's comp.props row: its input plus the reserved identity.
#[derive(Clone, Debug, PartialEq)]
pub struct SceneRow {
    pub id: SurfaceId,
    pub generation: u64,
    pub input: SceneInput,
}

#[derive(Debug, Default)]
pub struct SceneSurfaces {
    /// Scene key -> its reserved id.
    pub(crate) ids: BTreeMap<String, SurfaceId>,
    /// The mapped scene surfaces, in the host's order.
    pub rows: Vec<SceneRow>,
    /// The scene surface holding the keyboard (the iced focus), if any:
    /// comp.props `focus.keyboard` reports it ahead of the seat's surface.
    pub focus: Option<SurfaceId>,
}

impl SceneSurfaces {
    /// The id of `key` if it was ever seen.
    pub fn id_of(&self, key: &str) -> Option<SurfaceId> {
        self.ids.get(key).copied()
    }

    /// Whether `id` is a mapped scene surface.
    pub fn is_scene(&self, id: SurfaceId) -> bool {
        self.rows.iter().any(|row| row.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::comp::CompState;

    fn dialog(width: f32) -> SceneInput {
        SceneInput {
            key: "scene:editor".into(),
            output: "o_test".into(),
            x: 40.0,
            y: 10.0,
            width,
            height: 620.0,
            stratum: "overlay",
            interactivity: "on_demand",
            exclusive_zone: 0,
        }
    }

    /// One reserved id per scene for good; a fresh generation per map; the
    /// focus resolves through the id; only a real change moves the revision.
    #[test]
    fn scene_rows_keep_their_id_and_take_a_generation_per_map() {
        let mut comp = CompState::default();
        let r0 = comp.revision();
        comp.set_scene_surfaces(vec![dialog(880.0)], Some("scene:editor"));
        let first = comp.scenes.rows[0].clone();
        assert_eq!(comp.scenes.focus, Some(first.id));
        assert!(comp.registry.issued(first.id.0) && comp.registry.get(first.id).is_none());
        let r1 = comp.revision();
        assert_ne!(r0, r1);
        comp.set_scene_surfaces(vec![dialog(880.0)], Some("scene:editor"));
        assert_eq!(comp.revision(), r1, "nothing changed, no revision");
        comp.set_scene_surfaces(vec![dialog(860.0)], Some("scene:editor"));
        assert_eq!(comp.scenes.rows[0].generation, first.generation, "still mapped: same generation");
        // Hidden (no row, the focus gone), then shown again: same id, new generation.
        comp.set_scene_surfaces(vec![], Some("scene:editor"));
        assert!(comp.scenes.rows.is_empty());
        assert_eq!(comp.scenes.focus, None, "an unmapped scene holds no keyboard");
        comp.set_scene_surfaces(vec![dialog(880.0)], None);
        assert_eq!(comp.scenes.rows[0].id, first.id);
        assert!(comp.scenes.rows[0].generation > first.generation);
        assert_eq!(comp.scenes.focus, None);
    }
}

//! Occlusion decisions into the Bus edge pass.
//!
//! The renderer writes its draw-time cull into
//! `window::draw::occlude::record` while it draws; nothing in CompState moves
//! when a decision does, so the edge pass would never diff it. [`service`]
//! turns the windows whose decision changed into a revision bump and a
//! `wayland.occlusion` cause on their rows. Not content: `compd.truth` does
//! not carry occlusion, so the truth revision stays put.

use surfaces::SurfaceId;

use crate::state::Loop;
use crate::window::draw::occlude::record;

/// Once per Bus pass, before the edge pass.
pub fn service(lp: &mut Loop) {
    let changed = record::take_changed();
    if changed.is_empty() {
        return;
    }
    let comp = &mut lp.inner.comp;
    let windows: std::collections::HashSet<SurfaceId> =
        changed.into_iter().filter_map(|uuid| comp.registry.id_for_uuid(uuid)).collect();
    // The window's popups and subsurfaces share its decision (the
    // projection walks `parent`), so their rows changed with it.
    let ids: Vec<u64> = comp
        .registry
        .surface_rows()
        .filter(|record| {
            let mut current = Some(record.id());
            for _ in 0..64 {
                let Some(id) = current else { return false };
                if windows.contains(&id) {
                    return true;
                }
                current = comp.registry.get(id).and_then(|record| record.parent());
            }
            false
        })
        .map(|record| record.id().0)
        .collect();
    comp.occlusion_changed(&ids);
}

#[cfg(test)]
mod tests {
    use crate::comp::CompState;

    #[test]
    fn a_changed_decision_moves_the_edge_pass_with_its_cause_but_not_content() {
        let mut comp = CompState::default();
        let (revision, content) = (comp.revision(), comp.content_revision());
        comp.occlusion_changed(&[]);
        assert_eq!(comp.revision(), revision, "nothing changed");
        comp.occlusion_changed(&[5]);
        assert_ne!(comp.revision(), revision);
        assert_eq!(comp.content_revision(), content, "truth carries no occlusion");
        assert_eq!(comp.causes.resolve("surfaces.s5.occluded"), Some("wayland.occlusion"));
        assert_eq!(comp.causes.resolve("windows.s5.occlusion_reason"), Some("wayland.occlusion"));
        assert_eq!(comp.causes.resolve("surfaces.s6.occluded"), None);
    }
}

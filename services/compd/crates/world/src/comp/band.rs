//! `windows.s<id>.band`: a window
//! demoted to the `bottom` band draws, and is hit, behind every normal
//! window, and focusing or raising it restacks it only within that band.
//!
//! The band is the registry's (`StackBand`, what `windows.*.band` reads);
//! the drawing is the draw-order authority, whose tiers already order both
//! the draw pass and the hit test (`DrawOrder::ordered`): the window's
//! drawable moves to the [`BOTTOM`] tier, below `DrawLayer::CONTENT`, and
//! `raise` keeps a drawable in its own tier.

use surfaces::{StackBand, SurfaceId};

use crate::order::track::base::{ComponentId, DRAW_ORDER_MUT, DrawLayer};
use crate::state::Loop;

/// The draw tier of a `bottom`-band window: behind the content tier, above
/// group frames.
pub const BOTTOM: DrawLayer = DrawLayer(-50);

/// Move window `id` into `band` (`bottom` or `normal`): the registry's band
/// and its drawable's tier, on top of that tier. Returns the old and new
/// band names, or `None` when `id` is not a mapped toplevel. A fullscreen
/// window keeps the normal tier it was lifted to: the request becomes the
/// band it gets back when it leaves fullscreen.
pub fn set(lp: &mut Loop, id: SurfaceId, band: StackBand) -> Option<(&'static str, &'static str)> {
    let record = lp.inner.comp.registry.get(id)?;
    if record.role() != surfaces::SurfaceRole::Toplevel || !record.mapped() {
        return None;
    }
    if lp.inner.comp.fullscreen.lifted(id).is_some() {
        let old = lp.inner.comp.fullscreen.lifted(id)?;
        lp.inner.comp.fullscreen.set_restore_band(id, band);
        return Some((old.name(), band.name()));
    }
    let old = record.band();
    if old != band {
        apply(lp, id, band, "props.set");
    }
    Some((old.name(), band.name()))
}

/// The registry band and the draw tier of window `id`, with `cause`.
pub fn apply(lp: &mut Loop, id: SurfaceId, band: StackBand, cause: &'static str) {
    let comp = &mut lp.inner.comp;
    if comp.registry.set_band(id, band).is_err() {
        return;
    }
    if let Some(uuid) = comp.registry.uuid_for(id) {
        let tier = if band == StackBand::Bottom { BOTTOM } else { DrawLayer::CONTENT };
        let target = lp.inner.worlds.spawn_target();
        lp.inner
            .worlds
            .get_mut(target)
            .storage_mut()
            .get_mut(&DRAW_ORDER_MUT)
            .insert_top(ComponentId(uuid), tier);
    }
    // `windows.*.band` and `stack` moved.
    lp.inner.comp.settings_changed("stack", cause);
    lp.inner.comp.causes.note_window(id.0, cause);
}

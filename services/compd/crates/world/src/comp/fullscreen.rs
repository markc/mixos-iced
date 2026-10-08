//! Fullscreen fills an output.
//!
//! With the camera pinned to identity world coordinates are output-logical,
//! so a fullscreen window covers one output's
//! whole geometry (not the usable area; panels do not shrink it). Which
//! output: the one `comp.window.fullscreen {output}` selected (policy's
//! `Effect::SetFullscreenOutput`, held here by output name), else the output the
//! window overlaps, else the first. The engine's `fullscreen_set` asks [`target`]
//! for the rectangle (the engine call-out) and restores the pre-fullscreen
//! rectangle it stored. A bottom-band window is lifted to the normal band
//! while fullscreen and gets its band back after ([`service`]); a window
//! already fullscreen moves to a newly selected output ([`retarget`]).

use std::collections::HashMap;

use dispatcher::wire::trait_::surface_event::SurfaceHandle;
use dispatcher::wire::trait_::wire_trait::WireTrait;
use smithay::desktop::{Space, Window};
use smithay::output::Output;
use smithay::utils::{Logical, Point, Rectangle, Size};
use surfaces::{StackBand, SurfaceId};

use crate::state::Loop;
use crate::window::interface::record::window::LoopWindow;

#[derive(Debug, Default)]
pub struct FullscreenOutputs {
    /// The output (smithay name) a window's fullscreen was asked for.
    selected: HashMap<SurfaceId, String>,
    /// Each fullscreen window's band to return to on leaving; a non-normal
    /// band is lifted to normal meanwhile.
    restore_band: HashMap<SurfaceId, StackBand>,
}

impl FullscreenOutputs {
    /// `Effect::SetFullscreenOutput` (`None` clears the selection).
    pub fn select(&mut self, id: SurfaceId, output: Option<String>) {
        match output {
            Some(key) => {
                self.selected.insert(id, key);
            }
            None => {
                self.selected.remove(&id);
            }
        }
    }

    /// The output name selected for `id`, if any.
    pub fn selected(&self, id: SurfaceId) -> Option<&str> {
        self.selected.get(&id).map(String::as_str)
    }

    pub fn forget(&mut self, id: SurfaceId) {
        self.selected.remove(&id);
        self.restore_band.remove(&id);
    }

    /// The band a fullscreen window returns to (`None`: not fullscreen).
    pub fn lifted(&self, id: SurfaceId) -> Option<StackBand> {
        self.restore_band.get(&id).copied()
    }

    /// A band set while lifted: what the window returns to.
    pub fn set_restore_band(&mut self, id: SurfaceId, band: StackBand) {
        self.restore_band.insert(id, band);
    }
}

/// The output window `window` goes fullscreen on: the selected one, else the
/// one it overlaps, else the first.
fn output_for(comp: &super::CompState, space: &Space<Window>, window: &Window) -> Option<Output> {
    let selected = SurfaceHandle::of_window(window)
        .and_then(|handle| comp.registry.id_for_handle(&handle))
        .and_then(|id| comp.fullscreen.selected(id).map(str::to_string));
    if let Some(name) = selected
        && let Some(output) = space.outputs().find(|output| output.name() == name)
    {
        return Some(output.clone());
    }
    space
        .outputs_for_element(window)
        .into_iter()
        .next()
        .or_else(|| space.outputs().next().cloned())
}

/// Renderer-free production target, shared with native protocol fixtures.
pub fn target_geometry(
    comp: &super::CompState,
    space: &Space<Window>,
    window: &Window,
) -> Option<Rectangle<i32, Logical>> {
    space.output_geometry(&output_for(comp, space, window)?)
}

/// The engine's `fullscreen_set` entering: the rectangle the fullscreen `window`
/// takes (host Space, logical; its output's whole geometry, `None` with no
/// output). A window in a non-normal band is lifted to the normal one for
/// the duration (a bottom-band video must not stay behind other windows).
pub fn target(lp: &mut Loop, window: &Window) -> Option<(Point<i32, Logical>, Size<i32, Logical>)> {
    // Every fullscreen window holds its band to return to, so a band set
    // while fullscreen lands there instead of demoting it (band::set).
    if let Some(id) = record_id(lp, window)
        && lp.inner.comp.fullscreen.lifted(id).is_none()
        && let Some(band) = lp.inner.comp.registry.get(id).map(|record| record.band())
    {
        lp.inner.comp.fullscreen.set_restore_band(id, band);
        if band != StackBand::Normal {
            super::band::apply(lp, id, StackBand::Normal, "comp.window");
        }
    }
    let owner = lp.inner.world_of_window(window)?;
    let geometry = target_geometry(&lp.inner.comp, &lp.inner.space_of(owner).state, window)?;
    Some((geometry.loc, geometry.size))
}

/// One loop pass: a window that left fullscreen (by any path: the verb, the
/// client's own request, an X11 state change) gets its band back. Read off
/// the windows rather than hooked into the engine's `fullscreen_set`, so every path
/// is covered by one rule.
pub fn service(lp: &mut Loop) {
    if lp.inner.comp.fullscreen.restore_band.is_empty() {
        return;
    }
    let left: Vec<(SurfaceId, StackBand)> = lp
        .inner
        .comp
        .fullscreen
        .restore_band
        .iter()
        .filter(|(id, _)| !window_of(lp, **id).is_some_and(|window| window.is_fullscreen()))
        .map(|(id, band)| (*id, *band))
        .collect();
    for (id, band) in left {
        lp.inner.comp.fullscreen.restore_band.remove(&id);
        let alive = lp
            .inner
            .comp
            .registry
            .get(id)
            .is_some_and(|record| record.mapped());
        if alive && band != StackBand::Normal {
            super::band::apply(lp, id, band, "comp.window");
        }
    }
}

/// `comp.window.fullscreen {output}` on a window that is already fullscreen
/// re-targets it: it moves to the selected output's whole geometry.
/// The engine's `fullscreen_set` returns early for an already-fullscreen window, so
/// the move is made here.
pub fn retarget(lp: &mut Loop, window: &Window) {
    let Some(owner) = lp.inner.world_of_window(window) else {
        return;
    };
    let (comp, space) = lp.inner.comp_world_space_mut(owner);
    if reconcile_window(comp, space, window) {
        comp.mark_input_geometry_dirty();
        lp.state
            .schedule_redraw(dispatcher::state::state::RedrawReason::WindowState);
    }
}

/// Reconcile requested fullscreen against its actual owning Space. A committed
/// old fullscreen buffer during exit does not own a new fullscreen request:
/// keep its staged normal/tile return intact until the client commits it.
pub fn refresh_geometry(
    comp: &super::CompState,
    space: &mut Space<Window>,
    windows: &[(SurfaceId, Window)],
) -> bool {
    let mut changed = false;
    for (_, window) in windows {
        if window.is_fullscreen()
            && protocols::window::ident::ident::states(window).fullscreen
        {
            changed |= reconcile_window(comp, space, window);
        }
    }
    changed
}

fn reconcile_window(
    comp: &super::CompState,
    space: &mut Space<Window>,
    window: &Window,
) -> bool {
    let Some(location) = space.element_location(window) else {
        return false;
    };
    let Some(geometry) = target_geometry(comp, space, window) else {
        return false;
    };
    let slot = crate::camera::transform::translate::slot::decided_size(window);
    if location == geometry.loc && slot == Some(geometry.size) {
        return false;
    }
    // Keep immutable WindowFullscreen restore data, requested state, stack
    // order and committed client facts; only the current target is restaged.
    space.map_element(window.clone(), geometry.loc, false);
    crate::camera::transform::translate::slot::set_expected_size(window, geometry.size);
    protocols::window::shell::shell::stage(window, geometry.size, false);
    protocols::window::shell::shell::send(window);
    true
}

/// The window of record `id`, if one is in a world Space.
fn window_of(lp: &Loop, id: SurfaceId) -> Option<Window> {
    let handle = lp.inner.comp.registry.get(id)?.handle().clone();
    lp.inner
        .all_world_spaces()
        .iter()
        .flat_map(|space| space.state.elements())
        .find(|window| SurfaceHandle::of_window(window).as_ref() == Some(&handle))
        .cloned()
}

fn record_id(lp: &Loop, window: &Window) -> Option<SurfaceId> {
    SurfaceHandle::of_window(window)
        .and_then(|handle| lp.inner.comp.registry.id_for_handle(&handle))
}

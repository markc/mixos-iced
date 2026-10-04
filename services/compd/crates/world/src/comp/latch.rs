//! The exclusive-keyboard latch.
//!
//! While a mapped layer surface asks for `KeyboardInteractivity::Exclusive`,
//! it holds the keyboard from the moment it maps, not from the first key:
//! `focus.exclusive_latch` names it, nothing else takes the keyboard (the
//! focus and switch verbs see `exclusive_layer`, a toplevel's popup keyboard
//! grab is denied), and when it goes the keyboard falls back to the highest
//! visible window on the current workspace.
//!
//! Decided 2026-10-03: only the Top and Overlay strata can latch, as the
//! wlr-layer-shell spec guarantees exclusive keyboard only there. Latching
//! any stratum would let a Bottom or Background client asking for Exclusive
//! hold the keyboard forever; compd refuses to seize the keyboard for it. A
//! concealed layer (Quoin's holder enforcement) never latches. A session
//! lock takes precedence: no latch while it is in force.

use dispatcher::wire::trait_::surface_event::SurfaceHandle;
use dispatcher::wire::trait_::wire_trait::WireTrait;
use surfaces::SurfaceId;
use smithay::desktop::layer_map_for_output;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::SERIAL_COUNTER;
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::shell::wlr_layer::{KeyboardInteractivity, Layer};

use crate::state::Loop;

/// The latched layer, as the last [`service`] pass left it.
#[derive(Debug, Default)]
pub struct Latch {
    pub layer: Option<SurfaceId>,
}

/// A layer holds the keyboard latch.
pub fn active(lp: &Loop) -> bool {
    lp.inner.comp.latch.layer.is_some()
}

/// The topmost mapped, unconcealed Top/Overlay layer surface asking for an
/// exclusive keyboard (Overlay before Top; within a stratum, the layer map's
/// topmost first).
fn candidate(lp: &Loop) -> Option<WlSurface> {
    let space = &lp.inner.host_space().state;
    for band in [Layer::Overlay, Layer::Top] {
        for output in space.outputs() {
            let map = layer_map_for_output(output);
            for layer in map.layers_on(band).rev() {
                if layer.cached_state().keyboard_interactivity == KeyboardInteractivity::Exclusive
                    && !super::panels::concealed(layer.wl_surface())
                {
                    return Some(layer.wl_surface().clone());
                }
            }
        }
    }
    None
}

/// The highest visible managed window on the current workspace (draw
/// order), for the keyboard when the latch lets go.
fn fallback(lp: &Loop) -> Option<WlSurface> {
    let comp = &lp.inner.comp;
    let current = comp.current_workspace();
    let order = lp.inner.drawable_order();
    let id = order.iter().find_map(|uuid| {
        comp.registry.surface_rows().find(|record| {
            record.uuid() == Some(*uuid)
                && record.mapped()
                && !record.minimized()
                && record.role().managed_toplevel()
                && record.workspace() == Some(current)
        })
    })?;
    let handle = id.handle().clone();
    lp.inner
        .all_world_spaces()
        .iter()
        .flat_map(|space| space.state.elements())
        .find(|window| SurfaceHandle::of_window(window).as_ref() == Some(&handle))
        .and_then(|window| window.wl_surface().map(|surface| surface.into_owned()))
}

/// One loop pass (beside `session_lock::service`): latch, hold or release.
pub fn service(lp: &mut Loop) {
    let target = if super::session_lock::active(lp) { None } else { candidate(lp) };
    let id = target.as_ref().and_then(|surface| lp.inner.comp.id_for_surface(surface));
    let was = lp.inner.comp.latch.layer;
    lp.state.exclusive_latch = target.clone();
    let Some(keyboard) = lp.state.seat.seat.get_keyboard() else { return };
    match target {
        Some(surface) => {
            if keyboard.current_focus().as_ref() != Some(&surface) {
                keyboard.set_focus(&mut lp.state, Some(surface), SERIAL_COUNTER.next_serial());
            }
        }
        None if was.is_some() && !super::session_lock::active(lp) => {
            let back = fallback(lp);
            keyboard.set_focus(&mut lp.state, back, SERIAL_COUNTER.next_serial());
        }
        None => {}
    }
    if id != was {
        lp.inner.comp.latch.layer = id;
        // `focus.exclusive_latch` moved.
        lp.inner.comp.settings_changed("focus", "wayland.focus");
    }
}

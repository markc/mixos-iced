// SPDX-License-Identifier: MIT OR Apache-2.0
//! Renderer-free window geometry executor, shared by the compositor and its
//! protocol fixtures. Callers resolve windows across their owning worlds before
//! borrowing the placement Space, and own redraw and pointer invalidation.

use std::collections::BTreeMap;

use protocols::window::shell::shell;
use smithay::desktop::{Space, Window};
use smithay::output::Output;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::utils::{Logical, Rectangle};
use surfaces::SurfaceId;
use world::camera::transform::translate::slot;
use world::comp::{CompState, MaximizeRestore, usable::Reserved};
use world::window::interface::record::window::LoopWindow;

/// Work-area observation and actual window placement/configure changes are
/// separate: changing an empty output's usable area need not move a window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GeometryChange {
    pub usable: bool,
    pub windows: bool,
}

/// Layer-shell exclusive zones, followed by compositor-owned docked panels.
pub fn usable_area(
    output: &Output,
    geometry: Rectangle<i32, Logical>,
    reserved: Reserved,
) -> Rectangle<i32, Logical> {
    let map = smithay::desktop::layer_map_for_output(output);
    let layered = if map.layers().next().is_none() {
        geometry
    } else {
        let zone = map.non_exclusive_zone();
        Rectangle::new(
            geometry.loc + zone.loc,
            (zone.size.w.max(0), zone.size.h.max(0)).into(),
        )
    };
    reserved.shrink(layered)
}

fn target(
    comp: &CompState,
    space: &Space<Window>,
    id: SurfaceId,
    window: &Window,
) -> Option<(String, Rectangle<i32, Logical>)> {
    let owner = comp.maximize_restore(id);
    let output = owner
        .as_ref()
        .and_then(|restore| space.outputs().find(|output| output.name() == restore.output).cloned())
        .or_else(|| space.outputs_for_element(window).first().cloned())
        .or_else(|| space.outputs().next().cloned())?;
    let name = output.name();
    let reserved = comp.reserved.get(&name).copied().unwrap_or_default();
    let outer = usable_area(&output, space.output_geometry(&output)?, reserved);
    Some((name, decor::window::content_area(window, outer)))
}

/// An explicit maximise/unmaximise request. With no output, an existing record
/// survives; a new request cannot choose a target yet and makes no change.
pub fn set_maximized(
    comp: &mut CompState,
    space: &mut Space<Window>,
    id: SurfaceId,
    window: &Window,
    enabled: bool,
) -> GeometryChange {
    let area = if enabled {
        let Some((output, area)) = target(comp, space, id, window) else {
            return GeometryChange::default();
        };
        if let Some(mut restore) = comp.maximize_restore(id) {
            if restore.output != output {
                restore.output = output;
                comp.set_maximize_restore(id, Some(restore));
            }
        } else {
            comp.set_maximize_restore(id, Some(MaximizeRestore {
                location: space.element_location(window).unwrap_or(area.loc),
                size: slot::size_of(window).unwrap_or(window.geometry().size),
                output,
            }));
        }
        area
    } else {
        let Some(restore) = comp.maximize_restore(id) else {
            shell::send(window);
            return GeometryChange::default();
        };
        comp.set_maximize_restore(id, None);
        Rectangle::new(restore.location, restore.size)
    };
    apply(space, window, area, enabled);
    GeometryChange { usable: false, windows: true }
}

/// Refresh work areas and their requested-maximised windows. The candidates
/// must be resolved by the caller across all worlds, not just this Space.
pub fn refresh_usable(
    comp: &mut CompState,
    space: &mut Space<Window>,
    windows: &[(SurfaceId, Window)],
) -> GeometryChange {
    let usable: BTreeMap<_, _> = space.outputs().filter_map(|output| {
        let name = output.name();
        let reserved = comp.reserved.get(&name).copied().unwrap_or_default();
        Some((name, usable_area(output, space.output_geometry(output)?, reserved)))
    }).collect();
    let changed = usable != comp.usable;
    if changed {
        comp.usable = usable;
        comp.outputs_changed();
    }
    let mut change = GeometryChange { usable: changed, windows: false };
    for (id, window) in windows {
        let Some(mut restore) = comp.maximize_restore(*id) else { continue };
        // A dormant world's window may be resolved by the caller, but this
        // reconciliation must not admit it into the current placement Space.
        if space.element_location(window).is_none() { continue }
        // Fullscreen owns its geometry through delayed entry/exit commits.
        // Revisit on the next normal dispatch after it releases ownership,
        // even when the work-area map itself has not changed again.
        if fullscreen_owns(window) { continue }
        let Some((output, area)) = target(comp, space, *id, window) else { continue };
        if restore.output != output {
            restore.output = output;
            comp.set_maximize_restore(*id, Some(restore));
        }
        let maximized = if let Some(toplevel) = window.toplevel() {
            toplevel.with_pending_state(|state| state.states.contains(xdg_toplevel::State::Maximized))
        } else {
            window.x11_surface().is_some_and(|x11| x11.is_maximized())
        };
        if space.element_location(window) != Some(area.loc)
            || slot::decided_size(window) != Some(area.size)
            || !maximized {
            apply(space, window, area, true);
            change.windows = true;
        }
    }
    change
}

fn fullscreen_owns(window: &Window) -> bool {
    window.is_fullscreen()
        || protocols::window::ident::ident::states(window).fullscreen
        || window.toplevel().is_some_and(|toplevel| {
            toplevel.with_committed_state(|state| {
                state.is_some_and(|state| state.states.contains(xdg_toplevel::State::Fullscreen))
            })
        })
}

fn apply(space: &mut Space<Window>, window: &Window, area: Rectangle<i32, Logical>, maximized: bool) {
    shell::stage(window, area.size, false);
    if let Some(toplevel) = window.toplevel() {
        toplevel.with_pending_state(|state| {
            if maximized {
                state.states.set(xdg_toplevel::State::Maximized);
            } else {
                state.states.unset(xdg_toplevel::State::Maximized);
            }
        });
    }
    if let Some(x11) = window.x11_surface() {
        let _ = x11.set_maximized(maximized);
    }
    shell::send(window);
    slot::set_expected_size(window, area.size);
    space.map_element(window.clone(), area.loc, false);
}

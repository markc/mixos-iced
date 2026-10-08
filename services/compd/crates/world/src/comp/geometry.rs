// SPDX-License-Identifier: MIT OR Apache-2.0
//! Renderer-free window geometry executor, shared by the compositor and its
//! protocol fixtures. Callers resolve windows across their owning worlds before
//! borrowing the placement Space, and own redraw and pointer invalidation.

use std::collections::BTreeMap;

use crate::camera::transform::translate::slot;
use crate::comp::{CompState, MaximizeRestore, usable::Reserved};
use crate::window::interface::record::window::LoopWindow;
use dispatcher::wire::trait_::surface_event::SurfaceHandle;
use policy::tiling::{
    self, AdmissionError, Constraints, Facts, Group, Insets, Member, Plan, Rect, Target,
};
use protocols::window::shell::shell;
use smithay::desktop::{Space, Window};
use smithay::output::Output;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::Resource;
use smithay::utils::{Logical, Rectangle};
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::xdg::SurfaceCachedState;
use surfaces::SurfaceId;

/// Work-area observation and actual window placement/configure changes are
/// separate: changing an empty output's usable area need not move a window.
/// `windows` reports a geometry application; an explicit request may still send
/// a protocol response without changing placement or its decided slot.
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
        .and_then(|restore| {
            space
                .outputs()
                .find(|output| {
                    output.name() == restore.output && space.output_geometry(output).is_some()
                })
                .cloned()
        })
        // Overlap membership can still contain a just-unmapped output until
        // Space refreshes. It must not prevent an available-output fallback.
        .or_else(|| {
            space
                .outputs_for_element(window)
                .into_iter()
                .find(|output| space.output_geometry(output).is_some())
        })
        .or_else(|| {
            space
                .outputs()
                .find(|output| space.output_geometry(output).is_some())
                .cloned()
        })?;
    let name = output.name();
    let reserved = comp.reserved.get(&name).copied().unwrap_or_default();
    let outer = usable_area(&output, space.output_geometry(&output)?, reserved);
    Some((
        name,
        decor::window::normal_extents(window)
            .map_or(outer, |extents| decor::window::inset(outer, extents)),
    ))
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
            comp.set_maximize_restore(
                id,
                Some(MaximizeRestore {
                    location: normal_restore(comp, id).map_or_else(
                        || space.element_location(window).unwrap_or(area.loc),
                        |normal| normal.loc,
                    ),
                    size: normal_restore(comp, id).map_or_else(
                        || slot::size_of(window).unwrap_or(window.geometry().size),
                        |normal| normal.size,
                    ),
                    output,
                }),
            );
        }
        area
    } else {
        let Some(restore) = comp.maximize_restore(id) else {
            if let Some(area) = tile_return(comp, space, id) {
                apply_return(space, window, area);
                comp.mark_input_geometry_dirty();
                return GeometryChange {
                    usable: false,
                    windows: true,
                };
            }
            shell::set_maximized(window, false);
            shell::send(window);
            return GeometryChange::default();
        };
        comp.set_maximize_restore(id, None);
        if let Some(area) = tile_return(comp, space, id) {
            apply_return(space, window, area);
            comp.mark_input_geometry_dirty();
            return GeometryChange {
                usable: false,
                windows: true,
            };
        }
        Rectangle::new(restore.location, restore.size)
    };
    apply(space, window, area, enabled);
    comp.mark_input_geometry_dirty();
    GeometryChange {
        usable: false,
        windows: true,
    }
}

/// Refresh work areas and their requested-maximised windows. The candidates
/// must be resolved by the caller across all worlds, not just this Space.
pub fn refresh_usable(
    comp: &mut CompState,
    space: &mut Space<Window>,
    windows: &[(SurfaceId, Window)],
) -> GeometryChange {
    let usable: BTreeMap<_, _> = space
        .outputs()
        .filter_map(|output| {
            let name = output.name();
            let reserved = comp.reserved.get(&name).copied().unwrap_or_default();
            Some((
                name,
                usable_area(output, space.output_geometry(output)?, reserved),
            ))
        })
        .collect();
    let changed = usable != comp.usable;
    if changed {
        comp.usable = usable;
        comp.outputs_changed();
    }
    let mut change = GeometryChange {
        usable: changed,
        windows: false,
    };
    for (id, window) in windows {
        let Some(mut restore) = comp.maximize_restore(*id) else {
            continue;
        };
        // A dormant world's window may be resolved by the caller, but this
        // reconciliation must not admit it into the current placement Space.
        if space.element_location(window).is_none() {
            continue;
        }
        // Fullscreen owns its geometry through delayed entry/exit commits.
        // Revisit on the next normal dispatch after it releases ownership,
        // even when the work-area map itself has not changed again.
        if fullscreen_owns(window) {
            continue;
        }
        let Some((output, area)) = target(comp, space, *id, window) else {
            continue;
        };
        if restore.output != output {
            restore.output = output;
            comp.set_maximize_restore(*id, Some(restore));
        }
        let maximized = if let Some(toplevel) = window.toplevel() {
            toplevel
                .with_pending_state(|state| state.states.contains(xdg_toplevel::State::Maximized))
        } else {
            window.x11_surface().is_some_and(|x11| x11.is_maximized())
        };
        if space.element_location(window) != Some(area.loc)
            || slot::decided_size(window) != Some(area.size)
            || !maximized
        {
            apply(space, window, area, true);
            change.windows = true;
        }
    }
    let active = windows_in_space(comp, space);
    change.windows |= refresh_tiles(comp, space, &active);
    if change.windows {
        comp.mark_input_geometry_dirty();
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

fn apply(
    space: &mut Space<Window>,
    window: &Window,
    area: Rectangle<i32, Logical>,
    maximized: bool,
) {
    shell::stage(window, area.size, false);
    shell::set_tiled(window, false);
    shell::set_maximized(window, maximized);
    shell::send(window);
    slot::set_expected_size(window, area.size);
    space.map_element(window.clone(), area.loc, false);
}

fn pure(area: Rectangle<i32, Logical>) -> Rect {
    Rect {
        x: area.loc.x,
        y: area.loc.y,
        width: area.size.w,
        height: area.size.h,
    }
}
fn native(area: Rect) -> Rectangle<i32, Logical> {
    Rectangle::new((area.x, area.y).into(), (area.width, area.height).into())
}

fn normal_restore(comp: &CompState, id: SurfaceId) -> Option<Rectangle<i32, Logical>> {
    let record = comp.registry.get(id)?;
    comp.tiles
        .member(Target {
            id,
            generation: record.generation(),
        })
        .map(|member| native(member.normal))
}

fn constraints(window: &Window) -> Constraints {
    let (min_size, max_size) = window
        .toplevel()
        .map(|top| {
            with_states(top.wl_surface(), |states| {
                let mut cached = states.cached_state.get::<SurfaceCachedState>();
                let current = cached.current();
                (
                    (current.min_size.w, current.min_size.h),
                    (current.max_size.w, current.max_size.h),
                )
            })
        })
        .unwrap_or_default();
    let insets = decor::window::normal_extents(window)
        .map(|e| Insets {
            left: e.left.ceil() as i32,
            right: e.right.ceil() as i32,
            top: e.top.ceil() as i32,
            bottom: e.bottom.ceil() as i32,
        })
        .unwrap_or_default();
    Constraints {
        min_size,
        max_size,
        insets,
    }
}

fn overlay(comp: &CompState, id: SurfaceId, window: &Window) -> bool {
    let requested_fullscreen = protocols::window::ident::ident::states(window).fullscreen;
    let requested_tiled = shell::requested_tiled(window);
    let committed_maximized = window.toplevel().is_some_and(|top| {
        top.with_committed_state(|state| {
            state.is_some_and(|state| state.states.contains(xdg_toplevel::State::Maximized))
        })
    });
    window.is_fullscreen()
        || requested_fullscreen
        || comp.maximize_restore(id).is_some()
        || (!requested_tiled
            && (protocols::window::ident::ident::committed_fullscreen(window)
                || committed_maximized))
}

fn tile_plan(
    comp: &CompState,
    space: &Space<Window>,
    windows: &[(SurfaceId, Window)],
    group: &Group,
    returning: Option<SurfaceId>,
) -> Plan {
    let area = space
        .outputs()
        .find(|output| output.name() == group.output)
        .and_then(|output| {
            space.output_geometry(output).map(|geometry| {
                usable_area(
                    output,
                    geometry,
                    comp.reserved
                        .get(&group.output)
                        .copied()
                        .unwrap_or_default(),
                )
            })
        });
    comp.tiles
        .plan(&comp.registry, group, area.map(pure), |target| {
            windows
                .iter()
                .find(|(id, _)| *id == target.id)
                .map(|(_, window)| Facts {
                    constraints: constraints(window),
                    overlay: space.element_location(window).is_none()
                        || (returning != Some(target.id) && overlay(comp, target.id, window)),
                })
                .unwrap_or(Facts {
                    overlay: true,
                    ..Facts::default()
                })
        })
}

/// Current complete-group failure, derived from the actual work area and
/// committed hints. Membership alone never promises a feasible allocation.
pub fn tile_pending(
    comp: &CompState,
    space: &Space<Window>,
    id: SurfaceId,
) -> Option<tiling::LayoutError> {
    let record = comp.registry.get(id)?;
    let member = comp.tiles.member(Target {
        id,
        generation: record.generation(),
    })?;
    let windows = windows_in_space(comp, space);
    let exiting = windows
        .iter()
        .find(|(window_id, _)| *window_id == id)
        .is_some_and(|(_, window)| {
            !window.is_fullscreen()
                && !protocols::window::ident::ident::states(window).fullscreen
                && protocols::window::ident::ident::committed_fullscreen(window)
        });
    match tile_plan(comp, space, &windows, &member.group, exiting.then_some(id)) {
        Plan::Pending(error) => Some(error),
        Plan::Ready(_) => None,
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct TileFacts {
    pub membership: bool,
    pub requested: bool,
    pub committed: bool,
    pub pending: Option<tiling::LayoutError>,
    pub configure_pending: bool,
}

/// Protocol facts read by the production facade and native wait guards. ACK
/// alone cannot replace committed tiled flags or client window geometry.
pub fn tile_facts(
    comp: &CompState,
    space: &Space<Window>,
    id: SurfaceId,
    window: &Window,
) -> TileFacts {
    let membership = comp.registry.get(id).is_some_and(|record| {
        comp.tiles
            .member(Target {
                id,
                generation: record.generation(),
            })
            .is_some()
    });
    let requested = shell::requested_tiled(window);
    let committed = shell::committed_tiled(window);
    let participant = window
        .toplevel()
        .is_some_and(|top| shell::tile_geometry_participant(top.wl_surface()));
    TileFacts {
        membership,
        requested,
        committed,
        pending: tile_pending(comp, space, id),
        configure_pending: requested != committed
            || ((requested || participant)
                && slot::decided_size(window).is_some_and(|size| size != window.geometry().size)),
    }
}

/// Resolve only windows already in this owning Space. Never map a dormant
/// world's window into the hosted world as a side effect of reconciliation.
pub fn windows_in_space(comp: &CompState, space: &Space<Window>) -> Vec<(SurfaceId, Window)> {
    space
        .elements()
        .filter_map(|window| {
            SurfaceHandle::of_window(window)
                .and_then(|handle| comp.registry.id_for_handle(&handle))
                .map(|id| (id, window.clone()))
        })
        .collect()
}

pub fn refresh_space(comp: &mut CompState, space: &mut Space<Window>) -> GeometryChange {
    let windows = windows_in_space(comp, space);
    refresh_usable(comp, space, &windows)
}

fn refresh_tiles(
    comp: &mut CompState,
    space: &mut Space<Window>,
    windows: &[(SurfaceId, Window)],
) -> bool {
    comp.reconcile_tiles();
    for (id, window) in windows {
        if let Some(top) = window.toplevel() {
            comp.tile_input_admit(*id, top.wl_surface());
        }
    }
    let mut outputs: Vec<_> = space
        .outputs()
        .filter(|output| space.output_geometry(output).is_some())
        .map(|output| output.name())
        .collect();
    outputs.sort();
    if let Some(fallback) = outputs.first() {
        let missing: Vec<_> = comp
            .tiles
            .members()
            .iter()
            .map(|member| member.group.output.clone())
            .filter(|name| !outputs.contains(name))
            .collect();
        if !missing.is_empty() {
            for old in missing {
                comp.tiles.retarget_output(&old, fallback);
            }
            comp.tiling_changed();
        }
    }
    let mut groups = Vec::new();
    for member in comp.tiles.members() {
        if member.group.workspace == comp.current_workspace() && !groups.contains(&member.group) {
            groups.push(member.group.clone());
        }
    }
    let mut changed = false;
    for (id, window) in windows {
        let membership = comp.registry.get(*id).is_some_and(|record| {
            comp.tiles
                .member(Target {
                    id: *id,
                    generation: record.generation(),
                })
                .is_some()
        });
        if !membership && shell::requested_tiled(window) {
            shell::set_tiled(window, false);
            shell::send(window);
            changed = true;
        }
    }
    let mut observation = Vec::with_capacity(groups.len());
    for group in groups {
        let plan = tile_plan(comp, space, windows, &group, None);
        observation.push((
            group.clone(),
            match &plan {
                Plan::Pending(error) => Some(*error),
                Plan::Ready(_) => None,
            },
        ));
        let Plan::Ready(allocations) = plan else {
            continue;
        };
        for allocation in allocations {
            let Some((_, window)) = windows.iter().find(|(id, _)| *id == allocation.target.id)
            else {
                continue;
            };
            let area = native(allocation.cell.content);
            if space.element_location(window) != Some(area.loc)
                || slot::decided_size(window) != Some(area.size)
                || (window.toplevel().is_some() && !shell::requested_tiled(window))
            {
                apply_tile(space, window, area);
                changed = true;
            }
        }
    }
    comp.observe_tile_pending(observation);
    if changed {
        comp.tiling_changed();
    }
    changed
}

fn apply_tile(space: &mut Space<Window>, window: &Window, area: Rectangle<i32, Logical>) {
    shell::set_tiled(window, true);
    shell::set_maximized(window, false);
    shell::stage(window, area.size, false);
    shell::send(window);
    slot::set_expected_size(window, area.size);
    space.map_element(window.clone(), area.loc, false);
}

/// Current normal tile target during an overlay's exit, including target-mode
/// SSD. The returning window participates before its old overlay state commits.
#[derive(Clone, Copy, Debug)]
pub struct TileReturn {
    pub area: Rectangle<i32, Logical>,
    /// A successful complete group plan; false restores immutable normal mode
    /// while membership remains pending, never an obsolete tile rectangle.
    pub tiled: bool,
}

fn apply_return(space: &mut Space<Window>, window: &Window, target: TileReturn) {
    if target.tiled {
        apply_tile(space, window, target.area);
    } else {
        apply(space, window, target.area, false);
    }
}

pub fn tile_return(comp: &CompState, space: &Space<Window>, id: SurfaceId) -> Option<TileReturn> {
    if comp.maximize_restore(id).is_some() {
        return None;
    }
    let record = comp.registry.get(id)?;
    let target = Target {
        id,
        generation: record.generation(),
    };
    let member = comp.tiles.member(target)?;
    if member.group.workspace != comp.current_workspace() {
        return None;
    }
    let windows = windows_in_space(comp, space);
    match tile_plan(comp, space, &windows, &member.group, Some(id)) {
        Plan::Ready(allocations) => allocations
            .into_iter()
            .find(|allocation| allocation.target == target)
            .map(|allocation| TileReturn {
                area: native(allocation.cell.content),
                tiled: true,
            }),
        // Preserve membership/order and its pending plan, but leave the
        // overlay at immutable normal geometry with native tiled flags clear.
        Plan::Pending(_) => Some(TileReturn {
            area: native(member.normal),
            tiled: false,
        }),
    }
}

pub fn set_tiled(
    comp: &mut CompState,
    space: &mut Space<Window>,
    target: Target,
    window: &Window,
    enabled: bool,
    selected_output: Option<&str>,
) -> Result<GeometryChange, AdmissionError> {
    let mut restored = false;
    let id = target.id;
    tiling::resolve_control_target(&comp.registry, target, enabled)?;
    let record = comp.registry.get(id).expect("validated live tile target");
    if enabled
        && window
            .toplevel()
            .is_none_or(|top| top.xdg_toplevel().version() < 2)
    {
        return Err(AdmissionError::UnsupportedProtocol);
    }
    if SurfaceHandle::of_window(window).as_ref() != Some(record.handle()) {
        return Err(AdmissionError::Target(
            surfaces::WindowTargetError::UnknownWindow,
        ));
    }
    if enabled {
        if record.workspace() != Some(comp.current_workspace())
            || space.element_location(window).is_none()
        {
            return Err(AdmissionError::InvalidGroup);
        }
        let requested_output = selected_output.or_else(|| {
            comp.tiles
                .member(target)
                .map(|member| member.group.output.as_str())
        });
        let output = requested_output
            .and_then(|name| {
                space
                    .outputs()
                    .find(|output| output.name() == name && space.output_geometry(output).is_some())
                    .cloned()
            })
            .or_else(|| {
                if selected_output.is_some() {
                    None
                } else {
                    space
                        .outputs_for_element(window)
                        .into_iter()
                        .find(|output| space.output_geometry(output).is_some())
                }
            })
            .or_else(|| {
                if selected_output.is_some() {
                    None
                } else {
                    space
                        .outputs()
                        .find(|output| space.output_geometry(output).is_some())
                        .cloned()
                }
            })
            .ok_or(AdmissionError::Layout(tiling::LayoutError::NoOutput))?;
        let outer = usable_area(
            &output,
            space
                .output_geometry(&output)
                .ok_or(AdmissionError::Layout(tiling::LayoutError::NoOutput))?,
            comp.reserved
                .get(&output.name())
                .copied()
                .unwrap_or_default(),
        );
        let normal = comp
            .maximize_restore(id)
            .map(|restore| Rectangle::new(restore.location, restore.size))
            .or_else(|| {
                window
                    .fullscreen()
                    .map(|restore| Rectangle::new(restore.restore_loc, restore.restore_size))
            })
            .unwrap_or_else(|| {
                Rectangle::new(
                    space.element_location(window).unwrap(),
                    slot::size_of(window).unwrap_or(window.geometry().size),
                )
            });
        let windows = windows_in_space(comp, space);
        let requested = Member {
            target,
            group: Group {
                output: output.name(),
                workspace: comp.current_workspace(),
            },
            normal: pure(normal),
        };
        let facts: BTreeMap<_, _> = windows
            .iter()
            .map(|(id, window)| {
                (
                    id.0,
                    Facts {
                        constraints: constraints(window),
                        overlay: overlay(comp, *id, window),
                    },
                )
            })
            .collect();
        if comp
            .tiles
            .admit(&comp.registry, requested, Some(pure(outer)), |target| {
                facts.get(&target.id.0).copied().unwrap_or(Facts {
                    overlay: true,
                    ..Facts::default()
                })
            })?
        {
            comp.tiling_changed();
        }
    } else if let Some(member) = comp.tiles.remove(&comp.registry, target)? {
        comp.tile_input_retire(id);
        comp.tiling_changed();
        if window.fullscreen().is_some() && comp.maximize_restore(id).is_none() {
            window.set_fullscreen(Some(
                crate::window::interface::record::data::WindowFullscreen {
                    restore_loc: native(member.normal).loc,
                    restore_size: native(member.normal).size,
                },
            ));
        } else if window.fullscreen().is_none() && comp.maximize_restore(id).is_none() {
            shell::set_tiled(window, false);
            shell::stage(window, native(member.normal).size, false);
            shell::send(window);
            slot::set_expected_size(window, native(member.normal).size);
            space.map_element(window.clone(), native(member.normal).loc, false);
            restored = true;
        }
    }
    if enabled && let Some(top) = window.toplevel() {
        comp.tile_input_admit(id, top.wl_surface());
    }
    let mut change = refresh_space(comp, space);
    change.windows |= restored;
    if change.windows {
        comp.mark_input_geometry_dirty();
    }
    Ok(change)
}

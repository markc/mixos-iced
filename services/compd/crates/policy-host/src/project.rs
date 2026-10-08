//! The props-tree projection: `Loop` → `CompSnapshot`.
//!
//! The comp registry says which surfaces exist, their
//! role, generation, mapped/focused/minimised bits, names, pid and workspace;
//! the engine says where they are:
//! - windows: the Space element location and window geometry, the output the
//!   element overlaps, decoration mode, fullscreen, the foreign identifier;
//! - layer surfaces: the layer map's geometry on their output, plus the
//!   `layer.*` row (stratum, interactivity, exclusive zone, binding);
//! - popups: positioned off their root (window or layer) as smithay draws them;
//! - subsurfaces: their parent's origin plus the committed subsurface offset.
//!
//! Coordinates are the host Space's: with the camera pinned to identity,
//! world coordinates are output-logical.
//!
//! `focus.*` reads the primary seat live; `stack` lists the mapped root
//! surfaces topmost first (overlay and top layers, windows in draw order,
//! bottom and background layers). `workspaces` is the model in CompState;
//! a surface hidden by it (minimised or off the current workspace)
//! reads `visible: false`. `bindings` is the binding filter state;
//! `input.corners` is the live corner configuration.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use comp_model::snapshot::{
    CompSnapshot, CornersSnapshot, DecorationSnapshot, DmabufFailureRecord,
    DmabufLedgerSnapshot, FocusSnapshot, FocusWindowSnapshot,
    FullTreeCache, HostInputSnapshot, InfoSnapshot, InputSnapshot, LayerSnapshot, OcclusionCounters,
    OcclusionProps, OcclusionSnapshot, OutputSnapshot, PortSnapshot,
    OutputWorkspaceSnapshot, ReadScopes, RectSnapshot, SourceSnapshot, SurfaceSnapshot, WindowExtras,
    WorkspaceRowSnapshot,
    WorkspacesSnapshot, XwaylandSnapshot, output_key, output_slug_collides, project_window_row,
    surface_key,
};
use dispatcher::wire::trait_::surface_event::SurfaceHandle;
use dispatcher::wire::trait_::wire_trait::WireTrait;
use surfaces::{SurfaceId, SurfaceRecord, SurfaceRole};
use protocols::window::ident::ident;
use world::state::Loop;
use smithay::backend::renderer::utils::with_renderer_surface_state;
use smithay::desktop::{PopupKind, PopupManager, Window, layer_map_for_output};
use smithay::output::Output;
use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode;
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, Size};
use smithay::wayland::compositor::{SubsurfaceCachedState, with_states};
use smithay::wayland::shell::wlr_layer::{ExclusiveZone, KeyboardInteractivity, Layer};

/// What only the transport knows: the port's identity and its counters. The
/// compd adapter fills it from comp-service's `PortContext`, so this crate never
/// depends on the transport.
#[derive(Clone, Debug)]
pub struct Identity {
    pub service: Arc<str>,
    pub version: Arc<str>,
    /// `kms` / `nested`.
    pub backend: &'static str,
    /// The renderer id (`RendererId::label`).
    pub engine: &'static str,
    pub instance: Arc<str>,
    /// `bindings.profile`: `nested` or `kms-live`.
    pub binding_profile: &'static str,
    pub port: PortSnapshot,
}

/// One read snapshot. `scopes` bounds the volatile leaves: `input.seats` is
/// projected only for a read that can reach it, and so are `dmabuf.*` and
/// the `windows.*` / `outputs.*` presentation leaves.
pub fn project(lp: &Loop, identity: Identity, scopes: &ReadScopes) -> CompSnapshot {
    let Identity {
        service,
        version,
        backend,
        engine,
        instance,
        binding_profile,
        mut port,
    } = identity;
    let (mut outputs, output_keys, slug_collisions) = project_outputs(lp);
    // Presentation leaves are volatile: projected only for a read
    // that can reach them.
    let stats = &lp.inner.comp.presentation.stats;
    if scopes.wants("outputs") {
        for (output, key) in &output_keys {
            if let Some(row) = outputs.get_mut(key) {
                row.presentation = Some(comp_model::snapshot::output_presentation(
                    stats.output(&output.name()),
                    stats.epoch_us,
                ));
            }
        }
    }
    let window_presentation = scopes.wants("windows");
    port.slug_collisions = slug_collisions;
    let placements = Placements::of(lp, &output_keys);
    let registry = &lp.inner.comp.registry;
    let mut surfaces: BTreeMap<String, SurfaceSnapshot> = registry
        .surface_rows()
        .map(|record| (surface_key(record.id().0), project_surface(lp, record, &placements)))
        .collect();
    // compd's own scene surfaces: layer rows, as Quoin's are.
    for row in &lp.inner.comp.scenes.rows {
        surfaces.insert(surface_key(row.id.0), project_scene_surface(lp, row));
    }
    // The rule: a `windows.*` row is a mapped xdg toplevel.
    // A session lock empties `windows.*` and the workspaces' counts.
    let locked = world::comp::session_lock::active(lp);
    let windows = surfaces
        .iter()
        .filter(|(_, surface)| !locked && surface.role == "toplevel" && surface.mapped)
        .map(|(key, surface)| {
            let mut row = project_window_row(surface);
            if window_presentation {
                row.presentation = Some(stats.window(row.id, row.generation).map_or_else(
                    || ledger::presentation_stats::PresentationStats::new(stats.epoch_us).leaves(),
                    ledger::presentation_stats::PresentationStats::leaves,
                ));
            }
            (key.clone(), row)
        })
        .collect();
    let workspace_windows = |index: u32| {
        surfaces
            .values()
            .filter(|surface| surface.workspace == Some(index))
            .count() as u32
    };
    let model = &lp.inner.comp.workspaces;
    let workspaces = WorkspacesSnapshot {
        count: model.count,
        current: lp.inner.comp.current_workspace(),
        outputs: output_keys
            .iter()
            .map(|(_, key)| {
                (
                    key.clone(),
                    OutputWorkspaceSnapshot {
                        current: model.current_for(Some(key.as_str())),
                    },
                )
            })
            .collect(),
        list: (1..=model.count)
            .map(|index| WorkspaceRowSnapshot {
                index,
                windows: if locked { 0 } else { workspace_windows(index) },
            })
            .collect(),
    };
    let stack = project_stack(lp, &placements);
    let counters = world::window::draw::occlude::record::counters();
    CompSnapshot {
        // The renderer's draw-time cull as the occlusion snapshot (record.rs).
        occlusion: OcclusionSnapshot {
            counters: OcclusionCounters {
                withheld_opportunities: counters.withheld_opportunities,
                resumes: counters.resumes,
                recomputes: counters.recomputes,
                conservative_fallbacks: counters.conservative_fallbacks,
            },
        },
        info: InfoSnapshot {
            service,
            version,
            backend,
            engine,
            instance,
            explicit_sync_advertised: lp.state.dmabuf.syncobj_state.is_some(),
            // The latched fault: false for good once
            // a release signal, blocker or fence source failed.
            explicit_sync_healthy: dispatcher::wayland::dmabuf::explicit_sync::healthy(),
        },
        outputs,
        surfaces,
        windows,
        workspaces,
        // Content sources: compd's own iced surfaces, volatile,
        // projected only for a read that can reach them.
        sources: if scopes.wants("sources") {
            lp.inner
                .comp
                .presentation
                .sources
                .iter()
                .filter(|(id, _)| scopes.wants(&format!("sources.{id}")))
                .map(|(id, counters)| {
                    (
                        id.clone(),
                        SourceSnapshot {
                            output: source_output_name(lp, counters.output.as_deref()),
                            registered_at_us: counters.registered_at_us,
                            revision: counters.revision,
                            registration: counters.registration,
                            presentation: counters.leaves(),
                        },
                    )
                })
                .collect()
        } else {
            Default::default()
        },
        stack,
        focus: project_focus(lp),
        // The chrome preferences as the decoration handlers read them:
        // `decorations_ssd` and the installed theme's style (the default
        // `mac` before a theme is installed). Read-only (no
        // `decoration.*` set path), so nothing renegotiates open windows.
        // Per-window modes are in `surfaces.s<id>.decoration`.
        decoration: DecorationSnapshot {
            enabled: decor::window::ssd_enabled(),
            style: decor::window::installed().map_or("mac", |theme| theme.deco.style.name()),
        },
        // The binding filter and tables. Before the first Bus pass
        // builds the state, the profile's default table, enabled.
        bindings: lp.inner.comp.bindings.state().map_or_else(
            || {
                policy::bindings::BindingState::for_profile(
                    policy::bindings::BindingProfile::from_name(binding_profile),
                    true,
                )
                .snapshot()
            },
            policy::bindings::BindingState::snapshot,
        ),
        // `input.*`: the seats (volatile, scoped), which seat moved
        // last, and the nested host passthrough leaf.
        input: InputSnapshot {
            seats: scopes.wants("input.seats").then(|| crate::input::project_seats(lp)),
            last_origin: lp.inner.comp.injection.last_origin.map(surfaces::SeatKind::name),
            corners: corners(lp),
            host: lp
                .inner
                .comp
                .injection
                .host_passthrough_available
                .then_some(HostInputSnapshot {
                    passthrough: lp.inner.comp.injection.host_passthrough,
                }),
        },
        // `enabled` is the CONFIGURED switch (next start), the
        // display the live one, null until it is ready.
        xwayland: XwaylandSnapshot {
            enabled: crate::xwayland::configured(),
            persist_path: Arc::from(crate::xwayland::persist_path().to_string_lossy().as_ref()),
            display: x11_wm::display::display::get().map(Arc::from),
            state: crate::xwayland::state().name(),
            failures: crate::xwayland::failures(),
        },
        dmabuf: if scopes.wants("dmabuf") { project_dmabuf(lp) } else { Default::default() },
        port,
        full_tree: FullTreeCache::default(),
    }
}

/// `dmabuf.*`: the import ledger dispatcher keeps (volatile).
fn project_dmabuf(lp: &Loop) -> DmabufLedgerSnapshot {
    let ledger = &lp.state.dmabuf_ledger;
    DmabufLedgerSnapshot {
        accepted: ledger.accepted,
        failed: ledger.failed,
        failures: ledger
            .failures()
            .map(|record| DmabufFailureRecord {
                format: record.format.clone(),
                modifier: record.modifier.clone(),
                reason: record.reason,
                detail: record.detail.clone(),
                at_us: record.at_us,
            })
            .collect(),
    }
}

/// `input.corners`: the live configuration, `holders` (the holder plane is
/// served: compd sends `panel.command` and enforces conceals) and the
/// volatile per-edge `enforced` / `held` counts.
fn corners(lp: &Loop) -> CornersSnapshot {
    let (enforced, held) = crate::panel::edge_counts(lp);
    CornersSnapshot {
        enforced: Some(enforced),
        held: Some(held),
        ..lp.inner.comp.corners.config().into()
    }
}

/// The host Space's outputs as `outputs.o_<slug>` rows:
/// logical position and size, scale, refresh, and the usable area left by
/// layer-shell exclusive zones. The first output in the Space is the default.
/// Also returns each published output's key and the slug collision count.
/// A content source's output as the props tree names it. The scene host
/// registers the engine's output key (`make model serial`); the props tree
/// reports the protocol name. An output no longer present keeps its key.
pub(crate) fn source_output_name(lp: &Loop, key: Option<&str>) -> Option<String> {
    let key = key?;
    Some(
        lp.inner
            .host_space()
            .state
            .outputs()
            .find(|output| world::state::state::output_key(output) == key)
            .map_or_else(|| key.to_owned(), |output| output.name()),
    )
}

pub(crate) fn project_outputs(lp: &Loop) -> (BTreeMap<String, OutputSnapshot>, Vec<(Output, String)>, u64) {
    let space = &lp.inner.host_space().state;
    let default = space.outputs().next().cloned();
    let mut rows = BTreeMap::new();
    let mut keys = Vec::new();
    let mut collisions = 0_u64;
    for output in space.outputs() {
        let name = output.name();
        let key = output_key(&name);
        if output_slug_collides(&rows, &key, &name, &mut collisions) {
            continue;
        }
        let Some(geometry) = space.output_geometry(output) else {
            continue;
        };
        let usable = crate::control::usable_area(output, geometry, crate::control::reserved_for(lp, output));
        keys.push((output.clone(), key.clone()));
        rows.insert(
            key,
            OutputSnapshot {
                default: default.as_ref() == Some(output),
                x: geometry.loc.x,
                y: geometry.loc.y,
                width: u32::try_from(geometry.size.w).unwrap_or(0),
                height: u32::try_from(geometry.size.h).unwrap_or(0),
                scale: output.current_scale().fractional_scale(),
                refresh_mhz: output
                    .current_mode()
                    .map_or(0, |mode| u32::try_from(mode.refresh).unwrap_or(0)),
                usable: RectSnapshot {
                    x: usable.loc.x as f32,
                    y: usable.loc.y as f32,
                    width: usable.size.w as f32,
                    height: usable.size.h as f32,
                },
                presentation: None,
                name,
            },
        );
    }
    (rows, keys, collisions)
}

/// Where the engine has a surface: its buffer origin and size, and what the
/// engine knows beside the registry.
#[derive(Clone, Debug, Default)]
struct Placement {
    origin: (f32, f32),
    size: (f32, f32),
    output: Option<String>,
    /// Windows only: the window-geometry origin and size.
    window: Option<(f32, f32, f32, f32)>,
    visible: bool,
    decoration: Option<&'static str>,
    fullscreen: bool,
    maximized: bool,
    foreign_id: Option<String>,
    layer: Option<LayerSnapshot>,
    band: Option<&'static str>,
}

/// Placements for every surface the engine draws as a root (windows, layer
/// surfaces) and their popups, keyed like the registry. Subsurfaces are
/// resolved off their parent's placement in `project_surface`.
struct Placements {
    by_handle: HashMap<SurfaceHandle, Placement>,
    /// Layer surfaces per stratum, in layer-map order, for `stack`.
    layers: [Vec<SurfaceHandle>; 4],
}

impl Placements {
    fn of(lp: &Loop, output_keys: &[(Output, String)]) -> Self {
        let mut placements = Self {
            by_handle: HashMap::new(),
            layers: Default::default(),
        };
        let key_of = |output: &Output| {
            output_keys
                .iter()
                .find(|(candidate, _)| candidate == output)
                .map(|(_, key)| key.clone())
        };
        for space in lp.inner.all_world_spaces() {
            for window in space.state.elements() {
                let Some(handle) = SurfaceHandle::of_window(window) else { continue };
                let Some(location) = space.state.element_location(window) else { continue };
                let output = space
                    .state
                    .outputs_for_element(window)
                    .first()
                    .and_then(key_of);
                placements.place_window(lp, handle, window, location, output);
            }
        }
        let space = &lp.inner.host_space().state;
        for (output, key) in output_keys {
            let Some(output_geometry) = space.output_geometry(output) else { continue };
            let map = layer_map_for_output(output);
            for layer in map.layers() {
                let Some(geometry) = map.layer_geometry(layer) else { continue };
                let origin = output_geometry.loc + geometry.loc;
                let cached = layer.cached_state();
                let stratum = layer.layer();
                let handle = SurfaceHandle::wl(layer.wl_surface());
                placements.layers[stratum_index(stratum)].push(handle.clone());
                let row = LayerSnapshot {
                    stratum: layer_name(stratum),
                    interactivity: interactivity_name(cached.keyboard_interactivity),
                    exclusive_zone: exclusive_zone_value(cached.exclusive_zone),
                    binding: if lp.inner.comp.layer_explicit(&handle) {
                        "explicit"
                    } else {
                        "default"
                    },
                };
                placements.by_handle.insert(
                    handle,
                    Placement {
                        origin: point(origin),
                        size: size(geometry.size),
                        output: Some(key.clone()),
                        visible: true,
                        layer: Some(row),
                        band: Some(layer_name(stratum)),
                        ..Placement::default()
                    },
                );
                placements.place_popups(layer.wl_surface(), origin, Some(key.clone()));
            }
            // The session lock's surface on this output: the whole
            // output, in the lock band, shown while the lock is in force.
            if let Some(lock) = lp.state.session_lock.surface_for(&output.name()) {
                let handle = SurfaceHandle::wl(lock.wl_surface());
                let extent = surface_size(lock.wl_surface()).unwrap_or(output_geometry.size);
                placements.by_handle.insert(
                    handle,
                    Placement {
                        origin: point(output_geometry.loc),
                        size: size(extent),
                        output: Some(key.clone()),
                        visible: true,
                        band: Some("lock"),
                        ..Placement::default()
                    },
                );
            }
        }
        placements
    }

    fn place_window(
        &mut self,
        lp: &Loop,
        handle: SurfaceHandle,
        window: &Window,
        location: Point<i32, Logical>,
        output: Option<String>,
    ) {
        let geometry = window.geometry();
        // The buffer origin: the element location is the window geometry's.
        let origin = location - geometry.loc;
        let root = ident::surface(window);
        let size = root
            .as_ref()
            .and_then(surface_size)
            .unwrap_or_else(|| window.bbox().size);
        let decoration = window.toplevel().map(|toplevel| {
            match toplevel.with_committed_state(|state| state.and_then(|state| state.decoration_mode)) {
                Some(DecorationMode::ServerSide) => "server",
                Some(DecorationMode::ClientSide) => "client",
                _ => "unbound",
            }
        });
        let foreign_id = window
            .toplevel()
            .and_then(|toplevel| lp.state.foreign.identifier_of(toplevel.wl_surface()));
        self.by_handle.insert(
            handle,
            Placement {
                origin: point(origin),
                size: self::size(size),
                output: output.clone(),
                window: Some((
                    location.x as f32,
                    location.y as f32,
                    geometry.size.w as f32,
                    geometry.size.h as f32,
                )),
                visible: ident::is_drawn(window),
                decoration,
                fullscreen: ident::committed_fullscreen(window),
                maximized: crate::control::committed_maximized(window),
                foreign_id,
                layer: None,
                band: None,
            },
        );
        if let Some(root) = root {
            self.place_popups(&root, origin + geometry.loc, output);
        }
    }

    /// The popups of a root surface, where smithay draws them: the root's
    /// geometry origin plus the popup's offset, less the popup's own geometry
    /// offset.
    fn place_popups(&mut self, root: &WlSurface, geometry_origin: Point<i32, Logical>, output: Option<String>) {
        for (popup, offset) in PopupManager::popups_for_surface(root) {
            let origin = geometry_origin + offset - popup.geometry().loc;
            let handle = match &popup {
                PopupKind::X11(x11) => SurfaceHandle::x11(x11.x11_surface()),
                _ => SurfaceHandle::wl(popup.wl_surface()),
            };
            let size = surface_size(popup.wl_surface()).unwrap_or(popup.geometry().size);
            self.by_handle.insert(
                handle,
                Placement {
                    origin: point(origin),
                    size: self::size(size),
                    output: output.clone(),
                    visible: true,
                    ..Placement::default()
                },
            );
        }
    }

    /// A subsurface's placement: its parent's origin plus its committed offset,
    /// resolved up the parent chain (bounded, so a cycle cannot hang a read).
    fn subsurface(&self, lp: &Loop, record: &SurfaceRecord<SurfaceHandle>) -> Option<Placement> {
        let registry = &lp.inner.comp.registry;
        let mut offset = (0.0_f32, 0.0_f32);
        let mut current = record;
        for _ in 0..16 {
            let surface = wl_surface(lp, current.handle())?;
            let location = with_states(&surface, |states| {
                let mut cached = states.cached_state.get::<SubsurfaceCachedState>();
                cached.current().location
            });
            offset = (offset.0 + location.x as f32, offset.1 + location.y as f32);
            let parent = registry.get(current.parent()?)?;
            if parent.role() != SurfaceRole::Subsurface {
                let base = self.by_handle.get(parent.handle())?;
                let own = surface_size(&wl_surface(lp, record.handle())?).map(size).unwrap_or_default();
                return Some(Placement {
                    origin: (base.origin.0 + offset.0, base.origin.1 + offset.1),
                    size: own,
                    output: base.output.clone(),
                    visible: base.visible,
                    ..Placement::default()
                });
            }
            current = parent;
        }
        None
    }
}

/// One `surfaces.s<id>` row: identity from the registry, place from the engine.
fn project_surface(lp: &Loop, record: &SurfaceRecord<SurfaceHandle>, placements: &Placements) -> SurfaceSnapshot {
    let placement = if record.role() == SurfaceRole::Subsurface {
        placements.subsurface(lp, record)
    } else {
        placements.by_handle.get(record.handle()).cloned()
    }
    .unwrap_or_default();
    let window_row = record.mapped() && record.role() == SurfaceRole::Toplevel;
    // Under a session lock an ordinary
    // surface reads invisible and nameless; the lock's own rows do not.
    let redact = world::comp::session_lock::active(lp) && record.role() != SurfaceRole::Lock;
    let (window_x, window_y, window_width, window_height) = placement.window.unwrap_or_default();
    let occlusion = occlusion_of(lp, record);
    SurfaceSnapshot {
        occlusion,
        id: record.id().0,
        role: record.role().kind(),
        mapped: record.mapped(),
        visible: !redact && record.mapped() && placement.visible && !lp.inner.comp.hidden_id(record.id()),
        x: placement.origin.0,
        y: placement.origin.1,
        width: placement.size.0,
        height: placement.size.1,
        band: placement.band.unwrap_or_else(|| record.band().name()),
        // The stacking keys: `sequence` is the window's draw-order key,
        // allocated monotonically at insert and at every raise (the engine's
        // DrawOrder); surfaces outside the draw
        // order (layers, popups, the lock) read 0. `tree_index` is the
        // back-to-front position in the root's subsurface tree;
        // 0 for a root alone and for an X11 record (no wl_surface of its own).
        sequence: record.uuid().and_then(|uuid| lp.inner.draw_sequence(uuid)).unwrap_or(0),
        tree_index: wl_surface(lp, record.handle())
            .and_then(|surface| protocols::window::ident::ident::tree_index(&surface))
            .unwrap_or(0),
        parent: record.parent().map(|parent| parent.0),
        output: placement.output,
        title: record.title().filter(|_| !redact).cloned(),
        app_id: record.app_id().filter(|_| !redact).cloned(),
        focused: record.focused(),
        activated: record.focused(),
        maximized: placement.maximized,
        fullscreen: placement.fullscreen,
        minimized: record.minimized(),
        workspace: record.workspace_leaf(),
        decoration: (record.role() == SurfaceRole::Toplevel)
            .then_some(placement.decoration.unwrap_or("unbound")),
        layer: placement.layer,
        foreign_id: window_row.then_some(placement.foreign_id).flatten(),
        generation: record.generation(),
        window: WindowExtras {
            window_x,
            window_y,
            window_width,
            window_height,
            pid: window_row.then(|| record.pid()).flatten(),
            workspace: record.workspace().unwrap_or(0),
        },
    }
}

/// A surface's occlusion leaves: its window's decision from the
/// renderer's record, shared by the window's popups and subsurfaces (found up
/// the `parent` chain), decided per surface of the tree. `unknown`
/// under a session lock (the cull does not run, so the record holds its
/// pre-lock answers) and for a surface with no window.
fn occlusion_of(lp: &Loop, record: &SurfaceRecord<SurfaceHandle>) -> OcclusionProps {
    if world::comp::session_lock::active(lp) {
        return OcclusionProps::default();
    }
    let registry = &lp.inner.comp.registry;
    let mut current = record;
    for _ in 0..64 {
        if let Some(uuid) = current.uuid() {
            let props = world::window::draw::occlude::record::props(&uuid);
            return OcclusionProps {
                occluded: props.occluded,
                occlusion_reason: props.reason,
                occlusion_revision: props.revision,
            };
        }
        let Some(parent) = current.parent().and_then(|parent| registry.get(parent)) else {
            break;
        };
        current = parent;
    }
    OcclusionProps::default()
}

/// `focus.*` from the primary seat, resolved to registry ids.
pub(crate) fn project_focus(lp: &Loop) -> FocusSnapshot {
    let comp = &lp.inner.comp;
    let seat = &lp.state.seat.seat;
    let id = |surface: Option<WlSurface>| surface.and_then(|surface| comp.id_for_surface(&surface)).map(|id| id.0);
    let pointer = seat.get_pointer();
    // A scene surface holding the iced keyboard focus takes every key (the
    // seat routes them to iced), so it is the keyboard focus, not the window
    // the seat last focused. Not under a session lock: the lock owns it.
    let scene_keyboard = comp
        .scenes
        .focus
        .filter(|_| !world::comp::session_lock::active(lp))
        .map(|id| id.0);
    FocusSnapshot {
        keyboard: scene_keyboard.or_else(|| id(seat.get_keyboard().and_then(|keyboard| keyboard.current_focus()))),
        // The latched Top/Overlay layer (every stratum latches).
        exclusive_latch: lp.inner.comp.latch.layer.map(|id| id.0),
        pointer: id(pointer.as_ref().and_then(|pointer| pointer.current_focus())),
        // The interactive move/resize grab first,
        // else any other pointer grab. The engine's chrome (letterbox bar)
        // grabs are not separate yet, so `chrome` is never reported.
        pointer_grab: match comp.interactive {
            Some(grab) if grab.edges == 0 => "move",
            Some(_) => "resize",
            None if pointer.as_ref().is_some_and(|pointer| pointer.is_grabbed()) => "popup",
            None => "none",
        },
        session_lock: world::comp::session_lock::phase(lp).name(),
        // Null under a session lock.
        window: comp
            .registry
            .surface_rows()
            .filter(|_| !world::comp::session_lock::active(lp))
            .filter(|record| record.focused() && record.mapped() && record.role().managed_toplevel())
            .min_by_key(|record| record.id().0)
            .map_or_else(FocusWindowSnapshot::default, |record| FocusWindowSnapshot {
                id: Some(record.id().0),
                generation: Some(record.generation()),
            }),
    }
}

/// One compd scene surface as a `surfaces.s<id>` layer row: what the props
/// tree shows for the Quoin layer surface it stands for. No client, so no title,
/// app id, decoration or foreign id; hidden under a session lock as every
/// ordinary surface is.
fn project_scene_surface(lp: &Loop, row: &world::comp::scenes::SceneRow) -> SurfaceSnapshot {
    let input = &row.input;
    let focused = lp.inner.comp.scenes.focus == Some(row.id);
    SurfaceSnapshot {
        occlusion: OcclusionProps::default(),
        id: row.id.0,
        role: "layer",
        mapped: true,
        visible: !world::comp::session_lock::active(lp),
        x: input.x,
        y: input.y,
        width: input.width,
        height: input.height,
        band: input.stratum,
        sequence: 0,
        tree_index: 0,
        parent: None,
        output: Some(input.output.clone()),
        title: None,
        app_id: None,
        focused,
        activated: focused,
        maximized: false,
        fullscreen: false,
        minimized: false,
        workspace: None,
        decoration: None,
        layer: Some(LayerSnapshot {
            stratum: input.stratum,
            interactivity: input.interactivity,
            exclusive_zone: input.exclusive_zone,
            binding: "explicit",
        }),
        foreign_id: None,
        generation: row.generation,
        window: Default::default(),
    }
}

/// The mapped root surfaces, topmost first: overlay and top layers, the
/// windows in draw order, bottom and background layers.
fn project_stack(lp: &Loop, placements: &Placements) -> Vec<u64> {
    let registry = &lp.inner.comp.registry;
    let mapped_root = |id: SurfaceId| {
        registry
            .get(id)
            .is_some_and(|record| record.mapped() && record.parent().is_none())
    };
    let layers = |stratum: usize| {
        placements.layers[stratum]
            .iter()
            .rev()
            .filter_map(|handle| registry.id_for_handle(handle))
            .filter(|id| mapped_root(*id))
            .map(|id| id.0)
            .collect::<Vec<_>>()
    };
    let windows = lp
        .inner
        .drawable_order()
        .into_iter()
        .filter_map(|uuid| registry.id_for_uuid(uuid))
        .filter(|id| mapped_root(*id))
        .map(|id| id.0);
    // compd's scene surfaces over the Wayland layers of their stratum.
    let scenes = |stratum: &str| {
        lp.inner
            .comp
            .scenes
            .rows
            .iter()
            .filter(move |row| row.input.stratum == stratum)
            .map(|row| row.id.0)
            .collect::<Vec<_>>()
    };
    let mut stack = scenes("overlay");
    stack.extend(layers(3));
    stack.extend(scenes("top"));
    stack.extend(layers(2));
    stack.extend(windows);
    stack.extend(layers(1));
    stack.extend(layers(0));
    stack
}

pub(crate) fn wl_surface(lp: &Loop, handle: &SurfaceHandle) -> Option<WlSurface> {
    match handle {
        // from_id can reconstruct a retired ID without its surface userdata.
        // The registry can still contain that ID while destruction is drained.
        SurfaceHandle::Wl(id) => WlSurface::from_id(&lp.inner.loader.display_handle, id.clone())
            .ok()
            .filter(Resource::is_alive),
        SurfaceHandle::X11(_) => None,
    }
}

fn surface_size(surface: &WlSurface) -> Option<Size<i32, Logical>> {
    with_renderer_surface_state(surface, |state| state.surface_size()).flatten()
}

fn point(point: Point<i32, Logical>) -> (f32, f32) {
    (point.x as f32, point.y as f32)
}

fn size(size: Size<i32, Logical>) -> (f32, f32) {
    (size.w as f32, size.h as f32)
}

fn stratum_index(layer: Layer) -> usize {
    match layer {
        Layer::Background => 0,
        Layer::Bottom => 1,
        Layer::Top => 2,
        Layer::Overlay => 3,
    }
}

fn layer_name(layer: Layer) -> &'static str {
    match layer {
        Layer::Background => "background",
        Layer::Bottom => "bottom",
        Layer::Top => "top",
        Layer::Overlay => "overlay",
    }
}

fn interactivity_name(interactivity: KeyboardInteractivity) -> &'static str {
    match interactivity {
        KeyboardInteractivity::None => "none",
        KeyboardInteractivity::OnDemand => "on_demand",
        KeyboardInteractivity::Exclusive => "exclusive",
    }
}

fn exclusive_zone_value(zone: ExclusiveZone) -> i32 {
    match zone {
        ExclusiveZone::Exclusive(amount) => i32::try_from(amount).unwrap_or(i32::MAX),
        ExclusiveZone::Neutral => 0,
        ExclusiveZone::DontCare => -1,
    }
}

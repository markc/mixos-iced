//! The comp policy state the host owns: the all-role surface registry, fed by
//! `WireTrait::surface_event`.
//!
//! Map rule, per role (the policy's semantics on the engine's lifecycle):
//! - a toplevel or X11 window maps when it is PLACED (the frame hook's initial
//!   map, an X11 readmit, an X11 window tracked as a popup) and, once placed,
//!   follows its buffer: a null attach unmaps it, a buffer maps it again;
//! - every other role (popup, layer, subsurface, IME popup, drag icon) maps
//!   and unmaps with its buffer.
//!
//! A role object destroyed while its surface lives on goes dormant; the
//! surface's (or X window's) destruction removes the record.
//!
//! Workspaces: the model lives here, on ONE world.
//! A window that maps is stamped on the current workspace (rule 2); a
//! window that carries a workspace and is minimised or off the current one is
//! HIDDEN ([`CompState::hidden`]), which the draw reads through
//! `DrawWindow::visible`: no draw, no frame callbacks, no presentation, no
//! pointer hits.

pub mod band;
pub mod bindings;
pub mod causes;
pub mod corners;
pub mod fullscreen;
pub mod injection;
pub mod latch;
pub mod occlusion;
pub mod panels;
pub mod presentation;
pub mod region;
pub mod scenes;
pub mod session_lock;
pub mod usable;
pub mod visibility;
pub mod x11_place;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use dispatcher::wire::trait_::surface_event::{
    InteractiveOp, SurfaceEvent, SurfaceHandle, WindowRequest,
};
use policy::workspaces::{DefaultOutput, WorkspaceState};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, Rectangle, Size};
use surfaces::{Registry, SurfaceId, SurfaceRole};

/// The workspace a fresh `CompState` is on (the default: workspace 1 of
/// 4). Live code reads [`CompState::current_workspace`].
pub const CURRENT_WORKSPACE: u32 = 1;

/// Where a maximised window goes back to (its pre-maximise location and window
/// geometry size, in the host Space).
#[derive(Clone, Debug, PartialEq)]
pub struct MaximizeRestore {
    pub location: Point<i32, Logical>,
    pub size: Size<i32, Logical>,
    /// Output owning the requested maximised slot, independent of old buffers.
    pub output: String,
}

/// An interactive move/resize in progress: what
/// the grab reported, and the window geometry when it began (filled in by the
/// host on its first pass).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Interactive {
    pub id: SurfaceId,
    /// 0 = move; else the `xdg_toplevel.resize_edge` bits.
    pub edges: u32,
    pub delta: (f64, f64),
    pub start: Option<(Point<i32, Logical>, Size<i32, Logical>)>,
    /// A new delta the host has not applied yet.
    pub updated: bool,
    pub ended: bool,
    /// The last size a resize configured.
    pub last_size: Option<Size<i32, Logical>>,
}

/// What an output's generation is taken from: `(x, y, width, height)` in the
/// host Space, the mode's `(width, height)`, and the scale.
pub type OutputSignature = (i32, i32, i32, i32, i32, i32, f64);

/// One output's generation and the signature it was taken from (`None` once
/// the output is gone, so its return is a new generation).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct OutputGeneration {
    pub generation: u64,
    pub signature: Option<OutputSignature>,
}

#[derive(Default)]
pub struct CompState {
    pub registry: Registry<SurfaceHandle>,
    /// The workspace model (count, current per output).
    pub workspaces: WorkspaceState,
    /// The default output (the single-output rule), refreshed from the
    /// host Space before each Bus pass (`policy_host::refresh_default_output`).
    pub default_output: Option<DefaultOutput>,
    /// Commits with a buffer, per surface (one per drain that saw one): the
    /// comparator's frame-callback oracle (a frame-driven client stops
    /// committing when it gets no callbacks).
    commits: HashMap<SurfaceId, u64>,
    /// Minimised windows, oldest first: `comp.window.restore {}` pops the
    /// current workspace's most recent, else the global one (rule 8).
    minimize_lifo: Vec<SurfaceId>,
    /// Windows maximised by request, with
    /// where they go back to.
    maximized: HashMap<SurfaceId, MaximizeRestore>,
    /// Client window-state requests (xdg / X11), for the host's policy.
    requests: Vec<(SurfaceId, WindowRequest)>,
    /// `_NET_CURRENT_DESKTOP` root requests, 0-based.
    desktop_requests: Vec<u32>,
    /// The interactive move/resize in progress, if any.
    pub interactive: Option<Interactive>,
    /// Hot corners: the detector, the owned presses and the topics they
    /// earned.
    pub corners: corners::Corners,
    /// Bus-injected input: the `input_seq` mint, the injected holds per seat,
    /// the agent pointer.
    pub injection: injection::Injection,
    /// Quoin's panel holders.
    pub panels: panels::Panels,
    /// Each output's usable area (host Space, logical), by output name: what
    /// layer-shell exclusive zones leave.
    /// Refreshed every Bus pass; maximise and new-window placement use it.
    pub usable: BTreeMap<String, Rectangle<i32, Logical>>,
    /// What docked Mix Scenes panels reserve, by output name: taken out of
    /// the usable area after the layer zones. Set by the scene host's owner
    /// each pass; absent is nothing reserved.
    pub reserved: BTreeMap<String, usable::Reserved>,
    /// The Mix Scenes surfaces compd draws itself, as comp.props rows.
    pub scenes: scenes::SceneSurfaces,
    /// Each output's generation, by output name: bumped
    /// when its mode, size, scale or position changes, or when it comes back
    /// after removal. `{output, generation}` freshness rides on it (the region
    /// reply's `output_generation`).
    pub output_generations: BTreeMap<String, OutputGeneration>,
    /// The pointer was put at the first output's centre (`camera::pin::
    /// centre_pointer_once`), once.
    pub pointer_placed: bool,
    /// `comp.region.select`: the run holding the seat.
    pub region: region::Region,
    /// The key bindings on the human keyboard.
    pub bindings: bindings::Bindings,
    /// The session lock's policy state.
    pub lock: session_lock::LockPolicy,
    /// Presentation statistics.
    pub presentation: presentation::Presentation,
    /// The `props.changed` cause per source.
    pub causes: causes::Causes,
    /// The output each fullscreen was asked for.
    pub fullscreen: fullscreen::FullscreenOutputs,
    /// The exclusive-keyboard latch.
    pub latch: latch::Latch,
    /// The X11 cascade counter.
    pub x11_cascade: u32,
    /// Windows placed since their current role was taken.
    placed: HashSet<SurfaceId>,
    /// Layer surfaces that named their output (`layer.binding: explicit`).
    layer_explicit: HashSet<SurfaceHandle>,
    /// The record holding the primary seat's keyboard focus.
    focused: Option<SurfaceId>,
    /// Bumped by every event applied (a commit's buffer state included): the
    /// Bus edge pass runs only when it moved.
    revision: u64,
    /// Bumped only when registry content (a role, the mapped/focused bits,
    /// names, a uuid, a layer binding) actually changed: the `compd.truth`
    /// revision, which stays put while a client merely redraws.
    content: u64,
}

impl CompState {
    pub fn apply(&mut self, event: SurfaceEvent) {
        self.revision = self.revision.wrapping_add(1);
        match event {
            SurfaceEvent::RoleTaken {
                handle,
                role,
                parent,
            } => {
                let parent = parent.and_then(|parent| self.registry.id_for_handle(&parent));
                match self.registry.take_role(handle, role, parent) {
                    Ok((id, _)) => {
                        self.touched();
                        self.placed.remove(&id);
                        self.presentation.forget(id);
                        // A new role starts unminimised and unmaximised.
                        self.minimize_lifo.retain(|entry| *entry != id);
                        self.maximized.remove(&id);
                        if self.focused == Some(id) {
                            self.focused = None;
                        }
                    }
                    Err(error) => warn!("comp registry: role {role:?} refused: {error:?}"),
                }
            }
            SurfaceEvent::Dormant(handle) => {
                // The chrome's seat state (hover, armed button, double-click).
                decor::seat::forget_handle(&handle);
                if let Some(id) = self.registry.id_for_handle(&handle) {
                    self.forget(id);
                    match self.registry.go_dormant(id) {
                        Ok(Some(_)) => self.touched(),
                        Ok(None) => {}
                        Err(error) => warn!("comp registry: dormant {id:?}: {error:?}"),
                    }
                }
            }
            SurfaceEvent::Destroyed(handle) => {
                decor::seat::forget_handle(&handle);
                if let Some(id) = self.registry.id_for_handle(&handle) {
                    self.forget(id);
                    self.commits.remove(&id);
                    self.registry.destroy(id);
                    self.touched();
                }
                self.layer_explicit.remove(&handle);
            }
            SurfaceEvent::Buffer { handle, attached } => {
                let Some(id) = self.registry.id_for_handle(&handle) else {
                    return;
                };
                if attached {
                    let seq = {
                        let count = self.commits.entry(id).or_default();
                        *count += 1;
                        *count
                    };
                    let window = self.window_of(id);
                    self.presentation.published(id, window, seq);
                }
                let Some(role) = self.registry.get(id).map(|record| record.role()) else {
                    return;
                };
                let mapped = match role {
                    SurfaceRole::Dormant => return,
                    SurfaceRole::Toplevel | SurfaceRole::X11 { .. } => {
                        if attached && !self.placed.contains(&id) {
                            return;
                        }
                        attached
                    }
                    _ => attached,
                };
                if self.set_mapped(id, mapped) {
                    self.touched();
                    self.causes.note_window(
                        id.0,
                        if mapped {
                            "wayland.map"
                        } else {
                            "wayland.unmap"
                        },
                    );
                }
            }
            SurfaceEvent::Placed(handle) => {
                if let Some(id) = self.registry.id_for_handle(&handle) {
                    let newly = self.placed.insert(id);
                    if self.set_mapped(id, true) || newly {
                        self.touched();
                        self.causes.note_window(id.0, "wayland.map");
                    }
                }
            }
            SurfaceEvent::Names {
                handle,
                title,
                app_id,
            } => {
                if let Some(id) = self.registry.id_for_handle(&handle) {
                    let title: Option<Arc<str>> = title.map(Arc::from);
                    let app_id: Option<Arc<str>> = app_id.map(Arc::from);
                    let same = self.registry.get(id).is_some_and(|record| {
                        record.title() == title.as_ref() && record.app_id() == app_id.as_ref()
                    });
                    if !same {
                        let _ = self.registry.set_title(id, title);
                        let _ = self.registry.set_app_id(id, app_id);
                        self.touched();
                        self.causes.note_window(id.0, "wayland.map");
                    }
                }
            }
            SurfaceEvent::Focus(handle) => {
                let next = handle.and_then(|handle| self.registry.id_for_handle(&handle));
                if next == self.focused {
                    return;
                }
                self.touched();
                self.causes.note("focus", "wayland.focus");
                for id in [next, self.focused].into_iter().flatten() {
                    self.causes.note_window(id.0, "wayland.focus");
                }
                if !self.panels.is_empty() {
                    let change = (next.map(|id| id.0), self.focused.map(|id| id.0));
                    self.panels.focus_changes.push(change);
                    self.panels.last_focus_change = Some(change);
                }
                if let Some(previous) = self.focused.take() {
                    let _ = self.registry.set_focused(previous, false);
                }
                if let Some(next) = next
                    && self.registry.set_focused(next, true).is_ok()
                {
                    self.focused = Some(next);
                }
            }
            SurfaceEvent::Request { handle, request } => {
                if let Some(id) = self.registry.id_for_handle(&handle) {
                    self.requests.push((id, request));
                }
            }
            SurfaceEvent::CurrentDesktop(desktop) => self.desktop_requests.push(desktop),
            SurfaceEvent::TransientFor { handle, owner } => {
                // Not projected (the surfaces rows carry no owner), so no
                // content bump: only the next move reads it.
                if let Some(id) = self.registry.id_for_handle(&handle) {
                    let _ = self.registry.set_transient_for(id, owner);
                }
            }
            SurfaceEvent::LayerAck { handle, serial } => {
                if !self.panels.is_empty()
                    && let Some(id) = self.registry.id_for_handle(&handle)
                {
                    self.panels.layer_acks.push((id, serial));
                }
            }
            SurfaceEvent::Interactive { handle, op } => {
                let Some(id) = self.registry.id_for_handle(&handle) else {
                    return;
                };
                match op {
                    InteractiveOp::Begin { edges } => {
                        self.interactive = Some(Interactive {
                            id,
                            edges,
                            delta: (0.0, 0.0),
                            start: None,
                            updated: false,
                            ended: false,
                            last_size: None,
                        });
                    }
                    InteractiveOp::Update { dx, dy } => {
                        if let Some(grab) = self.interactive.as_mut().filter(|grab| grab.id == id) {
                            grab.delta = (dx, dy);
                            grab.updated = true;
                        }
                    }
                    InteractiveOp::End => {
                        if let Some(grab) = self.interactive.as_mut().filter(|grab| grab.id == id) {
                            grab.ended = true;
                        }
                    }
                }
            }
            SurfaceEvent::LayerBinding { handle, explicit } => {
                let changed = if explicit {
                    self.layer_explicit.insert(handle)
                } else {
                    self.layer_explicit.remove(&handle)
                };
                if changed {
                    self.touched();
                }
            }
        }
    }

    fn touched(&mut self) {
        self.content = self.content.wrapping_add(1);
    }

    fn forget(&mut self, id: SurfaceId) {
        self.placed.remove(&id);
        self.fullscreen.forget(id);
        self.presentation.forget(id);
        if self.interactive.is_some_and(|grab| grab.id == id) {
            self.interactive = None;
        }
        self.minimize_lifo.retain(|entry| *entry != id);
        self.maximized.remove(&id);
        if self.focused == Some(id) {
            self.focused = None;
        }
    }

    /// The window `id` belongs to (its root, walked up; a managed toplevel),
    /// as `{id, generation}`: what presentation stats are kept for.
    fn window_of(&self, mut id: SurfaceId) -> Option<(u64, u64)> {
        for _ in 0..64 {
            match self.registry.get(id).and_then(|record| record.parent()) {
                Some(parent) => id = parent,
                None => break,
            }
        }
        let record = self.registry.get(id)?;
        record
            .role()
            .managed_toplevel()
            .then(|| (record.id().0, record.generation()))
    }

    /// The mapped flag, stamping the workspace on the false→true edge.
    /// Returns whether it changed.
    fn set_mapped(&mut self, id: SurfaceId, mapped: bool) -> bool {
        let was_mapped = self.registry.get(id).is_some_and(|record| record.mapped());
        let changed = self.registry.set_mapped(id, mapped).unwrap_or(false);
        if mapped && !was_mapped {
            self.presentation.mapped(id);
        }
        let current = self.current_workspace();
        policy::workspaces::stamp_workspace_at_map(&mut self.registry, id, was_mapped, current);
        changed
    }

    /// Bind the window's uuid (and the client's pid) to the window's record
    /// (`initialize_surface_data`). A changed uuid mints a new generation
    /// inside the registry.
    pub fn bind_uuid(&mut self, handle: &SurfaceHandle, uuid: uuid::Uuid, pid: Option<u32>) {
        self.revision = self.revision.wrapping_add(1);
        self.touched();
        let Some(id) = self.registry.id_for_handle(handle) else {
            warn!("comp registry: uuid {uuid} for a surface with no role");
            return;
        };
        if let Err(error) = self.registry.bind_uuid(id, uuid) {
            warn!("comp registry: uuid {uuid} refused for {id:?}: {error:?}");
        }
        let _ = self.registry.set_pid(id, pid.map(u64::from));
    }

    /// A withdrawn X11 window maps again under the uuid it left with: a new
    /// role take (the generation bumps on every role take), the same
    /// uuid, mapped at once (readmit puts it straight back in the Space).
    pub fn readmit(
        &mut self,
        handle: SurfaceHandle,
        override_redirect: bool,
        uuid: uuid::Uuid,
        pid: Option<u32>,
    ) {
        self.apply(SurfaceEvent::RoleTaken {
            handle: handle.clone(),
            role: SurfaceRole::X11 { override_redirect },
            parent: None,
        });
        self.bind_uuid(&handle, uuid, pid);
        self.apply(SurfaceEvent::Placed(handle));
    }

    /// The record a `wl_surface` belongs to (an Xwayland-backed one resolves to
    /// its X window's record).
    pub fn id_for_surface(&self, surface: &WlSurface) -> Option<SurfaceId> {
        self.registry
            .id_for_handle(&SurfaceHandle::resolve(surface))
    }

    /// Moves whenever the registry may have changed.
    pub fn revision(&self) -> u64 {
        self.revision.wrapping_add(self.corners.changes())
    }

    /// Moves only when registry content (or the corner configuration)
    /// changed (`compd.truth`'s revision).
    pub fn content_revision(&self) -> u64 {
        self.content.wrapping_add(self.corners.changes())
    }

    /// The record holding the primary seat's keyboard focus, as the focus
    /// events left it.
    pub fn focused(&self) -> Option<SurfaceId> {
        self.focused
    }

    /// The default output's current workspace.
    pub fn current_workspace(&self) -> u32 {
        self.workspaces.current(self.default_output.as_ref())
    }

    /// Whether a record is hidden: it carries a workspace and is minimised or
    /// off the current one (policy `presentable`, less the mapped term).
    pub fn hidden_id(&self, id: SurfaceId) -> bool {
        let current = self.current_workspace();
        self.registry.get(id).is_some_and(|record| {
            record.role().carries_workspace()
                && (record.minimized() || !policy::workspaces::on_workspace(record, current))
        })
    }

    /// [`Self::hidden_id`] by handle; a surface with no record is not hidden.
    pub fn hidden(&self, handle: &SurfaceHandle) -> bool {
        self.registry
            .id_for_handle(handle)
            .is_some_and(|id| self.hidden_id(id))
    }

    /// Commits with a buffer this record has made.
    pub fn commits(&self, id: SurfaceId) -> u64 {
        self.commits.get(&id).copied().unwrap_or(0)
    }

    /// A workspace change (switch, move, count) moved what is shown: both the
    /// edge pass (activity) and the truth revision (content) must see it.
    pub fn workspaces_changed(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        self.touched();
        self.causes.note("", "workspace.switch");
    }

    /// A non-volatile `input.*` leaf changed (the host passthrough): the edge
    /// pass and the truth revision must see it.
    pub fn input_changed(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        self.touched();
        self.causes.note("input", "props.set");
    }

    /// A configuration leaf outside `input.*` changed under `prefix` (the
    /// xwayland switch, a band, the lock phase, the binding toggle): the edge
    /// pass and the truth revision must see it, with `cause`.
    pub fn settings_changed(&mut self, prefix: &str, cause: &'static str) {
        self.revision = self.revision.wrapping_add(1);
        self.touched();
        self.causes.note(prefix, cause);
    }

    /// The renderer's occlusion decision changed for windows `ids`: the
    /// edge pass must diff their rows, with the `wayland.occlusion` cause.
    /// Not content (`compd.truth` carries no occlusion).
    pub fn occlusion_changed(&mut self, ids: &[u64]) {
        if ids.is_empty() {
            return;
        }
        self.revision = self.revision.wrapping_add(1);
        for id in ids {
            self.causes.note_window(*id, "wayland.occlusion");
        }
    }

    /// The `_NET_CURRENT_DESKTOP` requests queued since the last call.
    pub fn take_desktop_requests(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.desktop_requests)
    }

    /// The client window-state requests queued since the last call.
    pub fn take_requests(&mut self) -> Vec<(SurfaceId, WindowRequest)> {
        std::mem::take(&mut self.requests)
    }

    /// Keep the minimise LIFO in step with a minimised flag the policy just
    /// changed (the registry already holds the new value).
    pub fn note_minimized(&mut self, id: SurfaceId, minimized: bool) {
        self.minimize_lifo.retain(|entry| *entry != id);
        if minimized {
            self.minimize_lifo.push(id);
        }
        self.revision = self.revision.wrapping_add(1);
        self.touched();
        self.causes.note_window(id.0, "comp.window");
    }

    /// Rule 8: the current workspace's most recently minimised window,
    /// else the most recent anywhere.
    pub fn lifo_restore_candidate(&self) -> Option<SurfaceId> {
        let current = self.current_workspace();
        let live = |id: &&SurfaceId| {
            self.registry.get(**id).is_some_and(|record| {
                record.mapped() && record.minimized() && record.role().managed_toplevel()
            })
        };
        self.minimize_lifo
            .iter()
            .rev()
            .filter(live)
            .find(|id| {
                self.registry
                    .get(**id)
                    .is_some_and(|record| record.workspace() == Some(current))
            })
            .or_else(|| self.minimize_lifo.iter().rev().find(live))
            .copied()
    }

    /// The restore geometry of a window maximised by request (`None`: not
    /// maximised).
    /// The windows maximised by request (they follow the usable area).
    pub fn maximized_ids(&self) -> Vec<SurfaceId> {
        let mut ids: Vec<SurfaceId> = self.maximized.keys().copied().collect();
        ids.sort_unstable_by_key(|id| id.0);
        ids
    }

    /// An output's usable area moved: the edge pass (`outputs.*.usable`) and
    /// the truth revision must see it.
    /// This pass's scene surfaces and the scene holding the keyboard (its
    /// host key), from the scene host's owner. Each scene keeps one reserved
    /// id; a scene that was not mapped last pass takes a fresh generation. A
    /// change moves the revision, so the edge pass diffs it.
    pub fn set_scene_surfaces(&mut self, inputs: Vec<scenes::SceneInput>, focus: Option<&str>) {
        let mut rows = Vec::with_capacity(inputs.len());
        for input in inputs {
            let id = match self.scenes.ids.get(&input.key) {
                Some(id) => *id,
                None => {
                    let id = self.registry.reserve_id();
                    self.scenes.ids.insert(input.key.clone(), id);
                    id
                }
            };
            let generation = match self.scenes.rows.iter().find(|row| row.id == id) {
                Some(row) => row.generation,
                None => self.registry.reserve_generation(),
            };
            rows.push(scenes::SceneRow {
                id,
                generation,
                input,
            });
        }
        // Only a mapped scene can hold the keyboard.
        let focus = focus
            .and_then(|key| self.scenes.id_of(key))
            .filter(|id| rows.iter().any(|row| row.id == *id));
        if rows != self.scenes.rows || focus != self.scenes.focus {
            self.scenes.rows = rows;
            self.scenes.focus = focus;
            self.revision = self.revision.wrapping_add(1);
            self.touched();
            // The same change as Quoin mapping a layer surface.
            self.causes.note("surfaces", "wayland.map");
        }
    }

    pub fn outputs_changed(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        self.touched();
        self.causes.note("outputs", "layer.arrange");
    }

    /// An output's current generation (0: never seen).
    pub fn output_generation(&self, name: &str) -> u64 {
        self.output_generations
            .get(name)
            .map_or(0, |entry| entry.generation)
    }

    /// Fold the outputs present now into the generations: a new or changed
    /// output, or one back after removal, moves to its next generation; a
    /// gone one keeps its number for its return. Returns whether anything
    /// moved (the truth revision then moves too).
    pub fn observe_outputs(&mut self, present: &BTreeMap<String, OutputSignature>) -> bool {
        let mut changed = false;
        for (name, entry) in &mut self.output_generations {
            if entry.signature.is_some() && !present.contains_key(name) {
                entry.signature = None;
                changed = true;
            }
        }
        for (name, signature) in present {
            let entry = self.output_generations.entry(name.clone()).or_default();
            if entry.signature != Some(*signature) {
                entry.generation = entry.generation.saturating_add(1);
                entry.signature = Some(*signature);
                changed = true;
            }
        }
        if changed {
            self.touched();
            self.causes.note("outputs", "output.geometry");
        }
        changed
    }

    /// The default output's usable area, when known.
    pub fn default_usable(&self) -> Option<Rectangle<i32, Logical>> {
        let name = &self.default_output.as_ref()?.name;
        self.usable.get(name).copied()
    }

    pub fn maximize_restore(&self, id: SurfaceId) -> Option<MaximizeRestore> {
        self.maximized.get(&id).cloned()
    }

    /// Record (or clear) a window's requested-maximised state.
    pub fn set_maximize_restore(&mut self, id: SurfaceId, restore: Option<MaximizeRestore>) {
        let changed = match restore {
            Some(restore) => self.maximized.insert(id, restore).is_none(),
            None => self.maximized.remove(&id).is_some(),
        };
        if changed {
            self.revision = self.revision.wrapping_add(1);
            self.touched();
        }
    }

    /// Whether a layer surface named its output.
    pub fn layer_explicit(&self, handle: &SurfaceHandle) -> bool {
        self.layer_explicit.contains(handle)
    }
}

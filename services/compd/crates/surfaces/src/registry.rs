// The SurfaceRecord identity subset, next_surface_id / next_role_generation,
// role_change_generation, toplevel_root_for_surface, WindowTargetError and
// resolve_window_target, the windows.* / focus.window / workspaces.list
// projection rules, and the id<->uuid adapter.

use std::{
    collections::{BTreeMap, HashMap},
    fmt,
    hash::Hash,
    sync::Arc,
};

use uuid::Uuid;

use crate::{StackBand, SurfaceId, SurfaceRole};

/// A surface's identity, role and window state. Render state (layout,
/// buffers, content sequence) stays with the engine.
///
/// Fields are read through accessors and written through [`Registry`], so
/// the id, handle and uuid indices can never disagree with a record.
#[derive(Clone, Debug, PartialEq)]
pub struct SurfaceRecord<H> {
    id: SurfaceId,
    role: SurfaceRole,
    /// Bumped every time this surface takes a role (including going
    /// dormant), and when the engine replaces a window's uuid. The id survives a
    /// role re-take; the generation does not, so a Bus caller holding
    /// `{id, generation}` cannot act on a different window that inherited
    /// the id.
    generation: u64,
    mapped: bool,
    minimized: bool,
    focused: bool,
    /// The 1-based workspace stamped at the map edge; `None` until the
    /// first stamp. A move never bumps `generation`: a moved window is the
    /// same window.
    workspace: Option<u32>,
    band: StackBand,
    app_id: Option<Arc<str>>,
    title: Option<Arc<str>>,
    pid: Option<u64>,
    /// The parent surface: a subsurface's parent, a popup's parent.
    parent: Option<SurfaceId>,
    /// An X11 window's `WM_TRANSIENT_FOR` owner, as the owner's handle (X
    /// window ids are compared live, so an owner registered later still
    /// matches). Kept apart from `parent`: an X11 record gets no parent, and
    /// the stack, focus and stats walkers follow `parent`.
    transient_for: Option<H>,
    /// The engine's uuid v7 (toplevels and X11 windows only).
    uuid: Option<Uuid>,
    handle: H,
}

impl<H> SurfaceRecord<H> {
    pub fn id(&self) -> SurfaceId {
        self.id
    }
    pub fn role(&self) -> SurfaceRole {
        self.role
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn mapped(&self) -> bool {
        self.mapped
    }
    pub fn minimized(&self) -> bool {
        self.minimized
    }
    pub fn focused(&self) -> bool {
        self.focused
    }
    /// The stored workspace stamp (`None` before the first map).
    pub fn workspace(&self) -> Option<u32> {
        self.workspace
    }
    pub fn band(&self) -> StackBand {
        self.band
    }
    pub fn app_id(&self) -> Option<&Arc<str>> {
        self.app_id.as_ref()
    }
    pub fn title(&self) -> Option<&Arc<str>> {
        self.title.as_ref()
    }
    pub fn pid(&self) -> Option<u64> {
        self.pid
    }
    pub fn parent(&self) -> Option<SurfaceId> {
        self.parent
    }
    pub fn transient_for(&self) -> Option<&H> {
        self.transient_for.as_ref()
    }
    pub fn uuid(&self) -> Option<Uuid> {
        self.uuid
    }
    pub fn handle(&self) -> &H {
        &self.handle
    }

    /// The `surfaces.s<id>.workspace` leaf: the stamp of a mapped managed
    /// toplevel (X11 included), null for every other role and before the
    /// first map, and null again once unmapped (a withdrawn window is on no
    /// workspace even though the record keeps its last stamp).
    pub fn workspace_leaf(&self) -> Option<u32> {
        self.workspace
            .filter(|workspace| self.mapped && self.role.managed_toplevel() && *workspace >= 1)
    }

    /// Whether this record is a `windows.*` row: a mapped xdg toplevel.
    /// X11 toplevels are not (their workspace is read from `surfaces.*`).
    pub fn is_window_row(&self) -> bool {
        self.role == SurfaceRole::Toplevel && self.mapped
    }
}

/// Why a window-addressed request did not resolve to a window. These are
/// correctness refusals (the request is aimed at the wrong thing), never
/// caller checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowTargetError {
    /// No live, role-bearing surface has this id.
    UnknownWindow,
    /// The id is live but now names a different role assignment.
    StaleTarget { requested: u64, current: u64 },
    /// The surface exists but is not a managed toplevel (popup, layer,
    /// override-redirect X11, ...).
    NotManaged,
    /// A managed toplevel with no mapped content.
    NotMapped,
}

impl WindowTargetError {
    /// The `error` code on the `comp.*` wire.
    pub fn code(self) -> &'static str {
        match self {
            Self::UnknownWindow => "unknown_window",
            Self::StaleTarget { .. } => "stale_target",
            Self::NotManaged => "not_managed",
            Self::NotMapped => "not_mapped",
        }
    }
}

/// A registry operation the engine asked for that cannot hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegistryError {
    /// No record has this id.
    UnknownSurface(SurfaceId),
    /// `Dormant` is not a role a surface takes; it is what losing one leaves.
    DormantIsNotARole,
    /// The engine stamps uuids on toplevels and X11 windows only.
    RoleCarriesNoUuid { id: SurfaceId, role: SurfaceRole },
    /// The uuid already names another live record.
    UuidInUse { uuid: Uuid, owner: SurfaceId },
    /// Workspaces are 1-based.
    InvalidWorkspace(u32),
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownSurface(id) => write!(f, "no surface record has id {}", id.0),
            Self::DormantIsNotARole => write!(f, "dormant is not a role a surface can take"),
            Self::RoleCarriesNoUuid { id, role } => write!(
                f,
                "surface {} has role {}, which carries no uuid",
                id.0,
                role.kind()
            ),
            Self::UuidInUse { uuid, owner } => {
                write!(f, "uuid {uuid} already names surface {}", owner.0)
            }
            Self::InvalidWorkspace(index) => write!(f, "workspace {index} is not 1-based"),
        }
    }
}

impl std::error::Error for RegistryError {}

/// What [`Registry::bind_uuid`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindOutcome {
    /// The record had no uuid; it has this one now. The generation is the
    /// role take's.
    Bound,
    /// The record already carried this uuid.
    Unchanged,
    /// The engine recreated the window without recreating the surface: the uuid
    /// changed, so a new role generation was minted (a stale
    /// `{id, generation}` must not reach the successor).
    Replaced { previous: Uuid, generation: u64 },
}

/// All-role surface registry: one record per role-bearing `wl_surface`.
#[derive(Clone, Debug)]
pub struct Registry<H> {
    records: BTreeMap<SurfaceId, SurfaceRecord<H>>,
    by_handle: HashMap<H, SurfaceId>,
    by_uuid: HashMap<Uuid, SurfaceId>,
    next_surface_id: u64,
    next_role_generation: u64,
}

impl<H: Clone + Eq + Hash> Default for Registry<H> {
    fn default() -> Self {
        Self::new()
    }
}

impl<H: Clone + Eq + Hash> Registry<H> {
    pub fn new() -> Self {
        Self {
            records: BTreeMap::new(),
            by_handle: HashMap::new(),
            by_uuid: HashMap::new(),
            next_surface_id: 1,
            next_role_generation: 1,
        }
    }

    /// Hands out the next role generation (one global counter, so a
    /// generation never repeats across surfaces either).
    fn next_role_generation(&mut self) -> u64 {
        let generation = self.next_role_generation;
        self.next_role_generation = generation.saturating_add(1);
        generation
    }

    fn unbind_uuid(&mut self, id: SurfaceId) {
        if let Some(record) = self.records.get_mut(&id)
            && let Some(uuid) = record.uuid.take()
            && self.by_uuid.get(&uuid) == Some(&id)
        {
            self.by_uuid.remove(&uuid);
        }
    }

    /// `handle` takes `role`. A surface seen before (dormant, or switching
    /// role) keeps its id; a new one gets the next id. Either way the role
    /// takes a fresh generation and starts unmapped, unfocused, not
    /// minimised, with no title, app id, pid or uuid (the engine re-binds a
    /// uuid that survives, e.g. an X11 re-association, with
    /// [`bind_uuid`](Self::bind_uuid), which then does not bump again).
    /// The workspace stamp is kept: the next map restamps it.
    pub fn take_role(
        &mut self,
        handle: H,
        role: SurfaceRole,
        parent: Option<SurfaceId>,
    ) -> Result<(SurfaceId, u64), RegistryError> {
        if role == SurfaceRole::Dormant {
            return Err(RegistryError::DormantIsNotARole);
        }
        let generation = self.next_role_generation();
        if let Some(id) = self.by_handle.get(&handle).copied() {
            self.unbind_uuid(id);
            let record = self
                .records
                .get_mut(&id)
                .ok_or(RegistryError::UnknownSurface(id))?;
            record.role = role;
            record.generation = generation;
            record.mapped = false;
            record.minimized = false;
            record.focused = false;
            record.band = StackBand::default();
            record.app_id = None;
            record.title = None;
            record.pid = None;
            record.parent = parent;
            record.transient_for = None;
            return Ok((id, generation));
        }
        let id = SurfaceId(self.next_surface_id);
        self.next_surface_id = self.next_surface_id.saturating_add(1);
        self.by_handle.insert(handle.clone(), id);
        self.records.insert(
            id,
            SurfaceRecord {
                id,
                role,
                generation,
                mapped: false,
                minimized: false,
                focused: false,
                workspace: None,
                band: StackBand::default(),
                app_id: None,
                title: None,
                pid: None,
                parent,
                transient_for: None,
                uuid: None,
                handle,
            },
        );
        Ok((id, generation))
    }

    /// A fresh id from the surface counter for a surface the compositor
    /// draws itself and that has no handle or record (a Mix Scenes surface).
    /// It counts as [`issued`](Self::issued), and no
    /// handle is ever given it.
    pub fn reserve_id(&mut self) -> SurfaceId {
        let id = SurfaceId(self.next_surface_id);
        self.next_surface_id = self.next_surface_id.saturating_add(1);
        id
    }

    /// A fresh role generation from the one global counter, for the
    /// surfaces [`reserve_id`](Self::reserve_id) serves (one per map).
    pub fn reserve_generation(&mut self) -> u64 {
        self.next_role_generation()
    }

    /// The surface lost its role while the `wl_surface` lives on. Returns
    /// the dormant generation, or `None` when it was already dormant
    /// (nothing changes then).
    pub fn go_dormant(&mut self, id: SurfaceId) -> Result<Option<u64>, RegistryError> {
        let role = self
            .records
            .get(&id)
            .ok_or(RegistryError::UnknownSurface(id))?
            .role;
        if role == SurfaceRole::Dormant {
            return Ok(None);
        }
        let generation = self.next_role_generation();
        self.unbind_uuid(id);
        let record = self
            .records
            .get_mut(&id)
            .ok_or(RegistryError::UnknownSurface(id))?;
        record.role = SurfaceRole::Dormant;
        record.generation = generation;
        record.mapped = false;
        record.minimized = false;
        record.focused = false;
        Ok(Some(generation))
    }

    /// The `wl_surface` is destroyed: its record and indices go. The id is
    /// never handed out again. The returned record keeps its last uuid, so
    /// the caller can still name the window it was (a final topic, a log);
    /// the registry no longer resolves that uuid.
    pub fn destroy(&mut self, id: SurfaceId) -> Option<SurfaceRecord<H>> {
        let record = self.records.remove(&id)?;
        if let Some(uuid) = record.uuid
            && self.by_uuid.get(&uuid) == Some(&id)
        {
            self.by_uuid.remove(&uuid);
        }
        if self.by_handle.get(&record.handle) == Some(&id) {
            self.by_handle.remove(&record.handle);
        }
        Some(record)
    }

    /// Bind the engine's uuid to a uuid-carrying record (see [`BindOutcome`]).
    pub fn bind_uuid(&mut self, id: SurfaceId, uuid: Uuid) -> Result<BindOutcome, RegistryError> {
        let record = self
            .records
            .get(&id)
            .ok_or(RegistryError::UnknownSurface(id))?;
        if !record.role.carries_uuid() {
            return Err(RegistryError::RoleCarriesNoUuid {
                id,
                role: record.role,
            });
        }
        if let Some(owner) = self.by_uuid.get(&uuid).copied()
            && owner != id
        {
            return Err(RegistryError::UuidInUse { uuid, owner });
        }
        let previous = record.uuid;
        match previous {
            Some(current) if current == uuid => Ok(BindOutcome::Unchanged),
            None => {
                self.by_uuid.insert(uuid, id);
                if let Some(record) = self.records.get_mut(&id) {
                    record.uuid = Some(uuid);
                }
                Ok(BindOutcome::Bound)
            }
            Some(previous) => {
                let generation = self.next_role_generation();
                self.by_uuid.remove(&previous);
                self.by_uuid.insert(uuid, id);
                if let Some(record) = self.records.get_mut(&id) {
                    record.uuid = Some(uuid);
                    record.generation = generation;
                }
                Ok(BindOutcome::Replaced {
                    previous,
                    generation,
                })
            }
        }
    }

    /// uuid -> id (the adapter's secondary index).
    pub fn id_for_uuid(&self, uuid: Uuid) -> Option<SurfaceId> {
        self.by_uuid.get(&uuid).copied()
    }

    /// id -> uuid, for the uuid-carrying roles.
    pub fn uuid_for(&self, id: SurfaceId) -> Option<Uuid> {
        self.records.get(&id).and_then(|record| record.uuid)
    }

    /// Whether `id` was ever handed out (live, dormant or destroyed). A wait
    /// on an id never issued would otherwise read as `gone` at once.
    pub fn issued(&self, id: u64) -> bool {
        id != 0 && id < self.next_surface_id
    }

    pub fn id_for_handle(&self, handle: &H) -> Option<SurfaceId> {
        self.by_handle.get(handle).copied()
    }

    pub fn get(&self, id: SurfaceId) -> Option<&SurfaceRecord<H>> {
        self.records.get(&id)
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    fn record_mut(&mut self, id: SurfaceId) -> Result<&mut SurfaceRecord<H>, RegistryError> {
        self.records
            .get_mut(&id)
            .ok_or(RegistryError::UnknownSurface(id))
    }

    /// Returns whether the map state changed. A map or an unmap never bumps
    /// the generation: only a role (re)take or a uuid replacement does.
    pub fn set_mapped(&mut self, id: SurfaceId, mapped: bool) -> Result<bool, RegistryError> {
        let record = self.record_mut(id)?;
        let changed = record.mapped != mapped;
        record.mapped = mapped;
        Ok(changed)
    }

    pub fn set_minimized(&mut self, id: SurfaceId, minimized: bool) -> Result<bool, RegistryError> {
        let record = self.record_mut(id)?;
        let changed = record.minimized != minimized;
        record.minimized = minimized;
        Ok(changed)
    }

    pub fn set_focused(&mut self, id: SurfaceId, focused: bool) -> Result<bool, RegistryError> {
        let record = self.record_mut(id)?;
        let changed = record.focused != focused;
        record.focused = focused;
        Ok(changed)
    }

    /// Stamp the 1-based workspace (the map edge, a move, a send).
    pub fn set_workspace(&mut self, id: SurfaceId, workspace: u32) -> Result<(), RegistryError> {
        if workspace == 0 {
            return Err(RegistryError::InvalidWorkspace(workspace));
        }
        self.record_mut(id)?.workspace = Some(workspace);
        Ok(())
    }

    pub fn set_band(&mut self, id: SurfaceId, band: StackBand) -> Result<(), RegistryError> {
        self.record_mut(id)?.band = band;
        Ok(())
    }

    pub fn set_title(&mut self, id: SurfaceId, title: Option<Arc<str>>) -> Result<(), RegistryError> {
        self.record_mut(id)?.title = title;
        Ok(())
    }

    pub fn set_app_id(&mut self, id: SurfaceId, app_id: Option<Arc<str>>) -> Result<(), RegistryError> {
        self.record_mut(id)?.app_id = app_id;
        Ok(())
    }

    pub fn set_pid(&mut self, id: SurfaceId, pid: Option<u64>) -> Result<(), RegistryError> {
        self.record_mut(id)?.pid = pid;
        Ok(())
    }

    pub fn set_parent(&mut self, id: SurfaceId, parent: Option<SurfaceId>) -> Result<(), RegistryError> {
        self.record_mut(id)?.parent = parent;
        Ok(())
    }

    /// An X11 window's `WM_TRANSIENT_FOR` owner (`None`: it names none).
    /// Returns whether it changed. A role take clears it; the engine sets
    /// it after the take and again on every property change.
    pub fn set_transient_for(&mut self, id: SurfaceId, owner: Option<H>) -> Result<bool, RegistryError> {
        let record = self.record_mut(id)?;
        let changed = record.transient_for != owner;
        record.transient_for = owner;
        Ok(changed)
    }

    /// The one resolver for every window-addressed request. `generation`
    /// is optional so id-only callers keep working; when given it must
    /// match the surface's current role generation. The refusal order is
    /// part of the wire: unknown, stale, dormant (unknown), not managed, not
    /// mapped.
    pub fn resolve_window_target(
        &self,
        id: u64,
        generation: Option<u64>,
    ) -> Result<&SurfaceRecord<H>, WindowTargetError> {
        let record = self
            .records
            .get(&SurfaceId(id))
            .ok_or(WindowTargetError::UnknownWindow)?;
        if let Some(requested) = generation
            && requested != record.generation
        {
            return Err(WindowTargetError::StaleTarget {
                requested,
                current: record.generation,
            });
        }
        if record.role == SurfaceRole::Dormant {
            return Err(WindowTargetError::UnknownWindow);
        }
        if !record.role.managed_toplevel() {
            return Err(WindowTargetError::NotManaged);
        }
        if !record.mapped {
            return Err(WindowTargetError::NotMapped);
        }
        Ok(record)
    }

    /// Resolve a window by its engine uuid, then fence it like a numeric target.
    pub fn resolve_uuid_target(
        &self,
        uuid: Uuid,
        generation: Option<u64>,
    ) -> Result<&SurfaceRecord<H>, WindowTargetError> {
        let id = self
            .id_for_uuid(uuid)
            .ok_or(WindowTargetError::UnknownWindow)?;
        self.resolve_window_target(id.0, generation)
    }

    /// Projection: every role-bearing record (`surfaces.*`), in id order.
    /// Dormant records are not rows.
    pub fn surface_rows(&self) -> impl Iterator<Item = &SurfaceRecord<H>> {
        self.records
            .values()
            .filter(|record| record.role != SurfaceRole::Dormant)
    }

    /// Projection: the `windows.*` rows (mapped xdg toplevels), in id
    /// order. A session lock empties this map; that is the caller's call.
    pub fn windows(&self) -> impl Iterator<Item = &SurfaceRecord<H>> {
        self.surface_rows().filter(|record| record.is_window_row())
    }

    /// Projection: `focus.window` as `(id, generation)`: the lowest-id
    /// focused, mapped, managed toplevel, or none.
    pub fn focus_window(&self) -> Option<(u64, u64)> {
        self.records
            .values()
            .filter(|record| record.focused && record.mapped && record.role.managed_toplevel())
            .min_by_key(|record| record.id.0)
            .map(|record| (record.id.0, record.generation))
    }

    /// Projection: `workspaces.list[*].windows` for workspaces `1..=count`:
    /// every row whose workspace leaf is non-null counts (X11 included).
    pub fn workspace_window_counts(&self, count: u32) -> Vec<u32> {
        let mut counts = vec![0; count as usize];
        for workspace in self.surface_rows().filter_map(SurfaceRecord::workspace_leaf) {
            if let Some(slot) = workspace
                .checked_sub(1)
                .and_then(|index| counts.get_mut(index as usize))
            {
                *slot += 1;
            }
        }
        counts
    }

    /// `(id, generation)` of the window `id` belongs to for presentation
    /// statistics: itself for a toplevel or X11 window, its root for a
    /// subsurface, none for popups, layers, locks, drag icons and dormant
    /// surfaces.
    pub fn stats_window(&self, id: SurfaceId) -> Option<(u64, u64)> {
        let mut current = self.records.get(&id)?;
        // A parent chain longer than the registry is a cycle.
        for _ in 0..=self.records.len() {
            match current.role {
                SurfaceRole::Subsurface => current = self.records.get(&current.parent?)?,
                SurfaceRole::Toplevel | SurfaceRole::X11 { .. } => {
                    return Some((current.id.0, current.generation));
                }
                SurfaceRole::Popup
                | SurfaceRole::ImePopup
                | SurfaceRole::DragIcon
                | SurfaceRole::Layer
                | SurfaceRole::Lock
                | SurfaceRole::Dormant => return None,
            }
        }
        None
    }
}

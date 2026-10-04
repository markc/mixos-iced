// The primitives mutate the model (this state plus surfaces's workspace
// stamps) and return the engine's side effects as an `Effect` list instead
// of performing them. Override-redirect children are found by the
// registry's `parent` (the engine records WM_TRANSIENT_FOR there).
// Workspace semantics, not the canvas's cells.

//! Workspaces (virtual desktops). Every managed toplevel carries a 1-based
//! workspace (unstamped until it maps); every output has a current
//! workspace. A window off its output's current workspace is suppressed
//! through the same visibility funnel minimise uses: no visibility, no
//! frame callbacks, no presentation ([`suppressed`]).
//!
//! Single-output rule: `current` is keyed per output, but a record is
//! compared against the DEFAULT output's current workspace only, so only
//! the default output can be switched.

use std::collections::{BTreeMap, BTreeSet};
use std::hash::Hash;

use serde_json::json;

use comp_model::observation::SetValidationError;
use comp_model::reply::ControlReply;
use comp_model::request::WorkspaceIndex;
use surfaces::{Registry, SurfaceId, SurfaceRecord, SurfaceRole, WindowTargetError};

use crate::Effect;

pub use comp_model::observation::WORKSPACE_COUNT_MAX;

/// Per-compositor workspace state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceState {
    /// Number of workspaces, `1..=WORKSPACE_COUNT_MAX`.
    pub count: u32,
    /// Current workspace per output key (`o_<slug>`); an absent key reads
    /// as 1.
    pub current: BTreeMap<String, u32>,
}

impl Default for WorkspaceState {
    fn default() -> Self {
        Self {
            count: 4,
            current: BTreeMap::new(),
        }
    }
}

/// The default output, by its `o_<slug>` key and its protocol name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefaultOutput {
    pub key: String,
    pub name: String,
}

/// Where a switch or a move is aimed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspaceTarget {
    /// A 1-based workspace index.
    Index(u32),
    Next,
    Prev,
}

impl From<WorkspaceIndex> for WorkspaceTarget {
    fn from(index: WorkspaceIndex) -> Self {
        match index {
            WorkspaceIndex::Absolute(index) => Self::Index(index),
            WorkspaceIndex::Next => Self::Next,
            WorkspaceIndex::Prev => Self::Prev,
        }
    }
}

/// Why a workspace primitive changed nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspaceRefusal {
    /// An index outside `1..=count`.
    InvalidIndex { count: u32 },
    /// `Next`/`Prev` at an end without `wrap`.
    AtEnd { from: u32, count: u32 },
    /// Not the default output (the single-output rule), or no output at all.
    UnknownOutput,
    /// A count outside `1..=WORKSPACE_COUNT_MAX`.
    InvalidCount { max: u32 },
    /// The record is not a mapped managed toplevel.
    NotAWindow,
}

/// What a switch did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceSwitch {
    pub output: String,
    pub from: u32,
    pub to: u32,
}

/// Rule 2: a record that carries a workspace joins the current one at its
/// mapped false→true edge (first commit, or an X11 remap from retained
/// content). Call with `was_mapped` read before the flag flipped. Returns
/// whether it stamped (the engine publishes `_NET_WM_DESKTOP` for a
/// managed X11 window then).
pub fn stamp_workspace_at_map<H: Clone + Eq + Hash>(
    registry: &mut Registry<H>,
    id: SurfaceId,
    was_mapped: bool,
    current: u32,
) -> bool {
    let Some(record) = registry.get(id) else {
        return false;
    };
    if was_mapped || !record.mapped() || !record.role().carries_workspace() {
        return false;
    }
    registry.set_workspace(id, current).is_ok()
}

/// THE workspace term: whether `record` is on the workspace `current`.
/// Only a record that carries a workspace (a managed toplevel, an
/// override-redirect X11 window) has one; everything else is on every
/// workspace. An unstamped carrier is on none.
pub fn on_workspace<H>(record: &SurfaceRecord<H>, current: u32) -> bool {
    !record.role().carries_workspace() || record.workspace() == Some(current)
}

/// The suppression decision: a surface off the current workspace gets no
/// visibility, no frame callbacks and no presentation. Popups and
/// subsurfaces follow their toplevel through the visibility recompute.
pub fn suppressed<H>(record: &SurfaceRecord<H>, current: u32) -> bool {
    !on_workspace(record, current)
}

/// Whether a frame may present this surface's content (without the
/// session-lock term).
pub fn presentable<H>(record: &SurfaceRecord<H>, current: u32) -> bool {
    record.mapped() && !record.minimized() && on_workspace(record, current)
}

/// THE movable term: a mapped managed toplevel on a real workspace.
pub fn workspace_movable<H>(record: &SurfaceRecord<H>) -> bool {
    record.mapped() && record.role().managed_toplevel() && record.workspace().is_some()
}

/// D15: an X11 window's suspended flag is minimised OR off the current
/// workspace.
pub fn x11_suspended<H>(record: &SurfaceRecord<H>, current: u32) -> bool {
    record.minimized() || !on_workspace(record, current)
}

/// Resolve a target against the workspace `from` on a `count`-wide ring.
pub fn resolve_workspace_target(
    from: u32,
    count: u32,
    target: WorkspaceTarget,
    wrap: bool,
) -> Result<u32, WorkspaceRefusal> {
    match target {
        WorkspaceTarget::Index(index) if index == 0 || index > count => {
            Err(WorkspaceRefusal::InvalidIndex { count })
        }
        WorkspaceTarget::Index(index) => Ok(index),
        WorkspaceTarget::Next if from >= count => {
            if wrap {
                Ok(1)
            } else {
                Err(WorkspaceRefusal::AtEnd { from, count })
            }
        }
        WorkspaceTarget::Next => Ok(from + 1),
        WorkspaceTarget::Prev if from <= 1 => {
            if wrap {
                Ok(count)
            } else {
                Err(WorkspaceRefusal::AtEnd { from, count })
            }
        }
        WorkspaceTarget::Prev => Ok(from - 1),
    }
}

/// The gates on a bring-into-view switch the engine knows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SwitchGates {
    pub session_lock: bool,
    pub exclusive_layer: bool,
    /// The window passes the KMS input gate.
    pub input_presentable: bool,
}

/// The one gate on a bring-into-view switch: `Some(workspace)` when a switch to the
/// window's workspace may run. `None` under a session lock or an exclusive
/// layer (D18), and for a window that could not take focus once shown.
pub fn switch_allowed_for<H>(record: &SurfaceRecord<H>, gates: SwitchGates) -> Option<u32> {
    if gates.session_lock || gates.exclusive_layer {
        return None;
    }
    (record.mapped()
        && !record.minimized()
        && record.role().managed_toplevel()
        && gates.input_presentable)
        .then_some(record.workspace())
        .flatten()
}

impl WorkspaceState {
    /// The current workspace of the output `key`; an absent or unknown key
    /// reads as 1.
    pub fn current_for(&self, key: Option<&str>) -> u32 {
        key.and_then(|key| self.current.get(key)).copied().unwrap_or(1)
    }

    /// The default output's current workspace (the single-output rule).
    pub fn current(&self, default_output: Option<&DefaultOutput>) -> u32 {
        self.current_for(default_output.map(|output| output.key.as_str()))
    }

    /// The output key a request addresses: `None` = the default output;
    /// `Some(k)` must be the default output's key or name (the single-output rule).
    pub fn resolve_output(
        default_output: Option<&DefaultOutput>,
        key: Option<&str>,
    ) -> Option<String> {
        let output = default_output?;
        match key {
            None => Some(output.key.clone()),
            Some(requested) => (output.key == requested || output.name == requested)
                .then(|| output.key.clone()),
        }
    }

    /// Whether `record` is a mapped managed toplevel off the current
    /// workspace.
    pub fn window_off_current<H>(
        &self,
        default_output: Option<&DefaultOutput>,
        record: &SurfaceRecord<H>,
    ) -> bool {
        workspace_movable(record) && record.workspace() != Some(self.current(default_output))
    }

    /// Switch the output `key` (`None` = default) to `target`. A switch to
    /// the current workspace is `Ok` with no effects. Minimise state is
    /// untouched.
    pub fn switch<H: Clone + Eq + Hash>(
        &mut self,
        registry: &Registry<H>,
        default_output: Option<&DefaultOutput>,
        key: Option<&str>,
        target: WorkspaceTarget,
        wrap: bool,
        prefer: Option<SurfaceId>,
    ) -> Result<(WorkspaceSwitch, Vec<Effect>), WorkspaceRefusal> {
        let output =
            Self::resolve_output(default_output, key).ok_or(WorkspaceRefusal::UnknownOutput)?;
        let from = self.current_for(Some(&output));
        let to = resolve_workspace_target(from, self.count, target, wrap)?;
        let switched = WorkspaceSwitch {
            output: output.clone(),
            from,
            to,
        };
        if to == from {
            return Ok((switched, Vec::new()));
        }
        let mut leaving = Vec::new();
        let mut arriving = Vec::new();
        for record in registry.surface_rows() {
            if !record.mapped() || !record.role().managed_toplevel() {
                continue;
            }
            if record.workspace() == Some(from) {
                leaving.push(record.id());
            } else if record.workspace() == Some(to) {
                arriving.push(record.id());
            }
        }
        // `current` moves first: the per-window halves derive the X11
        // suspended flag from it.
        self.current.insert(output, to);
        let mut effects = Vec::new();
        effects.extend(leaving.into_iter().map(|id| Effect::Withdraw {
            id,
            cause: "workspace.switch",
        }));
        effects.extend(arriving.into_iter().map(|id| Effect::Present {
            id,
            cause: "workspace.switch",
        }));
        effects.push(Effect::WorkspacesDirty("workspace.switch"));
        effects.push(Effect::PublishDesktops);
        effects.push(Effect::Settle { prefer });
        Ok((switched, effects))
    }

    /// Move one mapped managed toplevel to `target` without switching.
    /// `Next`/`Prev` are relative to the window's own workspace and always
    /// wrap. Returns `(from, to)`; the generation is never touched.
    pub fn move_window<H: Clone + Eq + Hash>(
        &mut self,
        registry: &mut Registry<H>,
        default_output: Option<&DefaultOutput>,
        id: SurfaceId,
        target: WorkspaceTarget,
        prefer: Option<SurfaceId>,
    ) -> Result<((u32, u32), Vec<Effect>), WorkspaceRefusal> {
        let from = registry
            .get(id)
            .filter(|record| workspace_movable(record))
            .and_then(SurfaceRecord::workspace)
            .ok_or(WorkspaceRefusal::NotAWindow)?;
        let to = resolve_workspace_target(from, self.count, target, true)?;
        if to == from {
            return Ok(((from, to), Vec::new()));
        }
        let current = self.current(default_output);
        let mut effects = relabel(registry, id, to);
        if from == current {
            effects.push(Effect::Withdraw {
                id,
                cause: "workspace.move",
            });
        } else if to == current {
            effects.push(Effect::Present {
                id,
                cause: "workspace.move",
            });
        }
        effects.push(Effect::WorkspacesDirty("workspace.move"));
        if from == current || to == current {
            effects.push(Effect::Settle { prefer });
        }
        Ok(((from, to), effects))
    }

    /// Move one window to `target` AND make that workspace current, in ONE
    /// settle, with the window raised and preferred by the keyboard
    /// throughout, so no bystander on either workspace takes the keyboard
    /// in between. `allowed` is [`switch_allowed_for`]'s answer for the
    /// window: only then does the settle prefer it.
    pub fn move_and_follow<H: Clone + Eq + Hash>(
        &mut self,
        registry: &mut Registry<H>,
        default_output: Option<&DefaultOutput>,
        id: SurfaceId,
        target: WorkspaceTarget,
        allowed: bool,
    ) -> Result<((u32, u32), Vec<Effect>), WorkspaceRefusal> {
        let Some(output) = default_output else {
            return self.move_window(registry, None, id, target, None);
        };
        let from = registry
            .get(id)
            .filter(|record| workspace_movable(record))
            .and_then(SurfaceRecord::workspace)
            .ok_or(WorkspaceRefusal::NotAWindow)?;
        let to = resolve_workspace_target(from, self.count, target, true)?;
        let current = self.current(Some(output));
        let prefer = allowed.then_some(id);
        // On top of its band first, so the arrival is also a raise.
        let mut effects = vec![Effect::Raise(id)];
        if to == current {
            let (moved, more) = self.move_window(registry, Some(output), id, target, prefer)?;
            effects.extend(more);
            return Ok((moved, effects));
        }
        if from != to {
            effects.extend(relabel(registry, id, to));
        }
        // The window is on `to` already, so the switch presents it.
        let key = output.key.clone();
        match self.switch(
            registry,
            Some(output),
            Some(&key),
            WorkspaceTarget::Index(to),
            true,
            prefer,
        ) {
            Ok((_, more)) => {
                effects.extend(more);
                Ok(((from, to), effects))
            }
            Err(refusal) => {
                // Unreachable by construction: `to` came from the ring and
                // the key from the default output. Put the label back.
                if from != to {
                    relabel(registry, id, from);
                }
                Err(refusal)
            }
        }
    }

    /// Set the workspace count. Shrinking strands: every record above the
    /// new count moves to the last workspace and every output's current is
    /// clamped. Returns `(old, new)`.
    pub fn set_count<H: Clone + Eq + Hash>(
        &mut self,
        registry: &mut Registry<H>,
        count: u32,
    ) -> Result<((u32, u32), Vec<Effect>), WorkspaceRefusal> {
        if count == 0 || count > WORKSPACE_COUNT_MAX {
            return Err(WorkspaceRefusal::InvalidCount {
                max: WORKSPACE_COUNT_MAX,
            });
        }
        let old = self.count;
        self.count = count;
        if count == old {
            return Ok(((old, count), Vec::new()));
        }
        let mut effects = vec![Effect::WorkspacesDirty("workspace.count")];
        if count > old {
            // A grow strands nothing: only the root count changes.
            effects.push(Effect::PublishDesktops);
            return Ok(((old, count), effects));
        }
        // Every record with a workspace, override-redirect included: an OR
        // record left above the count would be hidden on every workspace.
        let stranded = registry
            .surface_rows()
            .filter(|record| {
                record.role().carries_workspace()
                    && record.workspace().is_some_and(|workspace| workspace > count)
            })
            .map(SurfaceRecord::id)
            .collect::<Vec<_>>();
        for id in stranded {
            if registry.set_workspace(id, count).is_ok() {
                effects.push(Effect::Relabelled {
                    id,
                    to: count,
                    cause: "workspace.count",
                });
            }
        }
        for current in self.current.values_mut() {
            if *current > count {
                *current = count;
            }
        }
        effects.push(Effect::ResyncAllX11);
        effects.push(Effect::PublishDesktops);
        effects.push(Effect::Settle { prefer: None });
        Ok(((old, count), effects))
    }

    /// Bring a window's workspace on screen before activating it (focus,
    /// restore, xdg-activation, X11 activate all go through this). `allowed`
    /// is [`switch_allowed_for`]'s answer. Inert (no effects) when it is
    /// `None` or the window is already on the current workspace; otherwise
    /// the switch's settle prefers the window itself.
    pub fn ensure_shown<H: Clone + Eq + Hash>(
        &mut self,
        registry: &Registry<H>,
        default_output: Option<&DefaultOutput>,
        id: SurfaceId,
        allowed: Option<u32>,
    ) -> Option<Vec<Effect>> {
        let workspace = allowed?;
        if workspace == self.current(default_output) {
            return None;
        }
        self.switch(
            registry,
            default_output,
            None,
            WorkspaceTarget::Index(workspace),
            true,
            Some(id),
        )
        .ok()
        .map(|(_, effects)| effects)
    }

    /// After an output topology change: the retiring default output's
    /// current workspace is carried to the replacing one, and if the
    /// effective value changed anyway the state is re-derived and settled.
    /// `previous_key` and `previous_current` are read before the change.
    pub fn reconcile_after_topology_change(
        &mut self,
        default_output: Option<&DefaultOutput>,
        previous_key: Option<&str>,
        previous_current: u32,
    ) -> Vec<Effect> {
        if let Some(output) = default_output
            && previous_key.is_some_and(|previous| previous != output.key)
        {
            self.current.insert(output.key.clone(), previous_current);
        }
        let mut effects = vec![Effect::PublishDesktops];
        if self.current(default_output) != previous_current {
            effects.push(Effect::ResyncAllX11);
            effects.push(Effect::Settle { prefer: None });
        }
        effects
    }
}

/// THE per-window relabel every move goes through: the record and its
/// override-redirect descendants (a menu, its submenu, a tooltip:
/// every OR record whose `WM_TRANSIENT_FOR` names the
/// owner, read from the registry's `transient_for`) move to `to`. The walk
/// keeps a visited set: the property is client-controlled.
fn relabel<H: Clone + Eq + Hash>(registry: &mut Registry<H>, owner: SurfaceId, to: u32) -> Vec<Effect> {
    let mut effects = Vec::new();
    let mut seen = BTreeSet::from([owner]);
    let mut pending = vec![owner];
    while let Some(id) = pending.pop() {
        if registry.set_workspace(id, to).is_ok() {
            effects.push(Effect::Relabelled {
                id,
                to,
                cause: "workspace.move",
            });
        }
        // Only an X11 window can be named by WM_TRANSIENT_FOR.
        let Some(owner_handle) = registry
            .get(id)
            .filter(|record| matches!(record.role(), SurfaceRole::X11 { .. }))
            .map(|record| record.handle().clone())
        else {
            continue;
        };
        let children = registry
            .surface_rows()
            .filter(|record| {
                record.id() != id
                    && record.role()
                        == SurfaceRole::X11 {
                            override_redirect: true,
                        }
                    && record.transient_for() == Some(&owner_handle)
            })
            .map(SurfaceRecord::id)
            .collect::<Vec<_>>();
        for child in children {
            if seen.insert(child) {
                pending.push(child);
            }
        }
    }
    effects
}

/// `range` for an index refusal, as `1..=<count>`.
pub fn workspace_index_range(count: u32) -> &'static str {
    const RANGES: [&str; 16] = [
        "1..=1", "1..=2", "1..=3", "1..=4", "1..=5", "1..=6", "1..=7", "1..=8", "1..=9", "1..=10",
        "1..=11", "1..=12", "1..=13", "1..=14", "1..=15", "1..=16",
    ];
    RANGES
        .get(count.saturating_sub(1) as usize)
        .copied()
        .unwrap_or("1..=workspaces.count")
}

/// The contract's wire form of a core refusal. `output` is the requested
/// output resolved to its key, echoed on `at_end`; `id` the window a send
/// named.
pub fn workspace_refusal(refusal: WorkspaceRefusal, output: Option<&str>, id: u64) -> ControlReply {
    match refusal {
        WorkspaceRefusal::InvalidIndex { count } => {
            ControlReply::Validation(SetValidationError::InvalidValue {
                path: "index".into(),
                expected: "unsigned integer",
                range: workspace_index_range(count),
            })
        }
        WorkspaceRefusal::AtEnd { from, count } => ControlReply::refused(
            "at_end",
            json!({"output": output, "from": from, "count": count}),
        ),
        WorkspaceRefusal::UnknownOutput => {
            ControlReply::Validation(SetValidationError::InvalidValue {
                path: "output".into(),
                expected: "outputs.<key> key or output name",
                range: "an existing output",
            })
        }
        WorkspaceRefusal::InvalidCount { max: _ } => {
            ControlReply::Validation(SetValidationError::InvalidValue {
                path: "count".into(),
                expected: "unsigned integer",
                range: "1..=16",
            })
        }
        // Unreachable after the `{id, generation}` fence, kept honest.
        WorkspaceRefusal::NotAWindow => ControlReply::WindowTarget {
            id,
            error: WindowTargetError::NotMapped,
        },
    }
}

/// `comp.workspace.switch`: the reply and
/// the effects, or the refusal.
pub fn service_switch<H: Clone + Eq + Hash>(
    state: &mut WorkspaceState,
    registry: &Registry<H>,
    default_output: Option<&DefaultOutput>,
    output: Option<&str>,
    index: WorkspaceIndex,
    wrap: bool,
) -> (ControlReply, Vec<Effect>) {
    match state.switch(registry, default_output, output, index.into(), wrap, None) {
        Ok((switched, effects)) => (
            ControlReply::Body(json!({
                "output": switched.output,
                "from": switched.from,
                "to": switched.to,
            })),
            effects,
        ),
        Err(refusal) => {
            // `at_end` names the output by its key, as the success reply
            // does; only an unknown output fails to resolve.
            let output = WorkspaceState::resolve_output(default_output, output);
            (workspace_refusal(refusal, output.as_deref(), 0), Vec::new())
        }
    }
}

#[cfg(test)]
#[path = "workspaces_tests.rs"]
mod tests;

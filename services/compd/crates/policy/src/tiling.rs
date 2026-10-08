// SPDX-License-Identifier: MIT OR Apache-2.0
//! Column allocation and bounded requested membership, without a renderer.
//!
//! This is the internal owner foundation, not a public tile operation. A host
//! must execute an admitted plan through its real geometry/protocol owner before
//! advertising tile verbs or committed tiled state. Rectangles here are desired
//! allocations; ACK, buffer geometry and presentation are separate observations.

use std::hash::Hash;

use surfaces::{Registry, SurfaceId, WindowTargetError};

pub const MAX_MEMBERS: usize = 256;
const MAX_OUTPUT_NAME_BYTES: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    fn valid(self) -> bool {
        self.width > 0
            && self.height > 0
            && self.x.checked_add(self.width).is_some()
            && self.y.checked_add(self.height).is_some()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Insets {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Constraints {
    /// Client content hints: zero means unset, as in xdg_toplevel.
    pub min_size: (i32, i32),
    pub max_size: (i32, i32),
    /// Prepared target-mode SSD metrics, supplied by the geometry owner.
    pub insets: Insets,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutError {
    NoOutput,
    InvalidArea,
    Capacity,
    InvalidConstraints { index: usize },
    InsufficientArea { index: usize },
}

impl LayoutError {
    pub fn name(self) -> &'static str {
        match self {
            Self::NoOutput => "no_output",
            Self::InvalidArea => "invalid_area",
            Self::Capacity => "capacity",
            Self::InvalidConstraints { .. } => "invalid_constraints",
            Self::InsufficientArea { .. } => "insufficient_area",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub outer: Rect,
    pub content: Rect,
}

/// Partition the complete usable width in stable member order. Wide arithmetic
/// conserves odd widths, with neither gaps nor overlapping outer cells. An
/// infeasible group yields no partial plan and never a zero-size configure.
pub fn columns(area: Rect, constraints: &[Constraints]) -> Result<Vec<Cell>, LayoutError> {
    if constraints.len() > MAX_MEMBERS {
        return Err(LayoutError::Capacity);
    }
    if !area.valid() {
        return Err(LayoutError::InvalidArea);
    }
    if constraints.is_empty() {
        return Ok(Vec::new());
    }
    let count = constraints.len() as i64;
    let mut cells = Vec::with_capacity(constraints.len());
    for (index, hints) in constraints.iter().enumerate() {
        let Insets {
            left,
            top,
            right,
            bottom,
        } = hints.insets;
        let (min_w, min_h) = hints.min_size;
        let (max_w, max_h) = hints.max_size;
        if [left, top, right, bottom, min_w, min_h, max_w, max_h]
            .iter()
            .any(|value| *value < 0)
            || (max_w > 0 && max_w < min_w)
            || (max_h > 0 && max_h < min_h)
        {
            return Err(LayoutError::InvalidConstraints { index });
        }
        let start = index as i64 * i64::from(area.width) / count;
        let end = (index as i64 + 1) * i64::from(area.width) / count;
        let outer = Rect {
            x: (i64::from(area.x) + start) as i32,
            y: area.y,
            width: (end - start) as i32,
            height: area.height,
        };
        let width = i64::from(outer.width) - i64::from(left) - i64::from(right);
        let height = i64::from(outer.height) - i64::from(top) - i64::from(bottom);
        if width < i64::from(min_w.max(1))
            || height < i64::from(min_h.max(1))
            || (max_w > 0 && width > i64::from(max_w))
            || (max_h > 0 && height > i64::from(max_h))
        {
            return Err(LayoutError::InsufficientArea { index });
        }
        cells.push(Cell {
            outer,
            content: Rect {
                x: outer.x + left,
                y: outer.y + top,
                width: width as i32,
                height: height as i32,
            },
        });
    }
    Ok(cells)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    pub id: SurfaceId,
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    /// Actual output name, rather than a transient facade key.
    pub output: String,
    pub workspace: u32,
}

impl Group {
    fn valid(&self) -> bool {
        !self.output.is_empty() && self.output.len() <= MAX_OUTPUT_NAME_BYTES && self.workspace > 0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub target: Target,
    pub group: Group,
    /// Captured once from the decided normal slot, never from a later buffer.
    pub normal: Rect,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    pub constraints: Constraints,
    /// Requested OR committed maximise/fullscreen owns this window's slot.
    pub overlay: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Allocation {
    pub target: Target,
    pub cell: Cell,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Plan {
    Ready(Vec<Allocation>),
    /// Existing members and normal restores survive a temporarily infeasible
    /// output/work area. No desired cell is fabricated while pending.
    Pending(LayoutError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdmissionError {
    UnsupportedProtocol,
    Target(WindowTargetError),
    InvalidGroup,
    InvalidRestore,
    Capacity,
    Layout(LayoutError),
}

/// Shared public-control admission fence: removal remains valid while suspended.
pub fn resolve_control_target<H: Clone + Eq + Hash>(
    registry: &Registry<H>,
    target: Target,
    enabled: bool,
) -> Result<(), AdmissionError> {
    if enabled {
        registry
            .resolve_window_target(target.id.0, Some(target.generation))
            .map_err(AdmissionError::Target)?;
    } else {
        let record = registry
            .get(target.id)
            .ok_or(AdmissionError::Target(WindowTargetError::UnknownWindow))?;
        if record.generation() != target.generation {
            return Err(AdmissionError::Target(WindowTargetError::StaleTarget {
                requested: target.generation,
                current: record.generation(),
            }));
        }
        if !record.role().managed_toplevel() {
            return Err(AdmissionError::Target(WindowTargetError::NotManaged));
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tiles {
    /// One bounded admission order across groups; suspension and transfer keep
    /// that order, so resuming a member cannot append it behind younger peers.
    members: Vec<Member>,
}

impl Tiles {
    pub fn members(&self) -> &[Member] {
        &self.members
    }

    pub fn member(&self, target: Target) -> Option<&Member> {
        self.members.iter().find(|member| member.target == target)
    }

    /// Validate the complete proposed destination allocation before mutation.
    /// Failure leaves all membership/restores untouched. Repeated same-group
    /// admission is idempotent; transfer preserves the first normal restore.
    pub fn admit<H: Clone + Eq + Hash>(
        &mut self,
        registry: &Registry<H>,
        request: Member,
        usable: Option<Rect>,
        facts: impl Fn(Target) -> Facts,
    ) -> Result<bool, AdmissionError> {
        let record = registry
            .resolve_window_target(request.target.id.0, Some(request.target.generation))
            .map_err(AdmissionError::Target)?;
        if !request.group.valid() || record.workspace() != Some(request.group.workspace) {
            return Err(AdmissionError::InvalidGroup);
        }
        if let Some(existing) = self.member(request.target)
            && existing.group == request.group
        {
            return Ok(false);
        }
        let mut proposed = self.clone();
        proposed.reconcile(registry);
        let group = request.group.clone();
        if let Some(existing) = proposed
            .members
            .iter_mut()
            .find(|member| member.target == request.target)
        {
            existing.group = request.group;
        } else {
            if !request.normal.valid() {
                return Err(AdmissionError::InvalidRestore);
            }
            if proposed.members.len() == MAX_MEMBERS {
                return Err(AdmissionError::Capacity);
            }
            proposed.members.push(request);
        }
        if let Plan::Pending(reason) = proposed.plan(registry, &group, usable, facts) {
            return Err(AdmissionError::Layout(reason));
        }
        *self = proposed;
        Ok(true)
    }

    /// Generation-fenced removal also works while the same role is unmapped.
    /// Lifecycle retirement uses forget(), whose caller owns the registry event.
    pub fn remove<H: Clone + Eq + Hash>(
        &mut self,
        registry: &Registry<H>,
        target: Target,
    ) -> Result<Option<Member>, AdmissionError> {
        resolve_control_target(registry, target, false)?;
        let Some(index) = self
            .members
            .iter()
            .position(|member| member.target == target)
        else {
            return Ok(None);
        };
        Ok(Some(self.members.remove(index)))
    }

    pub fn forget(&mut self, id: SurfaceId) {
        self.members.retain(|member| member.target.id != id);
    }
    /// Output-loss fallback, chosen by the geometry owner from actual mapped
    /// outputs. Retain original admission order and every normal restore.
    pub fn retarget_output(&mut self, old: &str, new: &str) {
        if new.is_empty() || new.len() > MAX_OUTPUT_NAME_BYTES {
            return;
        }
        for member in &mut self.members {
            if member.group.output == old {
                member.group.output = new.into();
            }
        }
    }

    /// Actual registry lifetime/workspace changes, without a timer. Unmap and
    /// minimise preserve membership; destruction or role/UUID generation changes
    /// retire it. Workspace moves transfer while preserving admission order.
    pub fn reconcile<H: Clone + Eq + Hash>(&mut self, registry: &Registry<H>) {
        self.members.retain_mut(|member| {
            let Some(record) = registry.get(member.target.id) else {
                return false;
            };
            if record.generation() != member.target.generation || !record.role().managed_toplevel()
            {
                return false;
            }
            if let Some(workspace) = record.workspace() {
                member.group.workspace = workspace;
            }
            true
        });
    }

    pub fn plan<H: Clone + Eq + Hash>(
        &self,
        registry: &Registry<H>,
        group: &Group,
        usable: Option<Rect>,
        facts: impl Fn(Target) -> Facts,
    ) -> Plan {
        let active: Vec<_> = self
            .members
            .iter()
            .filter_map(|member| {
                let record = registry.get(member.target.id)?;
                if member.group != *group
                    || record.workspace() != Some(group.workspace)
                    || record.generation() != member.target.generation
                    || !record.role().managed_toplevel()
                    || !record.mapped()
                    || record.minimized()
                {
                    return None;
                }
                let facts = facts(member.target);
                (!facts.overlay).then_some((member.target, facts.constraints))
            })
            .collect();
        if active.is_empty() {
            return Plan::Ready(Vec::new());
        }
        let Some(area) = usable else {
            return Plan::Pending(LayoutError::NoOutput);
        };
        let constraints: Vec<_> = active.iter().map(|(_, constraints)| *constraints).collect();
        match columns(area, &constraints) {
            Ok(cells) => Plan::Ready(
                active
                    .into_iter()
                    .zip(cells)
                    .map(|((target, _), cell)| Allocation { target, cell })
                    .collect(),
            ),
            Err(error) => Plan::Pending(error),
        }
    }
}

#[cfg(test)]
mod tests;

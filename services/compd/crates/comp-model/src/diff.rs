// The snapshot and occlusion types are this crate's; the prop helpers and
// `queue_prop_change` are `crate::observation`'s; `collect_snapshot_diff` is
// public (the engine runs it).

//! The `props.changed` reducers: two snapshots in, the changed leaves out.

use std::collections::BTreeSet;

use crate::observation::{
    HOST_PASSTHROUGH_PATH, PendingPropChanges, PropValue, prop_opt_string, prop_opt_u32,
    prop_opt_u64, prop_str, queue_prop_change,
};
use crate::snapshot::*;

pub fn collect_snapshot_diff(
    old: &CompSnapshot,
    new: &CompSnapshot,
    cause: &'static str,
    pending: &mut PendingPropChanges,
) {
    let output_keys = old
        .outputs
        .keys()
        .chain(new.outputs.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    for key in output_keys {
        diff_output_row(
            &format!("outputs.{key}"),
            old.outputs.get(&key),
            new.outputs.get(&key),
            cause,
            pending,
        );
    }
    let surface_keys = old
        .surfaces
        .keys()
        .chain(new.surfaces.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    for key in surface_keys {
        diff_surface_row(
            &format!("surfaces.{key}"),
            old.surfaces.get(&key),
            new.surfaces.get(&key),
            cause,
            pending,
        );
    }
    let window_keys = old
        .windows
        .keys()
        .chain(new.windows.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    for key in window_keys {
        diff_window_row(
            &format!("windows.{key}"),
            old.windows.get(&key),
            new.windows.get(&key),
            cause,
            pending,
        );
    }
    queue_prop_change(
        pending,
        "stack".into(),
        PropValue::U64List(old.stack.clone()),
        PropValue::U64List(new.stack.clone()),
        cause,
    );
    diff_focus("focus", &old.focus, &new.focus, cause, pending);
    queue_prop_change(
        pending,
        "decoration.enabled".into(),
        PropValue::Bool(old.decoration.enabled),
        PropValue::Bool(new.decoration.enabled),
        cause,
    );
    queue_prop_change(
        pending,
        "decoration.style".into(),
        prop_str(old.decoration.style),
        prop_str(new.decoration.style),
        cause,
    );
    queue_prop_change(
        pending,
        "bindings.enabled".into(),
        PropValue::Bool(old.bindings.enabled),
        PropValue::Bool(new.bindings.enabled),
        cause,
    );
    queue_prop_change(
        pending,
        "bindings.profile".into(),
        prop_str(old.bindings.profile),
        prop_str(new.bindings.profile),
        cause,
    );
    queue_prop_change(
        pending,
        "bindings.table".into(),
        PropValue::BindingRows(old.bindings.table.clone()),
        PropValue::BindingRows(new.bindings.table.clone()),
        cause,
    );
    diff_corners(old, new, cause, pending);
    diff_workspaces(old, new, cause, pending);
    #[cfg(feature = "xwayland")]
    queue_prop_change(
        pending,
        "xwayland.display".into(),
        prop_opt_string(old.xwayland.display.as_deref()),
        prop_opt_string(new.xwayland.display.as_deref()),
        cause,
    );
    #[cfg(feature = "xwayland")]
    queue_prop_change(
        pending,
        "xwayland.state".into(),
        prop_str(old.xwayland.state),
        prop_str(new.xwayland.state),
        cause,
    );
    #[cfg(feature = "xwayland")]
    queue_prop_change(
        pending,
        "xwayland.failures".into(),
        PropValue::U32(old.xwayland.failures),
        PropValue::U32(new.xwayland.failures),
        cause,
    );
}

/// `workspaces.*`: the scalar leaves, one `o_<key>.current` per output in
/// either snapshot (an output that left reads null, like an output row),
/// and the row list as one value (the `bindings.table` precedent).
fn diff_workspaces(
    old: &CompSnapshot,
    new: &CompSnapshot,
    cause: &'static str,
    pending: &mut PendingPropChanges,
) {
    let (old, new) = (&old.workspaces, &new.workspaces);
    queue_prop_change(
        pending,
        "workspaces.count".into(),
        PropValue::U32(old.count),
        PropValue::U32(new.count),
        cause,
    );
    queue_prop_change(
        pending,
        "workspaces.current".into(),
        PropValue::U32(old.current),
        PropValue::U32(new.current),
        cause,
    );
    let keys = old
        .outputs
        .keys()
        .chain(new.outputs.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    for key in keys {
        let current = |row: Option<&OutputWorkspaceSnapshot>| {
            row.map_or_else(PropValue::null, |row| PropValue::U32(row.current))
        };
        queue_prop_change(
            pending,
            format!("workspaces.{key}.current"),
            current(old.outputs.get(&key)),
            current(new.outputs.get(&key)),
            cause,
        );
    }
    queue_prop_change(
        pending,
        "workspaces.list".into(),
        PropValue::WorkspaceRows(old.list.clone()),
        PropValue::WorkspaceRows(new.list.clone()),
        cause,
    );
}

fn diff_corners(
    old: &CompSnapshot,
    new: &CompSnapshot,
    cause: &'static str,
    pending: &mut PendingPropChanges,
) {
    let (old_host, new_host) = (old.input.host, new.input.host);
    let old = old.input.corners;
    let new = new.input.corners;
    for (leaf, old, new) in [
        (
            "enabled",
            PropValue::Bool(old.enabled),
            PropValue::Bool(new.enabled),
        ),
        (
            "deadzone_px",
            PropValue::F64(old.deadzone_px),
            PropValue::F64(new.deadzone_px),
        ),
        (
            "dwell_ms",
            PropValue::U64(old.dwell_ms),
            PropValue::U64(new.dwell_ms),
        ),
        (
            "velocity_max_px_s",
            PropValue::F64(old.velocity_max_px_s),
            PropValue::F64(new.velocity_max_px_s),
        ),
        (
            "affordance",
            PropValue::Bool(old.affordance),
            PropValue::Bool(new.affordance),
        ),
        (
            "discovery",
            PropValue::Bool(old.discovery),
            PropValue::Bool(new.discovery),
        ),
    ] {
        queue_prop_change(pending, format!("input.corners.{leaf}"), old, new, cause);
    }
    if let (Some(old), Some(new)) = (old_host, new_host) {
        queue_prop_change(
            pending,
            HOST_PASSTHROUGH_PATH.into(),
            PropValue::Bool(old.passthrough),
            PropValue::Bool(new.passthrough),
            cause,
        );
    }
}

fn diff_output_row(
    prefix: &str,
    old: Option<&OutputSnapshot>,
    new: Option<&OutputSnapshot>,
    cause: &'static str,
    pending: &mut PendingPropChanges,
) {
    match (old, new) {
        (None, None) => {}
        (None, Some(new)) => {
            queue_prop_change(
                pending,
                prefix.into(),
                PropValue::null(),
                PropValue::OutputRow(Box::new(OutputSnapshot {
                    presentation: None,
                    ..new.clone()
                })),
                cause,
            );
        }
        (Some(old), None) => {
            queue_prop_change(
                pending,
                prefix.into(),
                PropValue::OutputRow(Box::new(OutputSnapshot {
                    presentation: None,
                    ..old.clone()
                })),
                PropValue::null(),
                cause,
            );
        }
        (Some(old), Some(new)) => {
            for (leaf, old, new) in [
                ("name", prop_str(&old.name), prop_str(&new.name)),
                (
                    "default",
                    PropValue::Bool(old.default),
                    PropValue::Bool(new.default),
                ),
                ("x", PropValue::I32(old.x), PropValue::I32(new.x)),
                ("y", PropValue::I32(old.y), PropValue::I32(new.y)),
                (
                    "width",
                    PropValue::U32(old.width),
                    PropValue::U32(new.width),
                ),
                (
                    "height",
                    PropValue::U32(old.height),
                    PropValue::U32(new.height),
                ),
                (
                    "scale",
                    PropValue::F64(old.scale),
                    PropValue::F64(new.scale),
                ),
                (
                    "refresh_mhz",
                    PropValue::U32(old.refresh_mhz),
                    PropValue::U32(new.refresh_mhz),
                ),
                (
                    "instance",
                    PropValue::String(old.instance.clone()),
                    PropValue::String(new.instance.clone()),
                ),
                (
                    "generation",
                    PropValue::U64(old.generation),
                    PropValue::U64(new.generation),
                ),
                (
                    "usable.x",
                    PropValue::F32(old.usable.x),
                    PropValue::F32(new.usable.x),
                ),
                (
                    "usable.y",
                    PropValue::F32(old.usable.y),
                    PropValue::F32(new.usable.y),
                ),
                (
                    "usable.width",
                    PropValue::F32(old.usable.width),
                    PropValue::F32(new.usable.width),
                ),
                (
                    "usable.height",
                    PropValue::F32(old.usable.height),
                    PropValue::F32(new.usable.height),
                ),
            ] {
                queue_prop_change(pending, format!("{prefix}.{leaf}"), old, new, cause);
            }
        }
    }
}

fn diff_occlusion(
    prefix: &str,
    old: &OcclusionProps,
    new: &OcclusionProps,
    cause: &'static str,
    pending: &mut PendingPropChanges,
) {
    for (leaf, old, new) in [
        (
            "occluded",
            PropValue::Bool(old.occluded),
            PropValue::Bool(new.occluded),
        ),
        (
            "occlusion_reason",
            prop_str(old.occlusion_reason),
            prop_str(new.occlusion_reason),
        ),
        (
            "occlusion_revision",
            PropValue::U64(old.occlusion_revision),
            PropValue::U64(new.occlusion_revision),
        ),
    ] {
        queue_prop_change(pending, format!("{prefix}.{leaf}"), old, new, cause);
    }
}

fn diff_surface_row(
    prefix: &str,
    old: Option<&SurfaceSnapshot>,
    new: Option<&SurfaceSnapshot>,
    cause: &'static str,
    pending: &mut PendingPropChanges,
) {
    if let (Some(old), Some(new)) = (old, new) {
        diff_occlusion(prefix, &old.occlusion, &new.occlusion, cause, pending);
    }
    let (old, new) = match (old, new) {
        (None, None) => return,
        (None, Some(new)) => {
            let new = new.clone();
            queue_prop_change(
                pending,
                prefix.into(),
                PropValue::null(),
                PropValue::SurfaceRow(Box::new(new)),
                cause,
            );
            return;
        }
        (Some(old), None) => {
            let old = old.clone();
            queue_prop_change(
                pending,
                prefix.into(),
                PropValue::SurfaceRow(Box::new(old)),
                PropValue::null(),
                cause,
            );
            return;
        }
        (Some(old), Some(new)) => (old, new),
    };
    for (leaf, old, new) in [
        ("id", PropValue::U64(old.id), PropValue::U64(new.id)),
        ("role", prop_str(old.role), prop_str(new.role)),
        (
            "mapped",
            PropValue::Bool(old.mapped),
            PropValue::Bool(new.mapped),
        ),
        (
            "visible",
            PropValue::Bool(old.visible),
            PropValue::Bool(new.visible),
        ),
        ("x", PropValue::F32(old.x), PropValue::F32(new.x)),
        ("y", PropValue::F32(old.y), PropValue::F32(new.y)),
        (
            "width",
            PropValue::F32(old.width),
            PropValue::F32(new.width),
        ),
        (
            "height",
            PropValue::F32(old.height),
            PropValue::F32(new.height),
        ),
        ("band", prop_str(old.band), prop_str(new.band)),
        (
            "sequence",
            PropValue::U64(old.sequence),
            PropValue::U64(new.sequence),
        ),
        (
            "tree_index",
            PropValue::U32(old.tree_index),
            PropValue::U32(new.tree_index),
        ),
        ("parent", prop_opt_u64(old.parent), prop_opt_u64(new.parent)),
        (
            "output",
            prop_opt_string(old.output.as_deref()),
            prop_opt_string(new.output.as_deref()),
        ),
        (
            "title",
            prop_opt_string(old.title.as_deref()),
            prop_opt_string(new.title.as_deref()),
        ),
        (
            "app_id",
            prop_opt_string(old.app_id.as_deref()),
            prop_opt_string(new.app_id.as_deref()),
        ),
        (
            "focused",
            PropValue::Bool(old.focused),
            PropValue::Bool(new.focused),
        ),
        (
            "activated",
            PropValue::Bool(old.activated),
            PropValue::Bool(new.activated),
        ),
        (
            "maximized",
            PropValue::Bool(old.maximized),
            PropValue::Bool(new.maximized),
        ),
        (
            "fullscreen",
            PropValue::Bool(old.fullscreen),
            PropValue::Bool(new.fullscreen),
        ),
        (
            "minimized",
            PropValue::Bool(old.minimized),
            PropValue::Bool(new.minimized),
        ),
        (
            "workspace",
            prop_opt_u32(old.workspace),
            prop_opt_u32(new.workspace),
        ),
        (
            "decoration",
            prop_opt_string(old.decoration),
            prop_opt_string(new.decoration),
        ),
        (
            "foreign_id",
            prop_opt_string(old.foreign_id.as_deref()),
            prop_opt_string(new.foreign_id.as_deref()),
        ),
        (
            "generation",
            PropValue::U64(old.generation),
            PropValue::U64(new.generation),
        ),
    ] {
        queue_prop_change(pending, format!("{prefix}.{leaf}"), old, new, cause);
    }
    diff_layer(
        prefix,
        old.layer.as_ref(),
        new.layer.as_ref(),
        cause,
        pending,
    );
}

fn diff_layer(
    prefix: &str,
    old: Option<&LayerSnapshot>,
    new: Option<&LayerSnapshot>,
    cause: &'static str,
    pending: &mut PendingPropChanges,
) {
    let old_stratum = old.map_or_else(PropValue::null, |row| prop_str(row.stratum));
    let new_stratum = new.map_or_else(PropValue::null, |row| prop_str(row.stratum));
    let old_interactivity = old.map_or_else(PropValue::null, |row| prop_str(row.interactivity));
    let new_interactivity = new.map_or_else(PropValue::null, |row| prop_str(row.interactivity));
    let old_zone = old.map_or_else(PropValue::null, |row| PropValue::I32(row.exclusive_zone));
    let new_zone = new.map_or_else(PropValue::null, |row| PropValue::I32(row.exclusive_zone));
    let old_binding = old.map_or_else(PropValue::null, |row| prop_str(row.binding));
    let new_binding = new.map_or_else(PropValue::null, |row| prop_str(row.binding));
    for (leaf, old, new) in [
        ("stratum", old_stratum, new_stratum),
        ("interactivity", old_interactivity, new_interactivity),
        ("exclusive_zone", old_zone, new_zone),
        ("binding", old_binding, new_binding),
    ] {
        queue_prop_change(pending, format!("{prefix}.layer.{leaf}"), old, new, cause);
    }
}

fn diff_window_row(
    prefix: &str,
    old: Option<&WindowSnapshot>,
    new: Option<&WindowSnapshot>,
    cause: &'static str,
    pending: &mut PendingPropChanges,
) {
    if let (Some(old), Some(new)) = (old, new) {
        diff_occlusion(prefix, &old.occlusion, &new.occlusion, cause, pending);
    }
    let (old, new) = match (old, new) {
        (None, None) => return,
        (None, Some(new)) => {
            queue_prop_change(
                pending,
                prefix.into(),
                PropValue::null(),
                PropValue::WindowRow(Box::new(WindowSnapshot {
                    presentation: None,
                    ..new.clone()
                })),
                cause,
            );
            return;
        }
        (Some(old), None) => {
            queue_prop_change(
                pending,
                prefix.into(),
                PropValue::WindowRow(Box::new(WindowSnapshot {
                    presentation: None,
                    ..old.clone()
                })),
                PropValue::null(),
                cause,
            );
            return;
        }
        (Some(old), Some(new)) => (old, new),
    };
    for (leaf, old, new) in [
        ("id", PropValue::U64(old.id), PropValue::U64(new.id)),
        (
            "foreign_id",
            prop_opt_string(old.foreign_id.as_deref()),
            prop_opt_string(new.foreign_id.as_deref()),
        ),
        (
            "title",
            prop_opt_string(old.title.as_deref()),
            prop_opt_string(new.title.as_deref()),
        ),
        (
            "tiled",
            PropValue::Bool(old.tiled),
            PropValue::Bool(new.tiled),
        ),
        (
            "requested_tiled",
            PropValue::Bool(old.requested_tiled),
            PropValue::Bool(new.requested_tiled),
        ),
        (
            "native_requested_tiled",
            PropValue::Bool(old.native_requested_tiled),
            PropValue::Bool(new.native_requested_tiled),
        ),
        (
            "configure_pending",
            PropValue::Bool(old.configure_pending),
            PropValue::Bool(new.configure_pending),
        ),
        (
            "tile_pending_reason",
            prop_opt_string(old.tile_pending_reason),
            prop_opt_string(new.tile_pending_reason),
        ),
        (
            "app_id",
            prop_opt_string(old.app_id.as_deref()),
            prop_opt_string(new.app_id.as_deref()),
        ),
        ("x", PropValue::F32(old.x), PropValue::F32(new.x)),
        ("y", PropValue::F32(old.y), PropValue::F32(new.y)),
        (
            "width",
            PropValue::F32(old.width),
            PropValue::F32(new.width),
        ),
        (
            "height",
            PropValue::F32(old.height),
            PropValue::F32(new.height),
        ),
        (
            "focused",
            PropValue::Bool(old.focused),
            PropValue::Bool(new.focused),
        ),
        (
            "maximized",
            PropValue::Bool(old.maximized),
            PropValue::Bool(new.maximized),
        ),
        (
            "fullscreen",
            PropValue::Bool(old.fullscreen),
            PropValue::Bool(new.fullscreen),
        ),
        (
            "minimized",
            PropValue::Bool(old.minimized),
            PropValue::Bool(new.minimized),
        ),
        (
            "output",
            prop_opt_string(old.output.as_deref()),
            prop_opt_string(new.output.as_deref()),
        ),
        (
            "band",
            PropValue::String(old.band.into()),
            PropValue::String(new.band.into()),
        ),
        (
            "generation",
            PropValue::U64(old.generation),
            PropValue::U64(new.generation),
        ),
        (
            "window_x",
            PropValue::F32(old.window_x),
            PropValue::F32(new.window_x),
        ),
        (
            "window_y",
            PropValue::F32(old.window_y),
            PropValue::F32(new.window_y),
        ),
        (
            "window_width",
            PropValue::F32(old.window_width),
            PropValue::F32(new.window_width),
        ),
        (
            "window_height",
            PropValue::F32(old.window_height),
            PropValue::F32(new.window_height),
        ),
        (
            "visible",
            PropValue::Bool(old.visible),
            PropValue::Bool(new.visible),
        ),
        ("pid", prop_opt_u64(old.pid), prop_opt_u64(new.pid)),
        (
            "workspace",
            PropValue::U32(old.workspace),
            PropValue::U32(new.workspace),
        ),
    ] {
        queue_prop_change(pending, format!("{prefix}.{leaf}"), old, new, cause);
    }
}

fn diff_focus(
    prefix: &str,
    old: &FocusSnapshot,
    new: &FocusSnapshot,
    cause: &'static str,
    pending: &mut PendingPropChanges,
) {
    for (leaf, old, new) in [
        (
            "keyboard",
            prop_opt_u64(old.keyboard),
            prop_opt_u64(new.keyboard),
        ),
        (
            "exclusive_latch",
            prop_opt_u64(old.exclusive_latch),
            prop_opt_u64(new.exclusive_latch),
        ),
        (
            "pointer",
            prop_opt_u64(old.pointer),
            prop_opt_u64(new.pointer),
        ),
        (
            "pointer_grab",
            prop_str(old.pointer_grab),
            prop_str(new.pointer_grab),
        ),
        (
            "session_lock",
            prop_str(old.session_lock),
            prop_str(new.session_lock),
        ),
        (
            "window.id",
            prop_opt_u64(old.window.id),
            prop_opt_u64(new.window.id),
        ),
        (
            "window.generation",
            prop_opt_u64(old.window.generation),
            prop_opt_u64(new.window.generation),
        ),
    ] {
        queue_prop_change(pending, format!("{prefix}.{leaf}"), old, new, cause);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn occlusion_counters_never_emit_changes_but_decisions_do() {
        let old = OcclusionProps::default();
        let mut new = old.clone();
        let mut changes = PendingPropChanges::new();
        queue_prop_change(
            &mut changes,
            "occlusion.counters.recomputes".into(),
            PropValue::U64(0),
            PropValue::U64(42),
            "wayland.occlusion",
        );
        diff_occlusion("surfaces.s1", &old, &new, "wayland.occlusion", &mut changes);
        assert!(changes.is_empty());
        new.occluded = true;
        new.occlusion_reason = "opaque-coverage";
        new.occlusion_revision = 2;
        diff_occlusion("surfaces.s1", &old, &new, "wayland.occlusion", &mut changes);
        assert_eq!(changes.len(), 3);
        assert!(changes.contains_key("surfaces.s1.occluded"));
    }
}

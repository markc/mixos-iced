// Each decision resolves its `{id, generation}` target through
// surfaces, reads the window facts the engine keeps (configure state,
// size limits, visibility, geometry, outputs) from small structs, mutates
// only the model (registry flags, workspace state), and returns the
// engine's work as an `Effect` list. The grid snap of a placement and the
// size clamp are engine callbacks (they read the output scale and the
// client's size hints).

//! `comp.window.*` decisions over the registry.

use std::collections::BTreeMap;
use std::hash::Hash;

use serde_json::{Value, json};

use comp_model::reply::ControlReply;
use comp_model::request::{
    PlaceSpec, StatsTarget, WaitSpec, WaitUntil, WindowMatch, WindowOp, WindowState, WorkspaceIndex,
};
use comp_model::snapshot::OutputSnapshot;
use surfaces::{Registry, SurfaceId, SurfaceRecord, SurfaceRole, WindowTargetError};

use crate::Effect;
use crate::workspaces::{self, DefaultOutput, SwitchGates, WorkspaceState};

/// A session lock refuses every `comp.window.*` verb that names or changes
/// a window, and the workspace switch (D12: it changes what is on screen).
/// Source stats and a global stats reset still answer.
pub fn locked_refusal(op: &WindowOp, session_lock: bool) -> Option<ControlReply> {
    let names_window = match op {
        WindowOp::Stats { target, .. } => matches!(target, StatsTarget::Window { .. }),
        WindowOp::StatsReset { target } => {
            matches!(target, Some(StatsTarget::Window { .. }))
        }
        _ => true,
    };
    (names_window && session_lock).then_some(ControlReply::Locked)
}

/// The `{id, generation}` fence every window verb starts with.
pub fn resolve<H: Clone + Eq + Hash>(
    registry: &Registry<H>,
    id: u64,
    generation: u64,
) -> Result<&SurfaceRecord<H>, ControlReply> {
    registry
        .resolve_window_target(id, Some(generation))
        .map_err(|error| ControlReply::WindowTarget { id, error })
}

/// A `comp.window.*` success body from the record.
pub fn window_reply<H>(record: &SurfaceRecord<H>, changed: bool) -> ControlReply {
    ControlReply::Window {
        id: record.id().0,
        generation: record.generation(),
        title: record.title().cloned(),
        app_id: record.app_id().cloned(),
        minimized: record.minimized(),
        changed,
    }
}

/// Merge `extra`'s fields into the object `body`.
pub fn merged(mut body: Value, extra: Value) -> Value {
    if let (Some(body), Value::Object(extra)) = (body.as_object_mut(), extra) {
        body.extend(extra);
    }
    body
}

/// The window facts the engine keeps beside the registry record.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WindowFacts {
    /// Persistent generation-fenced group membership, including suspended or
    /// pending members. Distinct from the native requested/committed flags.
    pub requested_tiled: bool,
    pub native_requested_tiled: bool,
    pub committed_tiled: bool,
    pub tile_pending: bool,
    pub requested_maximized: bool,
    pub requested_fullscreen: bool,
    pub committed_maximized: bool,
    pub committed_fullscreen: bool,
    /// Requested maximise or fullscreen differs from client-committed state.
    /// An xdg ACK alone does not clear this: the acknowledged state must commit.
    pub configure_pending: bool,
    /// The client's size hints (0 = unset).
    pub min_size: (i32, i32),
    pub max_size: (i32, i32),
    /// Effective visibility (`layout.visible`).
    pub visible: bool,
    /// The KMS input gate (`surface_is_input_presentable`).
    pub input_presentable: bool,
    /// Global logical origin of the window geometry.
    pub window_origin: (f32, f32),
    /// The committed window-geometry size.
    pub geometry_size: (i32, i32),
    /// A client move/resize of this window is in progress.
    pub interactive: bool,
    pub fullscreen_output: Option<String>,
    /// A frame shown since this mapping (`until: presented`).
    pub presented_since_map: bool,
}

/// `comp.window.{maximize,unmaximize,fullscreen,unfullscreen}`: the
/// refusals before any configure, and the effects. `outputs` is the
/// `outputs.*` projection (key -> row). The engine still refuses
/// `configure_refused` when the requested state did not stick.
#[allow(clippy::too_many_arguments)]
pub fn set_state<H: Clone + Eq + Hash>(
    registry: &Registry<H>,
    id: u64,
    generation: u64,
    state: WindowState,
    enabled: bool,
    output: Option<&str>,
    facts: &WindowFacts,
    outputs: &BTreeMap<String, OutputSnapshot>,
) -> Result<Vec<Effect>, ControlReply> {
    let record = resolve(registry, id, generation)?;
    let (min, max) = (facts.min_size, facts.max_size);
    if enabled && ((max.0 > 0 && min.0 >= max.0) || (max.1 > 0 && min.1 >= max.1)) {
        return Err(ControlReply::refused(
            "unsupported_state",
            json!({"id": id, "reason": "fixed_size"}),
        ));
    }
    let selected = match output {
        Some(name) => Some(
            outputs
                .iter()
                .find(|(key, row)| key.as_str() == name || row.name == name)
                .map(|(key, _)| key.clone())
                .ok_or_else(|| ControlReply::refused("unknown_output", json!({"output": name})))?,
        ),
        None => None,
    };
    if !matches!(
        record.role(),
        SurfaceRole::Toplevel | SurfaceRole::X11 { .. }
    ) {
        return Err(ControlReply::refused(
            "unsupported_state",
            json!({"id": id}),
        ));
    }
    let id = record.id();
    let mut effects = vec![Effect::MarkDirty {
        id,
        cause: "comp.window",
    }];
    if state == WindowState::Fullscreen && (output.is_some() || !enabled) {
        effects.push(Effect::SetFullscreenOutput {
            id,
            output: selected,
        });
    }
    effects.push(Effect::RequestWindowState { id, state, enabled });
    Ok(effects)
}

/// The state verbs' success body, read after the configure went out.
pub fn state_reply<H>(
    record: &SurfaceRecord<H>,
    changed: bool,
    facts: &WindowFacts,
) -> ControlReply {
    ControlReply::Body(merged(
        window_reply(record, changed).wire_json(),
        json!({
            "maximized": facts.committed_maximized,
            "fullscreen": facts.committed_fullscreen,
            "tiled": facts.committed_tiled,
            "requested_tiled": facts.requested_tiled,
            "native_requested_tiled": facts.native_requested_tiled,
            "tile_pending": facts.tile_pending,
            "configure_pending": facts.configure_pending,
        }),
    ))
}

/// `comp.window.minimize {id, generation}` / `restore {id, generation}`:
/// the minimised flag changes in the registry; the engine runs the
/// visibility funnel. Returns the reply and the effects.
pub fn set_minimized<H: Clone + Eq + Hash>(
    registry: &mut Registry<H>,
    id: u64,
    generation: u64,
    minimized: bool,
) -> Result<(ControlReply, Vec<Effect>), ControlReply> {
    let record = resolve(registry, id, generation)?;
    let sid = record.id();
    let before = record.minimized();
    let mut effects = vec![Effect::MarkDirty {
        id: sid,
        cause: "comp.window",
    }];
    if minimized != before {
        effects.push(if minimized {
            Effect::Minimize(sid)
        } else {
            Effect::Restore(sid)
        });
        let _ = registry.set_minimized(sid, minimized);
    }
    let record = registry.get(sid).ok_or(ControlReply::WindowTarget {
        id,
        error: WindowTargetError::UnknownWindow,
    })?;
    Ok((window_reply(record, before != minimized), effects))
}

/// `comp.window.restore {}` with nothing minimised to restore.
pub fn nothing_to_restore<H>(registry: &Registry<H>) -> ControlReply
where
    H: Clone + Eq + Hash,
{
    let minimized_count = registry
        .surface_rows()
        .filter(|record| record.mapped() && record.minimized() && record.role().managed_toplevel())
        .count();
    ControlReply::NotFound { minimized_count }
}

/// The scene facts focus decisions read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SceneFacts {
    pub session_lock: bool,
    pub exclusive_layer: bool,
}

/// What `comp.window.focus` decided.
#[derive(Clone, Debug, PartialEq)]
pub struct FocusDecision {
    pub effects: Vec<Effect>,
    /// The gate that held, if one did.
    pub reason: Option<&'static str>,
}

/// `comp.window.focus {id, generation, raise}`. Rule 6 (F1.2): a window on another workspace
/// is brought on screen by switching to its workspace, never pulled
/// across, and only when the workspace-independent rungs would let it take
/// focus: a refused verb must not change the desktop. The engine reads
/// `focused` back afterwards and replies with [`focus_reply`].
#[allow(clippy::too_many_arguments)]
pub fn focus<H: Clone + Eq + Hash>(
    registry: &Registry<H>,
    workspace_state: &mut WorkspaceState,
    default_output: Option<&DefaultOutput>,
    scene: SceneFacts,
    facts: &WindowFacts,
    id: u64,
    generation: u64,
    raise: bool,
) -> Result<FocusDecision, ControlReply> {
    let record = resolve(registry, id, generation)?;
    let sid = record.id();
    let may_focus = !scene.exclusive_layer && !record.minimized() && facts.input_presentable;
    let mut effects = Vec::new();
    let mut switched = false;
    if may_focus {
        effects.push(Effect::MarkDirty {
            id: sid,
            cause: "comp.window",
        });
        let allowed = workspaces::switch_allowed_for(
            record,
            SwitchGates {
                session_lock: scene.session_lock,
                exclusive_layer: scene.exclusive_layer,
                input_presentable: facts.input_presentable,
            },
        );
        if let Some(more) = workspace_state.ensure_shown(registry, default_output, sid, allowed) {
            effects.extend(more);
            switched = true;
        }
    }
    let on_screen = switched
        || (facts.visible
            && workspaces::on_workspace(record, workspace_state.current(default_output)));
    let reason = if scene.exclusive_layer {
        Some("exclusive_layer")
    } else if record.minimized() {
        Some("minimized")
    } else if !facts.input_presentable {
        Some("not_presentable")
    } else if !on_screen {
        Some("not_visible")
    } else {
        None
    };
    if reason.is_none() {
        effects.push(if raise {
            Effect::Activate(sid)
        } else {
            Effect::Focus(sid)
        });
    } else {
        // Nothing changed, so the refusal attributes nothing: the planted
        // mark goes.
        effects.retain(|effect| !matches!(effect, Effect::MarkDirty { .. }));
    }
    Ok(FocusDecision { effects, reason })
}

/// The focus verb's body: `focused` as read back; a `reason` when a gate
/// held, else `refused` when the focus did not take.
pub fn focus_reply(
    id: u64,
    generation: u64,
    focused: bool,
    reason: Option<&'static str>,
) -> ControlReply {
    let mut body = json!({"id": id, "generation": generation, "focused": focused});
    if let Some(reason) = reason.or((!focused).then_some("refused")) {
        body["reason"] = json!(reason);
    }
    ControlReply::Body(body)
}

/// `comp.window.raise`: stacking only, never a bring-into-view path (an
/// off-workspace or minimised window restacks in place). The engine
/// replies with [`raise_reply`] from the z delta.
pub fn raise<H: Clone + Eq + Hash>(
    registry: &Registry<H>,
    id: u64,
    generation: u64,
) -> Result<Vec<Effect>, ControlReply> {
    let sid = resolve(registry, id, generation)?.id();
    Ok(vec![
        Effect::MarkDirty {
            id: sid,
            cause: "comp.window",
        },
        Effect::Raise(sid),
        Effect::RetargetPointer,
    ])
}

pub fn raise_reply(id: u64, generation: u64, raised: bool) -> ControlReply {
    ControlReply::Body(json!({"id": id, "generation": generation, "raised": raised}))
}

/// `comp.window.close` without `force`: the polite close, nothing more.
pub fn close<H: Clone + Eq + Hash>(
    registry: &Registry<H>,
    id: u64,
    generation: u64,
) -> Result<(ControlReply, Vec<Effect>), ControlReply> {
    let sid = resolve(registry, id, generation)?.id();
    Ok((
        ControlReply::Body(json!({"id": id, "generation": generation, "closed": "polite"})),
        vec![Effect::ClosePolite(sid)],
    ))
}

/// `comp.window.close {force}` at admission: refused under the lock or for
/// a bad target; the polite close goes out now; an X11 window (whose
/// client is Xwayland itself) is refused at once, polite close sent.
/// `Ok(effects)` means: start the deadline.
pub fn start_force_close<H: Clone + Eq + Hash>(
    registry: &Registry<H>,
    id: u64,
    generation: u64,
    session_lock: bool,
) -> Result<Vec<Effect>, (ControlReply, Vec<Effect>)> {
    if session_lock {
        return Err((ControlReply::Locked, Vec::new()));
    }
    let record = resolve(registry, id, generation).map_err(|reply| (reply, Vec::new()))?;
    let effects = vec![Effect::ClosePolite(record.id())];
    if record.role() != SurfaceRole::Toplevel {
        return Err((
            ControlReply::refused(
                "still_open",
                json!({
                    "id": id,
                    "generation": generation,
                    "reason": "x11_kill_unsupported",
                    "polite_close_sent": true,
                }),
            ),
            effects,
        ));
    }
    Ok(effects)
}

/// What a force close does at its deadline.
#[derive(Clone, Debug, PartialEq)]
pub enum ForceCloseOutcome {
    /// The same `{id, generation}` is gone: reply [`close_reply`] `gone`.
    Gone,
    /// The lock owns the screen; no kill lands while it is up.
    Locked,
    /// No per-client kill for X11.
    StillOpen(ControlReply),
    /// Kill the client; reply [`kill_reply`].
    Kill { id: SurfaceId, mapped: bool },
}

/// The force-close deadline decision. Gone means unknown or a new
/// generation; an unmapped but alive window is still killed.
pub fn force_close_at_deadline<H: Clone + Eq + Hash>(
    registry: &Registry<H>,
    id: u64,
    generation: u64,
    session_lock: bool,
) -> ForceCloseOutcome {
    if matches!(
        registry.resolve_window_target(id, Some(generation)),
        Err(WindowTargetError::UnknownWindow | WindowTargetError::StaleTarget { .. })
    ) {
        return ForceCloseOutcome::Gone;
    }
    if session_lock {
        return ForceCloseOutcome::Locked;
    }
    let Some(record) = registry.get(SurfaceId(id)) else {
        return ForceCloseOutcome::Gone;
    };
    if record.role() != SurfaceRole::Toplevel {
        return ForceCloseOutcome::StillOpen(ControlReply::refused(
            "still_open",
            json!({"id": id, "generation": generation, "reason": "x11_kill_unsupported"}),
        ));
    }
    ForceCloseOutcome::Kill {
        id: record.id(),
        mapped: record.mapped(),
    }
}

pub fn close_reply(id: u64, generation: u64, closed: &str, waited_ms: u64) -> ControlReply {
    ControlReply::Body(json!({
        "id": id,
        "generation": generation,
        "closed": closed,
        "waited_ms": waited_ms,
    }))
}

/// The killed reply: the client's pid and every managed window it had
/// (ids, sorted), since the kill takes them all.
pub fn kill_reply(
    id: u64,
    generation: u64,
    waited_ms: u64,
    mapped: bool,
    pid: Option<i32>,
    mut windows: Vec<u64>,
) -> ControlReply {
    windows.sort_unstable();
    let ControlReply::Body(mut body) = close_reply(id, generation, "killed", waited_ms) else {
        unreachable!("close_reply builds a body");
    };
    body["window"] = json!(if mapped { "mapped" } else { "unmapped" });
    body["scope"] = json!("client");
    body["pid"] = json!(pid);
    body["windows"] = json!(windows);
    ControlReply::Body(body)
}

/// `comp.window.wait` at admission: an id never handed out would read as
/// `gone` at once, hiding a typo.
pub fn start_wait<H: Clone + Eq + Hash>(
    registry: &Registry<H>,
    spec: &WaitSpec,
) -> Result<(), ControlReply> {
    match spec.window.id {
        Some(id) if !registry.issued(id) => Err(ControlReply::WindowTarget {
            id,
            error: WindowTargetError::UnknownWindow,
        }),
        _ => Ok(()),
    }
}

/// The app id / title filters of a wait.
pub fn names_match<H>(filter: &WindowMatch, record: &SurfaceRecord<H>) -> bool {
    filter
        .app_id
        .as_deref()
        .is_none_or(|app_id| record.app_id().map(|value| &**value) == Some(app_id))
        && filter
            .title
            .as_deref()
            .is_none_or(|title| record.title().map(|value| &**value) == Some(title))
        && filter
            .title_contains
            .as_deref()
            .is_none_or(|needle| record.title().is_some_and(|title| title.contains(needle)))
}

/// How a wait resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitResolution {
    /// `window: null` (`unmapped` / `gone`).
    Null,
    /// `window:` that window's row.
    Window(SurfaceId),
}

/// The wait outcome: `None` while the condition does not hold. Under a
/// session lock only `gone`/`unmapped` of a named id resolve (the read tree
/// hides every window). `facts` gives each window's engine facts.
pub fn wait_outcome<H: Clone + Eq + Hash>(
    registry: &Registry<H>,
    spec: &WaitSpec,
    session_lock: bool,
    current_workspace: u32,
    facts: impl Fn(SurfaceId) -> WindowFacts,
) -> Option<WaitResolution> {
    if session_lock
        && !(spec.window.id.is_some()
            && matches!(spec.until, WaitUntil::Gone | WaitUntil::Unmapped))
    {
        return None;
    }
    let holds = |record: &SurfaceRecord<H>| {
        let facts = facts(record.id());
        match spec.until {
            WaitUntil::Tiled => !record.minimized() && workspaces::on_workspace(record, current_workspace)
                && facts.requested_tiled && facts.native_requested_tiled
                && facts.committed_tiled && !facts.tile_pending && !facts.configure_pending,
            WaitUntil::Untiled => !facts.requested_tiled && !facts.native_requested_tiled
                && !facts.committed_tiled && !facts.configure_pending,
            WaitUntil::Mapped => true,
            WaitUntil::Visible => facts.visible && !record.minimized(),
            WaitUntil::Presented => {
                !record.minimized()
                    && workspaces::on_workspace(record, current_workspace)
                    && facts.presented_since_map
            }
            WaitUntil::Size { width, height } => facts.geometry_size == (width, height),
            WaitUntil::Focused => record.focused(),
            WaitUntil::Maximized => !facts.configure_pending && facts.committed_maximized,
            WaitUntil::Unmaximized => !facts.configure_pending && !facts.committed_maximized,
            WaitUntil::Fullscreen => !facts.configure_pending && facts.committed_fullscreen,
            WaitUntil::Unfullscreen => !facts.configure_pending && !facts.committed_fullscreen,
            WaitUntil::Unmapped | WaitUntil::Gone => false,
        }
    };
    let live = |record: &SurfaceRecord<H>| {
        record.mapped() && record.role().managed_toplevel() && names_match(&spec.window, record)
    };
    if let Some(id) = spec.window.id {
        let record = registry.get(SurfaceId(id)).filter(|record| {
            record.role() != SurfaceRole::Dormant
                && spec
                    .window
                    .generation
                    .is_none_or(|generation| generation == record.generation())
        });
        return match spec.until {
            WaitUntil::Gone => record.is_none().then_some(WaitResolution::Null),
            WaitUntil::Unmapped => record
                .is_none_or(|record| !record.mapped())
                .then_some(WaitResolution::Null),
            _ => record
                .filter(|record| live(record) && holds(record))
                .map(|record| WaitResolution::Window(record.id())),
        };
    }
    match spec.until {
        WaitUntil::Gone | WaitUntil::Unmapped => registry
            .surface_rows()
            .all(|record| !live(record))
            .then_some(WaitResolution::Null),
        _ => registry
            .surface_rows()
            .filter(|record| live(record) && holds(record))
            .min_by_key(|record| record.id().0)
            .map(|record| WaitResolution::Window(record.id())),
    }
}

/// A resolved wait's body; `window` is the row (or null).
pub fn wait_reply(spec: &WaitSpec, window: Value, waited_ms: u64) -> ControlReply {
    ControlReply::Body(json!({
        "window": window,
        "until": spec.until.name(),
        "waited_ms": waited_ms,
    }))
}

/// What a placement resolved to: the effects plus what the reply needs.
#[derive(Clone, Debug, PartialEq)]
pub struct Placement {
    pub effects: Vec<Effect>,
    /// The `outputs.*` key placed on, and its origin.
    pub output: String,
    pub output_origin: (i32, i32),
    /// The size asked for, if a width or height was given (clamped).
    pub requested: Option<(i32, i32)>,
}

/// The output a place addresses: the named one (by key or name), else the
/// window's own, else the default.
fn place_output<'a>(
    outputs: &'a BTreeMap<String, OutputSnapshot>,
    requested: Option<&str>,
    window_output: Option<&str>,
    default_output: Option<&str>,
) -> Result<(&'a String, &'a OutputSnapshot), ControlReply> {
    if let Some(requested) = requested {
        return outputs
            .iter()
            .find(|(key, row)| key.as_str() == requested || row.name == requested)
            .ok_or_else(|| ControlReply::refused("unknown_output", json!({"output": requested})));
    }
    window_output
        .into_iter()
        .chain(default_output)
        .find_map(|key| outputs.get_key_value(key))
        .ok_or_else(|| ControlReply::refused("unknown_output", json!({"output": null})))
}

/// `comp.window.place`. An absent coordinate
/// keeps the window's offset within its output (the old one, when the
/// place changes outputs); an absent axis keeps the size the window really
/// has. `clamp` applies the client's size hints and work area; `snap` puts
/// the origin on the physical pixel grid. A window placed wholly off every
/// output is refused `off_output`. A maximised or fullscreen window is
/// refused `invalid_state`.
#[allow(clippy::too_many_arguments)]
pub fn place<H: Clone + Eq + Hash>(
    registry: &Registry<H>,
    spec: &PlaceSpec,
    facts: &WindowFacts,
    outputs: &BTreeMap<String, OutputSnapshot>,
    window_output: Option<&str>,
    default_output: Option<&str>,
    clamp: impl Fn((i32, i32)) -> (i32, i32),
    snap: impl Fn((f32, f32)) -> (f32, f32),
) -> Result<Placement, ControlReply> {
    let record = resolve(registry, spec.id, spec.generation)?;
    let sid = record.id();
    let maximized = facts.committed_maximized || facts.requested_maximized;
    let fullscreen = facts.committed_fullscreen || facts.requested_fullscreen;
    let tiled = facts.requested_tiled || facts.native_requested_tiled || facts.committed_tiled;
    if maximized || fullscreen || tiled {
        return Err(ControlReply::refused(
            "invalid_state",
            json!({"id": spec.id, "maximized": maximized, "fullscreen": fullscreen, "tiled": tiled}),
        ));
    }
    let origin = facts.window_origin;
    let current = facts.geometry_size;
    let (key, row) = place_output(
        outputs,
        spec.output.as_deref(),
        window_output,
        default_output,
    )?;
    let (old_x, old_y) = place_output(outputs, None, window_output, default_output)
        .map_or((row.x as f32, row.y as f32), |(_, old)| {
            (old.x as f32, old.y as f32)
        });
    let target = (
        row.x as f32 + spec.x.map_or(origin.0 - old_x, |x| x as f32),
        row.y as f32 + spec.y.map_or(origin.1 - old_y, |y| y as f32),
    );
    let requested = (spec.width.is_some() || spec.height.is_some()).then(|| {
        clamp((
            spec.width.unwrap_or(current.0),
            spec.height.unwrap_or(current.1),
        ))
    });
    let size = requested.unwrap_or(current);
    let visible_at = |target: (f32, f32)| {
        outputs.values().any(|output| {
            target.0 < (output.x as f32 + output.width as f32)
                && target.0 + size.0 as f32 > output.x as f32
                && target.1 < (output.y as f32 + output.height as f32)
                && target.1 + size.1 as f32 > output.y as f32
        })
    };
    let target = snap(target);
    if !visible_at(target) {
        return Err(ControlReply::refused(
            "off_output",
            json!({
                "id": spec.id,
                "x": target.0 - row.x as f32,
                "y": target.1 - row.y as f32,
                "width": size.0,
                "height": size.1,
            }),
        ));
    }
    let mut effects = Vec::new();
    // A client move/resize in progress would steer the window straight back.
    if facts.interactive {
        effects.push(Effect::FinishInteractive(sid));
    }
    effects.push(match requested {
        Some((width, height)) => Effect::Resize {
            id: sid,
            x: target.0,
            y: target.1,
            width,
            height,
        },
        None => Effect::MoveTo {
            id: sid,
            x: target.0,
            y: target.1,
        },
    });
    effects.push(Effect::RetargetPointer);
    Ok(Placement {
        effects,
        output: key.clone(),
        output_origin: (row.x, row.y),
        requested,
    })
}

/// The place verb's body, from where the window actually stands.
pub fn place_reply(
    spec: &PlaceSpec,
    placement: &Placement,
    placed_origin: (f32, f32),
    configure_pending: bool,
) -> ControlReply {
    ControlReply::Body(json!({
        "id": spec.id,
        "generation": spec.generation,
        "output": placement.output,
        "window_x": placed_origin.0 - placement.output_origin.0 as f32,
        "window_y": placed_origin.1 - placement.output_origin.1 as f32,
        "requested": placement
            .requested
            .map(|(width, height)| json!({"width": width, "height": height})),
        "configure_pending": configure_pending,
    }))
}

/// `comp.window.send_to_workspace`: the
/// fence, then the move; with `follow` (and the switch gate admitting the
/// window) the move and the switch are one settle, then the window is
/// activated. `followed` is read back: the window's workspace is current.
#[allow(clippy::too_many_arguments)]
pub fn send_to_workspace<H: Clone + Eq + Hash>(
    registry: &mut Registry<H>,
    workspace_state: &mut WorkspaceState,
    default_output: Option<&DefaultOutput>,
    gates: SwitchGates,
    id: u64,
    generation: u64,
    index: WorkspaceIndex,
    follow: bool,
) -> Result<(ControlReply, Vec<Effect>), ControlReply> {
    let record = resolve(registry, id, generation)?;
    let sid = record.id();
    let follow_now = follow && workspaces::switch_allowed_for(record, gates).is_some();
    let moved = if follow_now {
        workspace_state.move_and_follow(registry, default_output, sid, index.into(), true)
    } else {
        workspace_state.move_window(registry, default_output, sid, index.into(), None)
    };
    let ((_, to), mut effects) =
        moved.map_err(|refusal| workspaces::workspace_refusal(refusal, None, id))?;
    let mut body = json!({"id": id, "generation": generation, "index": to});
    if follow {
        let followed = workspace_state.current(default_output) == to;
        if followed {
            effects.push(Effect::Activate(sid));
        }
        body["followed"] = json!(followed);
    }
    Ok((ControlReply::Body(body), effects))
}

/// The content-source fence of `comp.window.stats {source}`: an unknown
/// source, or a registration that is not the current one.
pub fn source_target(
    id: &str,
    registration: Option<u64>,
    current: Option<u64>,
) -> Result<u64, ControlReply> {
    let Some(current) = current else {
        return Err(ControlReply::refused(
            "unknown_source",
            json!({"source": id}),
        ));
    };
    if let Some(requested) = registration
        && requested != current
    {
        return Err(ControlReply::refused(
            "stale_target",
            json!({"source": id, "registration": requested, "current": current}),
        ));
    }
    Ok(current)
}

#[cfg(test)]
#[path = "window_tests.rs"]
mod tests;

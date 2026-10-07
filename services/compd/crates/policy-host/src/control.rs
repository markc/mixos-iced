//! The comp control verbs compd answers, and the executor for the policy
//! effects they return.
//!
//! - `comp.workspace.switch`, `comp.window.send_to_workspace`;
//! - `comp.window.{maximize,unmaximize,fullscreen,unfullscreen}`,
//!   `minimize`, `restore` (named, or `{}` = rule 8's LIFO pick), `close`
//!   (polite); `close {force}` is a long verb (compd's waiter, kill via
//!   [`kill_client`]);
//! - `props.set` of `windows.s<id>.{minimized,maximized,fullscreen}`;
//! - the client's own maximise / minimise requests ([`apply_client_requests`]);
//! - `comp.window.place` (move and/or resize, output-relative) and the
//!   interactive move/resize grab ([`apply_interactive`]);
//! - `props.set` of `workspaces.count`, `workspaces.current`,
//!   `workspaces.o_<key>.current` and `windows.s<id>.workspace` (the same
//!   replies as the verbs).
//!
//! - `props.set windows.s<id>.band` (`bottom` / `normal`) and the window
//!   stats verbs.
//!
//! A session lock answers `locked` for every verb that names or changes a
//! window, and for the window and workspace sets.
//!
//! The effects: visibility is not stored but derived (`CompState::hidden`,
//! read by the draw through `DrawWindow::visible`), so `Withdraw` /
//! `Present` / `Relabelled` only need a frame; `Settle` re-arbitrates the
//! keyboard; `Activate` queues the engine's activation. The X11 halves
//! (`_NET_*_DESKTOP`, `_NET_WM_STATE_HIDDEN`) are [`crate::x11`]'s.

use serde_json::Value;
use smithay::desktop::Window;
use smithay::utils::SERIAL_COUNTER;

use comp_model::observation::{
    HOST_PASSTHROUGH_PATH, PropValue, WORKSPACE_COUNT_RANGE, WORKSPACE_INDEX_RANGE, WORKSPACE_OUTPUT_RANGE,
    WorkspacesSetTarget, invalid_value, parse_window_leaf_path, parse_workspaces_set_path,
    apply_corner_value, read_only_or_unknown, validate_corner_value, workspace_value,
};
use comp_model::reply::ControlReply;
use comp_model::request::{WindowOp, WindowState};
use comp_model::snapshot::output_key;
use dispatcher::state::state::RedrawReason;
use dispatcher::wire::trait_::surface_event::SurfaceHandle;
use dispatcher::wire::trait_::wire_trait::{ActivationOrigin, WireTrait};
use policy::Effect;
use policy::window::WindowFacts;
use world::camera::transform::translate::slot;
use world::comp::MaximizeRestore;
use world::comp::usable::Reserved;
use protocols::window::shell::shell;
use dispatcher::wire::trait_::surface_event::WindowRequest;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::backend::protocol::ProtocolError;
use smithay::utils::{Logical, Point, Rectangle, Size};
use smithay::wayland::compositor::with_states;
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::shell::xdg::SurfaceCachedState;
use policy::workspaces::{DefaultOutput, SwitchGates, WorkspaceRefusal, WorkspaceTarget};
use surfaces::{SurfaceId, WindowTargetError};
use world::state::Loop;
use world::window::interface::draw::visible::DrawWindow;

/// An output's usable area: its
/// `geometry` (host Space, logical) less what layer-shell exclusive zones
/// reserve, as smithay's layer arrangement computes it (anchors, margins, and
/// several layers on one edge stacking). An output with no layers is whole. A
/// concealed panel keeps its zone: enforcement hides and excludes an enforced layer
/// but leaves it in the layer map, so the area it reserves stays reserved.
/// `reserved` (docked Mix Scenes panels) comes off what the layers
/// leave, stacking as a further layer on that edge would.
pub fn usable_area(
    output: &smithay::output::Output,
    geometry: Rectangle<i32, Logical>,
    reserved: Reserved,
) -> Rectangle<i32, Logical> {
    let map = smithay::desktop::layer_map_for_output(output);
    let layered = if map.layers().next().is_none() {
        geometry
    } else {
        let zone = map.non_exclusive_zone();
        Rectangle::new(geometry.loc + zone.loc, Size::from((zone.size.w.max(0), zone.size.h.max(0))))
    };
    reserved.shrink(layered)
}

/// What docked scene panels reserve on `output` (none when unset).
pub fn reserved_for(lp: &Loop, output: &smithay::output::Output) -> Reserved {
    lp.inner.comp.reserved.get(&output.name()).copied().unwrap_or_default()
}

/// Refresh CompState's usable areas. When one moved (a
/// panel mapped, unmapped or resized its zone, an output changed), the
/// windows maximised by request take the new area.
pub fn refresh_usable(lp: &mut Loop) {
    refresh_output_generations(lp);
    let reserved = &lp.inner.comp.reserved;
    let space = &lp.inner.host_space().state;
    let usable: std::collections::BTreeMap<String, Rectangle<i32, Logical>> = space
        .outputs()
        .filter_map(|output| {
            let edges = reserved.get(&output.name()).copied().unwrap_or_default();
            Some((output.name(), usable_area(output, space.output_geometry(output)?, edges)))
        })
        .collect();
    if usable == lp.inner.comp.usable {
        return;
    }
    let previous = std::mem::replace(&mut lp.inner.comp.usable, usable);
    lp.inner.comp.outputs_changed();
    for id in lp.inner.comp.maximized_ids() {
        if let Some(restore) = lp.inner.comp.maximize_restore(id)
            && (previous.get(&restore.output) != lp.inner.comp.usable.get(&restore.output)
                || !lp.inner.comp.usable.contains_key(&restore.output)) {
            maximize(lp, id, true);
        }
    }
}

/// Refresh each output's generation: its place in the
/// host Space, its mode and its scale are the signature; a change, or a
/// return after removal, is a new generation.
fn refresh_output_generations(lp: &mut Loop) {
    let space = &lp.inner.host_space().state;
    let present: std::collections::BTreeMap<String, world::comp::OutputSignature> = space
        .outputs()
        .filter_map(|output| {
            let geometry = space.output_geometry(output)?;
            let mode = output.current_mode().map_or((0, 0), |mode| (mode.size.w, mode.size.h));
            Some((
                output.name(),
                (
                    geometry.loc.x,
                    geometry.loc.y,
                    geometry.size.w,
                    geometry.size.h,
                    mode.0,
                    mode.1,
                    output.current_scale().fractional_scale(),
                ),
            ))
        })
        .collect();
    lp.inner.comp.observe_outputs(&present);
    for output in lp.inner.host_space().state.outputs() {
        let generation = lp.inner.comp.output_generation(&output.name());
        let data = output.user_data();
        data.insert_if_missing(|| {
            comp_model::capture::OutputGeneration(std::sync::atomic::AtomicU64::new(generation),std::sync::Mutex::new(screencopy::file::output_signature(output)))
        });
        data.get::<comp_model::capture::OutputGeneration>()
            .expect("inserted generation")
            .0
            .store(generation, std::sync::atomic::Ordering::Relaxed);
        *data.get::<comp_model::capture::OutputGeneration>().expect("inserted generation").1.lock().expect("output signature")=screencopy::file::output_signature(output);
    }
}

/// Refresh the default output (the host Space's first output) the workspace
/// model keys its current workspace on. Called before each Bus pass.
pub fn refresh_default_output(lp: &mut Loop) {
    let default = lp.inner.host_space().state.outputs().next().map(|output| {
        let name = output.name();
        DefaultOutput {
            key: output_key(&name),
            name,
        }
    });
    lp.inner.comp.default_output = default;
}

/// A `comp.window.*` / `comp.workspace.*` verb, or `None` when compd does not
/// answer it yet (the caller replies `busy`).
pub fn window(lp: &mut Loop, op: &WindowOp) -> Option<ControlReply> {
    let reply = window_op(lp, op);
    // What a window verb changed carries `comp.window` as its cause.
    if let Some(id) = named_window(op) {
        lp.inner.comp.causes.note_window(id, "comp.window");
    }
    reply
}

/// The window a verb names, if any.
fn named_window(op: &WindowOp) -> Option<u64> {
    match op {
        WindowOp::State { id, .. }
        | WindowOp::Minimize { id, .. }
        | WindowOp::Focus { id, .. }
        | WindowOp::Raise { id, .. }
        | WindowOp::Close { id, .. }
        | WindowOp::SendToWorkspace { id, .. } => Some(*id),
        WindowOp::Restore { target } => target.map(|(id, _)| id),
        WindowOp::Place(spec) => Some(spec.id),
        _ => None,
    }
}

fn window_op(lp: &mut Loop, op: &WindowOp) -> Option<ControlReply> {
    // A session lock refuses every verb that names or changes a window, and
    // the workspace switch.
    if let Some(locked) = policy::window::locked_refusal(op, world::comp::session_lock::active(lp)) {
        return Some(locked);
    }
    match op {
        WindowOp::SwitchWorkspace {
            output,
            index,
            wrap,
        } => {
            let comp = &mut lp.inner.comp;
            let default_output = comp.default_output.clone();
            let (reply, effects) = policy::workspaces::service_switch(
                &mut comp.workspaces,
                &comp.registry,
                default_output.as_ref(),
                output.as_deref(),
                *index,
                *wrap,
            );
            execute(lp, effects);
            Some(reply)
        }
        WindowOp::SendToWorkspace {
            id,
            generation,
            index,
            follow,
        } => {
            let locked = world::comp::session_lock::active(lp);
            let latched = world::comp::latch::active(lp);
            let comp = &mut lp.inner.comp;
            let default_output = comp.default_output.clone();
            let gates = SwitchGates {
                session_lock: locked,
                exclusive_layer: latched,
                input_presentable: true,
            };
            let outcome = policy::window::send_to_workspace(
                &mut comp.registry,
                &mut comp.workspaces,
                default_output.as_ref(),
                gates,
                *id,
                *generation,
                *index,
                *follow,
            );
            Some(match outcome {
                Ok((reply, effects)) => {
                    execute(lp, effects);
                    reply
                }
                Err(refusal) => refusal,
            })
        }
        WindowOp::State {
            id,
            generation,
            state,
            enabled,
            output,
        } => Some(match window_state(lp, *id, *generation, *state, *enabled, output.as_deref()) {
            Ok((record_id, before, after)) => {
                let facts = window_facts(lp, record_id);
                let registry = &lp.inner.comp.registry;
                match registry.get(record_id) {
                    Some(record) => policy::window::state_reply(record, before != after, &facts),
                    None => ControlReply::WindowTarget {
                        id: *id,
                        error: WindowTargetError::UnknownWindow,
                    },
                }
            }
            Err(reply) => reply,
        }),
        WindowOp::Minimize { id, generation } => Some(minimized(lp, *id, *generation, true)),
        WindowOp::Restore {
            target: Some((id, generation)),
        } => Some(minimized(lp, *id, *generation, false)),
        WindowOp::Restore { target: None } => {
            let comp = &lp.inner.comp;
            match comp.lifo_restore_candidate().and_then(|id| comp.registry.get(id)) {
                Some(record) => {
                    let (id, generation) = (record.id().0, record.generation());
                    Some(minimized(lp, id, generation, false))
                }
                None => Some(policy::window::nothing_to_restore(&comp.registry)),
            }
        }
        WindowOp::Place(spec) => Some(place(lp, spec)),
        WindowOp::Focus { id, generation, raise } => Some(focus(lp, *id, *generation, *raise)),
        WindowOp::Raise { id, generation } => Some(raise(lp, *id, *generation)),
        WindowOp::Close { id, generation } => {
            Some(match policy::window::close(&lp.inner.comp.registry, *id, *generation) {
                Ok((reply, effects)) => {
                    execute(lp, effects);
                    reply
                }
                Err(reply) => reply,
            })
        }
        WindowOp::Stats { target, samples } => Some(stats(lp, target, *samples)),
        WindowOp::StatsReset { target } => Some(stats_reset(lp, target.as_ref())),
        // Every WindowOp is served; no catch-all, so a new
        // variant fails to compile here instead of answering busy silently.
    }
}

/// `body` with `extra`'s keys added.
fn merged(mut body: Value, extra: Value) -> Value {
    if let (Some(body), Value::Object(extra)) = (body.as_object_mut(), extra) {
        body.extend(extra);
    }
    body
}

/// `comp.window.stats`: the window's or content
/// source's leaves plus the newest `samples` of each ring.
fn stats(lp: &Loop, target: &comp_model::request::StatsTarget, samples: usize) -> ControlReply {
    use comp_model::request::StatsTarget;
    match target {
        StatsTarget::Window { id, generation } => {
            if let Err(error) = lp.inner.comp.registry.resolve_window_target(*id, Some(*generation)) {
                return ControlReply::WindowTarget { id: *id, error };
            }
            let stats = &lp.inner.comp.presentation.stats;
            let empty = ledger::presentation_stats::PresentationStats::new(stats.epoch_us);
            let window = stats.window(*id, *generation).unwrap_or(&empty);
            ControlReply::Body(merged(
                merged(serde_json::json!({"id": id, "generation": generation}), window.leaves().to_json()),
                window.samples(samples),
            ))
        }
        StatsTarget::Source { id, registration } => {
            let counters = match source_target(lp, id, *registration) {
                Ok(counters) => counters,
                Err(reply) => return reply,
            };
            ControlReply::Body(merged(
                merged(
                    serde_json::json!({
                        "source": id,
                        "registration": counters.registration,
                        "output": crate::project::source_output_name(lp, counters.output.as_deref()),
                        "registered_at_us": counters.registered_at_us,
                        "revision": counters.revision,
                    }),
                    serde_json::to_value(counters.leaves()).unwrap_or(serde_json::Value::Null),
                ),
                counters.samples(samples),
            ))
        }
    }
}

/// A registered source, at `registration` when given
/// (a re-registered id is a different source); policy's fence decides.
fn source_target<'a>(
    lp: &'a Loop,
    id: &str,
    registration: Option<u64>,
) -> Result<&'a ledger::presentation::SourceCounters, ControlReply> {
    let counters = lp.inner.comp.presentation.sources.get(id);
    policy::window::source_target(id, registration, counters.map(|counters| counters.registration))?;
    counters.ok_or_else(|| ControlReply::refused("unknown_source", serde_json::json!({"source": id})))
}

/// `comp.window.stats.reset`: one window, or
/// (no target) every window and output. Counting restarts now.
fn stats_reset(lp: &mut Loop, target: Option<&comp_model::request::StatsTarget>) -> ControlReply {
    use comp_model::request::StatsTarget;
    let now = world::comp::injection::monotonic_us();
    match target {
        None => {
            lp.inner.comp.presentation.stats.reset_all(now);
            lp.inner.comp.presentation.sources.reset_all(now);
            ControlReply::Body(serde_json::json!({"reset": "all", "since_us": now}))
        }
        Some(StatsTarget::Window { id, generation }) => {
            if let Err(error) = lp.inner.comp.registry.resolve_window_target(*id, Some(*generation)) {
                return ControlReply::WindowTarget { id: *id, error };
            }
            lp.inner.comp.presentation.stats.reset_window(*id, *generation, now);
            ControlReply::Body(serde_json::json!({
                "reset": "window",
                "id": id,
                "generation": generation,
                "since_us": now,
            }))
        }
        Some(StatsTarget::Source { id, registration }) => {
            let registration = match source_target(lp, id, *registration) {
                Ok(counters) => counters.registration,
                Err(reply) => return reply,
            };
            lp.inner.comp.presentation.sources.reset(id, now);
            ControlReply::Body(serde_json::json!({
                "reset": "source",
                "source": id,
                "registration": registration,
                "since_us": now,
            }))
        }
    }
}

/// `comp.window.focus {id, generation, raise}` (comp `service_window_focus`,
/// which the human `{window}` input path needs):
/// policy decides (rule 6 switches to the window's workspace), the
/// effects run, and `focused` is read back from the seat. The engine's
/// activation (the raise) lands at the frame step, so the keyboard is focused
/// at once for the read-back either way.
fn focus(lp: &mut Loop, id: u64, generation: u64, raise: bool) -> ControlReply {
    let sid = match lp.inner.comp.registry.resolve_window_target(id, Some(generation)) {
        Ok(record) => record.id(),
        Err(error) => return ControlReply::WindowTarget { id, error },
    };
    let facts = window_facts(lp, sid);
    let scene = policy::window::SceneFacts {
        session_lock: world::comp::session_lock::active(lp),
        exclusive_layer: world::comp::latch::active(lp),
    };
    let comp = &mut lp.inner.comp;
    let default_output = comp.default_output.clone();
    let decision = policy::window::focus(
        &comp.registry,
        &mut comp.workspaces,
        default_output.as_ref(),
        scene,
        &facts,
        id,
        generation,
        raise,
    );
    let decision = match decision {
        Ok(decision) => decision,
        Err(reply) => return reply,
    };
    let reason = decision.reason;
    execute(lp, decision.effects);
    if reason.is_none() {
        focus_keyboard(lp, sid);
    }
    let focused = lp
        .state
        .seat
        .seat
        .get_keyboard()
        .and_then(|keyboard| keyboard.current_focus())
        .and_then(|surface| lp.inner.comp.id_for_surface(&surface))
        == Some(sid);
    policy::window::focus_reply(id, generation, focused, reason)
}

/// `comp.window.raise`:
/// stacking only, never a bring-into-view path. `raised` is whether the
/// window's place in the draw order moved.
fn raise(lp: &mut Loop, id: u64, generation: u64) -> ControlReply {
    let effects = match policy::window::raise(&lp.inner.comp.registry, id, generation) {
        Ok(effects) => effects,
        Err(reply) => return reply,
    };
    let sid = lp.inner.comp.registry.resolve_window_target(id, Some(generation)).ok().map(|record| record.id());
    let position = |lp: &Loop| {
        let uuid = sid.and_then(|sid| lp.inner.comp.registry.uuid_for(sid))?;
        lp.inner.drawable_order().iter().position(|candidate| *candidate == uuid)
    };
    let before = position(lp);
    execute(lp, effects);
    let after = position(lp);
    policy::window::raise_reply(id, generation, before != after)
}

/// Give the primary seat's keyboard to a window.
fn focus_keyboard(lp: &mut Loop, id: SurfaceId) {
    let Some(window) = window_of(lp, id) else { return };
    let Some(surface) = window.wl_surface().map(|surface| surface.into_owned()) else { return };
    if let Some(keyboard) = lp.state.seat.seat.get_keyboard() {
        keyboard.set_focus(&mut lp.state, Some(surface), SERIAL_COUNTER.next_serial());
    }
}

/// `minimize` / `restore {id, generation}`: the flag through the policy,
/// the LIFO and the visibility funnel through the effects.
fn minimized(lp: &mut Loop, id: u64, generation: u64, minimized: bool) -> ControlReply {
    match policy::window::set_minimized(&mut lp.inner.comp.registry, id, generation, minimized) {
        Ok((reply, effects)) => {
            execute(lp, effects);
            reply
        }
        Err(reply) => reply,
    }
}

/// The policy's refusals, then the request; `Ok`
/// carries the record and the requested state before and after.
fn window_state(
    lp: &mut Loop,
    id: u64,
    generation: u64,
    state: WindowState,
    enabled: bool,
    output: Option<&str>,
) -> Result<(SurfaceId, bool, bool), ControlReply> {
    let record_id = lp
        .inner
        .comp
        .registry
        .resolve_window_target(id, Some(generation))
        .map_err(|error| ControlReply::WindowTarget { id, error })?
        .id();
    let facts = window_facts(lp, record_id);
    let (outputs, _, _) = crate::project::project_outputs(lp);
    let effects = policy::window::set_state(
        &lp.inner.comp.registry,
        id,
        generation,
        state,
        enabled,
        output,
        &facts,
        &outputs,
    )?;
    let before = requested(lp, record_id, state);
    execute(lp, effects);
    let after = requested(lp, record_id, state);
    if after != enabled {
        return Err(ControlReply::refused(
            "unsupported_state",
            serde_json::json!({"id": id, "reason": "configure_refused"}),
        ));
    }
    Ok((record_id, before, after))
}

/// The requested (not yet committed) state: compd's maximise record, the
/// engine's fullscreen record.
fn requested(lp: &Loop, id: SurfaceId, state: WindowState) -> bool {
    match state {
        WindowState::Maximized => lp.inner.comp.maximize_restore(id).is_some(),
        WindowState::Fullscreen => {
            window_of(lp, id).is_some_and(|window| protocols::window::ident::ident::states(&window).fullscreen)
        }
    }
}

/// Whether the client has committed the maximised state (xdg: its last acked
/// configure; X11: `_NET_WM_STATE`).
pub fn committed_maximized(window: &Window) -> bool {
    match (window.toplevel(), window.x11_surface()) {
        (Some(toplevel), _) => toplevel.with_committed_state(|state| {
            state.is_some_and(|state| state.states.contains(xdg_toplevel::State::Maximized))
        }),
        (None, Some(x11)) => x11.is_maximized(),
        (None, None) => false,
    }
}

/// An xdg toplevel's min/max size hints (0 = unset); X11 has none here.
fn size_hints(window: &Window) -> ((i32, i32), (i32, i32)) {
    window
        .toplevel()
        .map(|toplevel| {
            with_states(toplevel.wl_surface(), |states| {
                let mut cached = states.cached_state.get::<SurfaceCachedState>();
                let current = cached.current();
                (
                    (current.min_size.w, current.min_size.h),
                    (current.max_size.w, current.max_size.h),
                )
            })
        })
        .unwrap_or_default()
}

/// A size clamped to the client's hints and to at least 1x1.
fn clamp_size(size: (i32, i32), (min, max): ((i32, i32), (i32, i32))) -> (i32, i32) {
    let axis = |value: i32, min: i32, max: i32| {
        let value = value.max(min.max(1));
        if max > 0 { value.min(max) } else { value }
    };
    (axis(size.0, min.0, max.0), axis(size.1, min.1, max.1))
}

/// `comp.window.place`: policy decides the
/// target (output-relative x/y, clamped size, `off_output` / `invalid_state`
/// refusals); the effects move and/or configure; the reply says where the
/// window stands.
fn place(lp: &mut Loop, spec: &comp_model::request::PlaceSpec) -> ControlReply {
    let id = match lp.inner.comp.registry.resolve_window_target(spec.id, Some(spec.generation)) {
        Ok(record) => record.id(),
        Err(error) => return ControlReply::WindowTarget { id: spec.id, error },
    };
    let facts = window_facts(lp, id);
    let (outputs, _, _) = crate::project::project_outputs(lp);
    let window = window_of(lp, id);
    let hints = window.as_ref().map(size_hints).unwrap_or_default();
    let window_output = window.as_ref().and_then(|window| {
        let space = &lp.inner.host_space().state;
        space
            .outputs_for_element(window)
            .first()
            .map(|output| output_key(&output.name()))
    });
    let default_key = lp.inner.comp.default_output.as_ref().map(|output| output.key.clone());
    let placement = policy::window::place(
        &lp.inner.comp.registry,
        spec,
        &facts,
        &outputs,
        window_output.as_deref(),
        default_key.as_deref(),
        |size| clamp_size(size, hints),
        |point| point,
    );
    let mut placement = match placement {
        Ok(placement) => placement,
        Err(reply) => return reply,
    };
    execute(lp, std::mem::take(&mut placement.effects));
    let placed = window_of(lp, id)
        .and_then(|window| lp.inner.host_space().state.element_location(&window))
        .map_or(facts.window_origin, |origin| (origin.x as f32, origin.y as f32));
    policy::window::place_reply(spec, &placement, placed, placement.requested.is_some())
}

/// Put a window's geometry origin at `location` (and, with `size`, configure
/// that size: one configure, no Resizing state).
fn place_window(lp: &mut Loop, id: SurfaceId, location: Point<i32, Logical>, size: Option<Size<i32, Logical>>) {
    let Some(window) = window_of(lp, id) else { return };
    if let Some(size) = size {
        shell::stage(&window, size, false);
        shell::send(&window);
        slot::set_expected_size(&window, size);
    }
    lp.inner
        .host_space_mut()
        .state
        .map_element(window, location, false);
}

/// Apply the interactive move/resize grab's latest step: a move carries the
/// window by the pointer delta; a
/// resize grows from the grabbed edges, anchored on the opposite ones, clamped
/// to the client's hints, with the `Resizing` state until the button is let go.
pub fn apply_interactive(lp: &mut Loop) {
    let Some(mut grab) = lp.inner.comp.interactive.take() else { return };
    let Some(window) = window_of(lp, grab.id) else { return };
    let (origin, size) = match grab.start {
        Some(start) => start,
        None => {
            let origin = lp
                .inner
                .host_space()
                .state
                .element_location(&window)
                .unwrap_or_default();
            let start = (origin, window.geometry().size);
            grab.start = Some(start);
            start
        }
    };
    if grab.updated {
        grab.updated = false;
        let (dx, dy) = (grab.delta.0.round() as i32, grab.delta.1.round() as i32);
        if grab.edges == 0 {
            lp.inner
                .host_space_mut()
                .state
                .map_element(window.clone(), Point::from((origin.x + dx, origin.y + dy)), false);
        } else {
            let (top, bottom, left, right) = (
                grab.edges & 1 != 0,
                grab.edges & 2 != 0,
                grab.edges & 4 != 0,
                grab.edges & 8 != 0,
            );
            let wanted = (
                size.w + if right { dx } else if left { -dx } else { 0 },
                size.h + if bottom { dy } else if top { -dy } else { 0 },
            );
            let (width, height) = clamp_size(wanted, size_hints(&window));
            let location = Point::from((
                if left { origin.x + size.w - width } else { origin.x },
                if top { origin.y + size.h - height } else { origin.y },
            ));
            let new_size = Size::from((width, height));
            shell::stage(&window, new_size, true);
            shell::send(&window);
            slot::set_expected_size(&window, new_size);
            lp.inner
                .host_space_mut()
                .state
                .map_element(window.clone(), location, false);
            grab.last_size = Some(new_size);
        }
        lp.state.schedule_redraw(RedrawReason::WindowState);
    }
    if grab.ended {
        if grab.edges != 0 {
            shell::unstage_resizing(&window);
            shell::send(&window);
        }
        return;
    }
    lp.inner.comp.interactive = Some(grab);
}

/// The engine facts the window-state policy and replies read.
pub fn window_facts(lp: &Loop, id: SurfaceId) -> WindowFacts {
    let Some(window) = window_of(lp, id) else {
        return WindowFacts::default();
    };
    let requested_maximized = lp.inner.comp.maximize_restore(id).is_some();
    let committed_maximized = committed_maximized(&window);
    let (min_size, max_size) = size_hints(&window);
    let origin = lp
        .inner
        .host_space()
        .state
        .element_location(&window)
        .unwrap_or_default();
    let fullscreen = protocols::window::ident::ident::states(&window).fullscreen;
    let geometry = window.geometry();
    WindowFacts {
        requested_maximized,
        requested_fullscreen: fullscreen,
        committed_maximized,
        committed_fullscreen: fullscreen,
        configure_pending: requested_maximized != committed_maximized,
        min_size,
        max_size,
        visible: drawn(lp, &window),
        input_presentable: true,
        geometry_size: (geometry.size.w, geometry.size.h),
        window_origin: (origin.x as f32, origin.y as f32),
        interactive: lp.inner.comp.interactive.is_some_and(|grab| grab.id == id),
        ..WindowFacts::default()
    }
}

/// A `comp.props.set` compd answers, or `None` (the caller replies `busy`).
pub fn set(lp: &mut Loop, path: &str, value: &Value, generation: Option<u64>) -> Option<ControlReply> {
    let reply = set_leaf(lp, path, value, generation);
    // What an admitted set changed carries `props.set` as its cause: the window's rows
    // for a window leaf, else the leaf itself.
    if let Some(ControlReply::Set { old, new, .. }) = &reply {
        match parse_window_leaf_path(path) {
            Some((window, _)) => lp.inner.comp.causes.note_window(window, "props.set"),
            None => lp.inner.comp.causes.note(path, "props.set"),
        }
        // Every applied Bus set owes a frame, including input/model leaves
        // whose setter has no window-specific redraw side effect. Reads,
        // refusals and unchanged values do not disturb the idle desktop.
        if old != new {
            lp.state.schedule_redraw(RedrawReason::Bus);
        }
    }
    reply
}

fn set_leaf(lp: &mut Loop, path: &str, value: &Value, generation: Option<u64>) -> Option<ControlReply> {
    if let Some((window, leaf)) = parse_window_leaf_path(path) {
        // The fence first, so a stale write never reaches whatever window
        // inherited the id.
        if let Some(generation) = generation
            && let Err(error @ WindowTargetError::StaleTarget { .. }) =
                lp.inner.comp.registry.resolve_window_target(window, Some(generation))
        {
            return Some(ControlReply::WindowTarget { id: window, error });
        }
        // A session lock refuses the window-changing leaves.
        if world::comp::session_lock::active(lp)
            && matches!(leaf, "workspace" | "minimized" | "maximized" | "fullscreen")
        {
            return Some(ControlReply::Locked);
        }
        return match leaf {
            "workspace" => Some(set_window_workspace(lp, path, value, window, generation)),
            "minimized" => Some(set_window_minimized(lp, path, value, window, generation)),
            "maximized" | "fullscreen" => Some(set_window_state(lp, path, value, window, generation, leaf)),
            "band" => Some(set_window_band(lp, path, value, window)),
            _ => Some(ControlReply::Validation(read_only_or_unknown(path))),
        };
    }
    if path == "xwayland.enabled" {
        return Some(set_xwayland_enabled(lp, path, value));
    }
    if generation.is_some() {
        return Some(ControlReply::Validation(invalid_value(
            "generation",
            "absent",
            "generation applies to windows.s<id>.* paths only",
        )));
    }
    if let Some(target) = parse_workspaces_set_path(path) {
        if world::comp::session_lock::active(lp) {
            return Some(ControlReply::Locked);
        }
        return Some(set_workspaces(lp, path, value, target));
    }
    if path == HOST_PASSTHROUGH_PATH {
        return Some(set_host_passthrough(lp, path, value));
    }
    Some(set_corners(lp, path, value))
}

/// `input.host.passthrough` (nested only):
/// `false` stops host pointer and key input reaching the seat, so the host
/// cursor cannot overwrite an injected position. Process-lifetime; the leaf
/// does not exist on kms.
fn set_host_passthrough(lp: &mut Loop, path: &str, value: &Value) -> ControlReply {
    let injection = &mut lp.inner.comp.injection;
    if !injection.host_passthrough_available {
        return ControlReply::Validation(comp_model::observation::SetValidationError::UnknownPath);
    }
    let Some(wanted) = value.as_bool() else {
        return ControlReply::Validation(invalid_value(path, "bool", "true|false"));
    };
    let old = injection.host_passthrough;
    if old != wanted {
        injection.set_host_passthrough(wanted);
        lp.inner.comp.input_changed();
    }
    ControlReply::Set {
        path: path.to_string(),
        old: PropValue::Bool(old),
        new: PropValue::Bool(wanted),
        persisted: None,
    }
}

/// `xwayland.enabled`: the configured
/// value for the next start, persisted on every admitted set; the running
/// Xwayland is untouched. A failed write still changes the configured value
/// and replies `persisted: false` (no rollback).
fn set_xwayland_enabled(lp: &mut Loop, path: &str, value: &Value) -> ControlReply {
    let Some(wanted) = value.as_bool() else {
        return ControlReply::Validation(invalid_value(path, "bool", "true|false"));
    };
    let (old, persisted) = crate::xwayland::set(wanted);
    if old != wanted {
        lp.inner.comp.settings_changed("xwayland", "props.set");
    }
    ControlReply::Set {
        path: path.to_string(),
        old: PropValue::Bool(old),
        new: PropValue::Bool(wanted),
        persisted: Some(persisted),
    }
}

/// `input.corners.*`: every other path lands here and is refused
/// (read-only or unknown).
fn set_corners(lp: &mut Loop, path: &str, value: &Value) -> ControlReply {
    let validated = match validate_corner_value(path, value) {
        Ok(validated) => validated,
        Err(error) => return ControlReply::Validation(error),
    };
    let mut config = lp.inner.comp.corners.config();
    let (old, new) = apply_corner_value(&mut config, validated);
    lp.inner.comp.corners.set_config(config);
    ControlReply::Set {
        path: path.to_string(),
        old,
        new,
        persisted: None,
    }
}

/// `windows.s<id>.band`: `bottom` demotes
/// the window behind every normal window, `normal` restores it. Runtime
/// state only, never persisted.
fn set_window_band(lp: &mut Loop, path: &str, value: &Value, window: u64) -> ControlReply {
    let band = match comp_model::observation::validate_window_band_value(path, value) {
        Ok(band) => band,
        Err(error) => return ControlReply::Validation(error),
    };
    match world::comp::band::set(lp, SurfaceId(window), band) {
        Some((old, new)) => {
            lp.state.schedule_redraw(RedrawReason::Stack);
            // Stacking changed what is under the cursor.
            crate::input::retarget_pointer(lp);
            ControlReply::Set {
                path: path.to_string(),
                old: PropValue::String(old.into()),
                new: PropValue::String(new.into()),
                persisted: None,
            }
        }
        None => missing_window(path),
    }
}

fn missing_window(path: &str) -> ControlReply {
    ControlReply::Validation(invalid_value(path, "existing window id", "a live toplevel window"))
}

/// `windows.s<id>.minimized`.
fn set_window_minimized(
    lp: &mut Loop,
    path: &str,
    value: &Value,
    window: u64,
    generation: Option<u64>,
) -> ControlReply {
    let Some(wanted) = value.as_bool() else {
        return ControlReply::Validation(invalid_value(path, "bool", "true|false"));
    };
    let (id, current_generation, old) = match lp.inner.comp.registry.resolve_window_target(window, generation) {
        Ok(record) => (record.id(), record.generation(), record.minimized()),
        Err(error @ WindowTargetError::StaleTarget { .. }) => {
            return ControlReply::WindowTarget { id: window, error };
        }
        Err(_) => return missing_window(path),
    };
    match policy::window::set_minimized(&mut lp.inner.comp.registry, window, current_generation, wanted) {
        Ok((_, effects)) => execute(lp, effects),
        Err(_) => return missing_window(path),
    }
    let new = lp.inner.comp.registry.get(id).is_some_and(|record| record.minimized());
    ControlReply::Set {
        path: path.to_string(),
        old: PropValue::Bool(old),
        new: PropValue::Bool(new),
        persisted: None,
    }
}

/// `windows.s<id>.{maximized,fullscreen}`.
fn set_window_state(
    lp: &mut Loop,
    path: &str,
    value: &Value,
    window: u64,
    generation: Option<u64>,
    leaf: &str,
) -> ControlReply {
    let Some(enabled) = value.as_bool() else {
        return ControlReply::Validation(invalid_value(path, "bool", "true|false"));
    };
    let current_generation = match lp.inner.comp.registry.resolve_window_target(window, generation) {
        Ok(record) => record.generation(),
        Err(error @ WindowTargetError::StaleTarget { .. }) => {
            return ControlReply::WindowTarget { id: window, error };
        }
        Err(_) => return missing_window(path),
    };
    let state = if leaf == "maximized" {
        WindowState::Maximized
    } else {
        WindowState::Fullscreen
    };
    match window_state(lp, window, current_generation, state, enabled, None) {
        Ok((_, old, new)) => ControlReply::Set {
            path: path.to_string(),
            old: PropValue::Bool(old),
            new: PropValue::Bool(new),
            persisted: None,
        },
        Err(reply) => reply,
    }
}

fn set_window_workspace(
    lp: &mut Loop,
    path: &str,
    value: &Value,
    window: u64,
    generation: Option<u64>,
) -> ControlReply {
    let index = match workspace_value(path, value, WORKSPACE_INDEX_RANGE) {
        Ok(index) => index,
        Err(error) => return ControlReply::Validation(error),
    };
    let comp = &mut lp.inner.comp;
    let id = match comp.registry.resolve_window_target(window, generation) {
        Ok(record) => record.id(),
        Err(error @ WindowTargetError::StaleTarget { .. }) => {
            return ControlReply::WindowTarget { id: window, error };
        }
        Err(_) => return missing_window(path),
    };
    let default_output = comp.default_output.clone();
    let outcome = comp.workspaces.move_window(
        &mut comp.registry,
        default_output.as_ref(),
        id,
        WorkspaceTarget::Index(index),
        None,
    );
    match outcome {
        Ok(((old, new), effects)) => {
            execute(lp, effects);
            ControlReply::Set {
                path: path.to_string(),
                old: PropValue::U32(old),
                new: PropValue::U32(new),
                persisted: None,
            }
        }
        Err(WorkspaceRefusal::NotAWindow) => missing_window(path),
        Err(_) => ControlReply::Validation(invalid_value(path, "integer", WORKSPACE_INDEX_RANGE)),
    }
}

fn set_workspaces(lp: &mut Loop, path: &str, value: &Value, target: WorkspacesSetTarget) -> ControlReply {
    let range = match target {
        WorkspacesSetTarget::Count => WORKSPACE_COUNT_RANGE,
        WorkspacesSetTarget::Current(_) => WORKSPACE_INDEX_RANGE,
    };
    let value = match workspace_value(path, value, range) {
        Ok(value) => value,
        Err(error) => return ControlReply::Validation(error),
    };
    let comp = &mut lp.inner.comp;
    let default_output = comp.default_output.clone();
    let outcome = match &target {
        WorkspacesSetTarget::Count => comp.workspaces.set_count(&mut comp.registry, value),
        WorkspacesSetTarget::Current(key) => comp
            .workspaces
            .switch(
                &comp.registry,
                default_output.as_ref(),
                key.as_deref(),
                WorkspaceTarget::Index(value),
                true,
                None,
            )
            .map(|(switched, effects)| ((switched.from, switched.to), effects)),
    };
    match outcome {
        Ok(((old, new), effects)) => {
            execute(lp, effects);
            ControlReply::Set {
                path: path.to_string(),
                old: PropValue::U32(old),
                new: PropValue::U32(new),
                persisted: None,
            }
        }
        // The key may exist under `outputs.*`; what is refused is a key that
        // is not the DEFAULT output's.
        Err(WorkspaceRefusal::UnknownOutput) => {
            ControlReply::Validation(invalid_value(path, "output key", WORKSPACE_OUTPUT_RANGE))
        }
        Err(_) => ControlReply::Validation(invalid_value(path, "integer", range)),
    }
}

/// Carry out the effects the workspace policy returned.
pub fn execute(lp: &mut Loop, effects: Vec<Effect>) {
    let _span = ledger::frame_trace::span("comp_workspace_effects", 0);
    crate::x11::sync_windows(lp, effects.iter().filter_map(|effect| match effect {
        Effect::Withdraw { id, .. } | Effect::Present { id, .. } | Effect::Relabelled { id, .. } => Some(*id),
        _ => None,
    }));
    let mut moved = false;
    for effect in effects {
        match effect {
            // The X11 half: an X11 window's HIDDEN and `_NET_WM_DESKTOP`
            // follow its workspace (the relabel).
            Effect::Withdraw { .. } | Effect::Present { .. } | Effect::Relabelled { .. } => {
                moved = true;
            }
            Effect::MarkDirty { .. } | Effect::WorkspacesDirty(_) => moved = true,
            Effect::Settle { prefer } => {
                settle(lp, prefer);
                moved = true;
            }
            Effect::Activate(id) => {
                activate(lp, id);
                moved = true;
            }
            Effect::Focus(id) => focus_keyboard(lp, id),
            // Stacking: the draw order (hit-testing follows it) and the Space.
            Effect::Raise(id) => {
                raise_now(lp, id);
            }
            // Stacking or visibility changed what is under the cursor.
            Effect::RetargetPointer => crate::input::retarget_pointer(lp),
            Effect::Minimize(id) => {
                lp.inner.comp.note_minimized(id, true);
                crate::x11::sync_window(lp, id);
                settle(lp, None);
                moved = true;
            }
            Effect::Restore(id) => {
                lp.inner.comp.note_minimized(id, false);
                crate::x11::sync_window(lp, id);
                bring_into_view(lp, id);
                activate(lp, id);
                moved = true;
            }
            Effect::ClosePolite(id) => {
                if let Some(window) = window_of(lp, id) {
                    shell::close(&window);
                }
            }
            Effect::RequestWindowState { id, state, enabled } => {
                match state {
                    WindowState::Maximized => maximize(lp, id, enabled),
                    // Applied now, not queued to the frame hook (the engine's
                    // `fullscreen_request`): the verb reads the requested state
                    // back at once, and a queued request read as refused.
                    WindowState::Fullscreen => {
                        if let Some(window) = window_of(lp, id) {
                            // Already fullscreen and asked again (an `{output}`):
                            // move it there (a re-target); `fullscreen_set` returns early.
                            if enabled && protocols::window::ident::ident::states(&window).fullscreen {
                                world::comp::fullscreen::retarget(lp, &window);
                            } else {
                                world::window::interface::draw::fullscreen::fullscreen_set(lp, window, enabled);
                            }
                        }
                    }
                }
                moved = true;
            }
            Effect::MoveTo { id, x, y } => {
                place_window(lp, id, Point::from((x.round() as i32, y.round() as i32)), None);
                lp.state.schedule_redraw(RedrawReason::WindowState);
            }
            Effect::Resize {
                id,
                x,
                y,
                width,
                height,
            } => {
                place_window(
                    lp,
                    id,
                    Point::from((x.round() as i32, y.round() as i32)),
                    Some(Size::from((width, height))),
                );
                lp.state.schedule_redraw(RedrawReason::WindowState);
            }
            // A place mid-grab: the grab would steer the window straight back.
            Effect::FinishInteractive(id) => {
                if let Some(grab) = lp.inner.comp.interactive.as_mut().filter(|grab| grab.id == id) {
                    grab.ended = true;
                    grab.updated = false;
                    apply_interactive(lp);
                }
            }
            // The output a fullscreen covers: policy names it by its
            // `outputs.*` key; CompState keeps the output's name, which the
            // engine's fullscreen asks `comp::fullscreen::target` for.
            Effect::SetFullscreenOutput { id, output } => {
                let name = output.and_then(|key| {
                    let (rows, _, _) = crate::project::project_outputs(lp);
                    rows.get(&key).map(|row| row.name.clone())
                });
                lp.inner.comp.fullscreen.select(id, name);
            }
            // EWMH desktops: the root pair, and every X11 window's.
            Effect::PublishDesktops => crate::x11::publish_desktops(lp),
            Effect::ResyncAllX11 => crate::x11::sync(lp),
            // The workspace verbs return none of the rest.
            _ => {}
        }
    }
    if moved {
        lp.inner.comp.workspaces_changed();
        lp.state.schedule_redraw(RedrawReason::Workspace);
    }
}

/// The engine `Window` behind a registry record, wherever it lives.
pub(crate) fn window_of(lp: &Loop, id: SurfaceId) -> Option<Window> {
    let handle = lp.inner.comp.registry.get(id)?.handle().clone();
    lp.inner
        .all_world_spaces()
        .iter()
        .flat_map(|space| space.state.elements())
        .find(|window| SurfaceHandle::of_window(window).as_ref() == Some(&handle))
        .cloned()
}

/// Raise `id` now: the draw order (hit-testing follows it), the Space, and
/// an X11 window's X stack.
fn raise_now(lp: &mut Loop, id: SurfaceId) {
    if let Some(uuid) = lp.inner.comp.registry.uuid_for(id) {
        lp.inner.raise_drawable(uuid);
    }
    if let Some(window) = window_of(lp, id) {
        lp.inner.host_space_mut().state.raise_element(&window, false);
    }
    crate::x11::sync_stacking(lp);
    lp.state.schedule_redraw(RedrawReason::WindowState);
}

/// Focus and raise `id`. The raise happens
/// NOW, inside the verb: the engine's activation is queued to
/// the next frame step, and a verb that follows before that frame (a hit test,
/// `comp_control_smoke`'s occluded refusal) must already see the new
/// order. Idempotent with the engine's later activation.
fn activate(lp: &mut Loop, id: SurfaceId) {
    raise_now(lp, id);
    if let Some(window) = window_of(lp, id) {
        lp.inner.request_activation(window, ActivationOrigin::Foreign);
    }
}

/// A restored window on another workspace is brought on screen by switching
/// to it.
fn bring_into_view(lp: &mut Loop, id: SurfaceId) {
    let locked = world::comp::session_lock::active(lp);
    let latched = world::comp::latch::active(lp);
    let comp = &mut lp.inner.comp;
    let default_output = comp.default_output.clone();
    let allowed = comp.registry.get(id).and_then(|record| {
        policy::workspaces::switch_allowed_for(
            record,
            SwitchGates {
                session_lock: locked,
                exclusive_layer: latched,
                input_presentable: true,
            },
        )
    });
    if let Some(effects) = comp.workspaces.ensure_shown(&comp.registry, default_output.as_ref(), id, allowed) {
        execute(lp, effects);
    }
}

/// Maximise on the engine's window: the window takes its owning
/// output's usable area (what layer exclusive zones leave) and
/// goes back to where it was on unmaximise. One configure either way. Called
/// again for a maximised window when the usable area moves. A window with
/// server-side chrome gets the usable area less its chrome, so the titlebar
/// stays on the output.
pub fn maximize(lp: &mut Loop, id: SurfaceId, enabled: bool) {
    let Some(window) = window_of(lp, id) else { return };
    let target = if enabled {
        let restore = lp.inner.comp.maximize_restore(id);
        let space = &lp.inner.host_space().state;
        let Some(output) = restore.as_ref()
            .and_then(|restore| space.outputs().find(|output| output.name() == restore.output).cloned())
            .or_else(|| space.outputs_for_element(&window).first().cloned())
            .or_else(|| space.outputs().next().cloned()) else { return };
        let Some(geometry) = space.output_geometry(&output) else { return };
        let area = decor::window::content_area(&window, usable_area(&output, geometry, reserved_for(lp, &output)));
        if let Some(mut restore) = restore {
            if restore.output != output.name() {
                restore.output = output.name();
                lp.inner.comp.set_maximize_restore(id, Some(restore));
            }
        } else {
            let location = space.element_location(&window).unwrap_or(area.loc);
            let size = window.geometry().size;
            lp.inner
                .comp
                .set_maximize_restore(id, Some(MaximizeRestore { location, size, output: output.name() }));
        }
        area
    } else {
        let Some(restore) = lp.inner.comp.maximize_restore(id) else {
            // Not maximised by request: still answer with a configure.
            shell::send(&window);
            return;
        };
        lp.inner.comp.set_maximize_restore(id, None);
        Rectangle::new(restore.location, restore.size)
    };
    apply_window_geometry(lp, &window, target, enabled);
}

fn apply_window_geometry(lp: &mut Loop, window: &Window, area: Rectangle<i32, Logical>, maximized: bool) {
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
    lp.inner
        .host_space_mut()
        .state
        .map_element(window.clone(), area.loc, false);
}

/// The client's own maximise / minimise requests, through the same policy as
/// the verbs (a refusal still answers the client with a configure).
pub fn apply_client_requests(lp: &mut Loop) {
    for (id, request) in lp.inner.comp.take_requests() {
        let Some((window_id, generation)) = lp
            .inner
            .comp
            .registry
            .get(id)
            .map(|record| (record.id().0, record.generation()))
        else {
            continue;
        };
        match request {
            WindowRequest::Maximize(enabled) => {
                if window_state(lp, window_id, generation, WindowState::Maximized, enabled, None).is_err()
                    && let Some(window) = window_of(lp, id)
                {
                    shell::send(&window);
                }
            }
            WindowRequest::Minimize => {
                let _ = minimized(lp, window_id, generation, true);
            }
            // The chrome's close button: the polite close (xdg `close` /
            // `WM_DELETE_WINDOW`), through the same policy as `comp.window.close`.
            WindowRequest::Close => {
                if let Ok((_, effects)) = policy::window::close(&lp.inner.comp.registry, window_id, generation) {
                    execute(lp, effects);
                }
            }
            // X11 requests, answered through the
            // verbs' own code, so a session lock refuses them (`locked_refusal`)
            // and an exclusive latch refuses the focus. A request for an
            // override-redirect window is ignored (X-2a).
            WindowRequest::Unminimize | WindowRequest::Activate | WindowRequest::Desktop(_) | WindowRequest::Raise
                if !lp.inner.comp.registry.get(id).is_some_and(|record| record.role().managed_toplevel()) => {}
            // An X11 `Reorder::Top` ConfigureRequest: a raise, no focus.
            // Through the raise verb, so the band rules apply.
            WindowRequest::Raise => {
                let _ = window(lp, &WindowOp::Raise { id: window_id, generation });
            }
            // `WM_CHANGE_STATE` NormalState: a restore (switches to the
            // window's workspace when allowed).
            WindowRequest::Unminimize => {
                let _ = window(lp, &WindowOp::Restore { target: Some((window_id, generation)) });
            }
            // `_NET_ACTIVE_WINDOW`: activate (focus with raise; rule 6
            // brings it on screen).
            WindowRequest::Activate => {
                let _ = window(lp, &WindowOp::Focus { id: window_id, generation, raise: true });
            }
            // `_NET_WM_DESKTOP`: a move without a
            // switch. All-desktops (sticky) and an index past the count are
            // refused (decision §8.6.2).
            WindowRequest::Desktop(desktop) => {
                let Some(desktop) = policy::x11::desktop_request_target(desktop, lp.inner.comp.workspaces.count) else {
                    continue;
                };
                let index = comp_model::request::WorkspaceIndex::Absolute(desktop + 1);
                let _ = window(lp, &WindowOp::SendToWorkspace { id: window_id, generation, index, follow: false });
            }
        }
    }
    // `_NET_CURRENT_DESKTOP`: a switch,
    // refused past the count and under a lock.
    for desktop in lp.inner.comp.take_desktop_requests() {
        if desktop >= lp.inner.comp.workspaces.count {
            continue;
        }
        let index = comp_model::request::WorkspaceIndex::Absolute(desktop + 1);
        let _ = window(lp, &WindowOp::SwitchWorkspace { output: None, index, wrap: false });
    }
}

/// The binding chords queued since the last pass, run through the same code as the verbs they
/// mirror. A refusal is a no-op: a key press has nobody to reply to.
pub fn apply_bindings(lp: &mut Loop) {
    use comp_model::request::WorkspaceIndex;
    use policy::bindings::BindingAction;
    for action in lp.inner.comp.bindings.take_pending() {
        match action {
            BindingAction::RequestCloseFocused => {
                if let Some((id, generation)) = focused_window(lp) {
                    let _ = window(lp, &WindowOp::Close { id, generation });
                }
            }
            BindingAction::RestoreMostRecentlyMinimized => {
                let _ = window(lp, &WindowOp::Restore { target: None });
            }
            BindingAction::WorkspaceJump(n) => {
                let index = WorkspaceIndex::Absolute(u32::from(n));
                let _ = window(lp, &WindowOp::SwitchWorkspace { output: None, index, wrap: true });
            }
            BindingAction::WorkspaceStep { prev } => {
                let index = if prev { WorkspaceIndex::Prev } else { WorkspaceIndex::Next };
                let _ = window(lp, &WindowOp::SwitchWorkspace { output: None, index, wrap: true });
            }
            // D18: never under an exclusive layer (the policy's gate
            // refuses the follow there); a refused move changes nothing.
            BindingAction::WorkspaceMove(n) => {
                if let Some((id, generation)) = focused_window(lp)
                    && !crate::input::exclusive_layer(lp)
                {
                    let index = WorkspaceIndex::Absolute(u32::from(n));
                    let _ = window(lp, &WindowOp::SendToWorkspace { id, generation, index, follow: true });
                }
            }
            BindingAction::CycleWindow { reverse } => cycle_window(lp, reverse),
            // Run where they were decided (`world::comp::bindings::key`).
            BindingAction::ToggleInterception
            | BindingAction::SwitchVt(_)
            | BindingAction::ExitNestedCompositor
            | BindingAction::SendBusKey => {}
        }
    }
}

/// The window holding the human keyboard (its root, for a popup), as
/// `{id, generation}`.
fn focused_window(lp: &Loop) -> Option<(u64, u64)> {
    let surface = lp.state.seat.seat.get_keyboard()?.current_focus()?;
    let registry = &lp.inner.comp.registry;
    let mut id = registry.id_for_handle(&SurfaceHandle::resolve(&surface))?;
    for _ in 0..64 {
        match registry.get(id).and_then(|record| record.parent()) {
            Some(parent) => id = parent,
            None => break,
        }
    }
    let record = registry.get(id)?;
    record.role().managed_toplevel().then(|| (record.id().0, record.generation()))
}

/// The mapped, unminimised managed windows on the
/// current workspace in creation order; the one after (or before) the
/// focused one is focused and raised. Nothing under an exclusive layer.
fn cycle_window(lp: &mut Loop, reverse: bool) {
    if crate::input::exclusive_layer(lp) {
        return;
    }
    let comp = &lp.inner.comp;
    let current = comp.current_workspace();
    let mut candidates: Vec<(u64, u64)> = comp
        .registry
        .surface_rows()
        .filter(|record| {
            record.mapped()
                && !record.minimized()
                && record.role().managed_toplevel()
                && record.workspace() == Some(current)
        })
        .map(|record| (record.id().0, record.generation()))
        .collect();
    candidates.sort_unstable_by_key(|(id, _)| *id);
    if candidates.is_empty() {
        return;
    }
    let focused = focused_window(lp);
    let position = candidates.iter().position(|candidate| Some(*candidate) == focused);
    let len = candidates.len();
    let index = match (position, reverse) {
        (Some(index), true) => (index + len - 1) % len,
        (Some(index), false) => (index + 1) % len,
        (None, true) => len - 1,
        (None, false) => 0,
    };
    let (id, generation) = candidates[index];
    let _ = focus(lp, id, generation, true);
}

/// The force-close kill: the window's client goes (every window it had
/// with it). Returns its pid and the ids of its managed windows.
pub fn kill_client(lp: &Loop, id: SurfaceId) -> (Option<i32>, Vec<u64>) {
    let Some(window) = window_of(lp, id) else {
        return (None, Vec::new());
    };
    let Some(client) = window.toplevel().and_then(|toplevel| toplevel.wl_surface().client()) else {
        return (None, Vec::new());
    };
    let display = &lp.inner.loader.display_handle;
    let pid = client.get_credentials(display).ok().map(|credentials| credentials.pid);
    let comp = &lp.inner.comp;
    let windows = lp
        .inner
        .all_world_spaces()
        .iter()
        .flat_map(|space| space.state.elements())
        .filter(|candidate| {
            candidate
                .toplevel()
                .and_then(|toplevel| toplevel.wl_surface().client())
                .is_some_and(|owner| owner.id() == client.id())
        })
        .filter_map(SurfaceHandle::of_window)
        .filter_map(|handle| comp.registry.id_for_handle(&handle))
        .map(|id| id.0)
        .collect();
    client.kill(
        display,
        ProtocolError {
            code: 0,
            object_id: 0,
            object_interface: "wl_display".into(),
            message: "killed by compd: comp.window.close {force} deadline passed".into(),
        },
    );
    (pid, windows)
}

/// The settle, reduced to the keyboard: if the focused window is hidden
/// (or `prefer` names a shown one), focus `prefer`, else the topmost shown
/// managed window on the current workspace, else nothing.
fn settle(lp: &mut Loop, prefer: Option<SurfaceId>) {
    let comp = &lp.inner.comp;
    let shown = |id: SurfaceId| {
        comp.registry
            .get(id)
            .is_some_and(|record| record.mapped() && record.role().managed_toplevel())
            && !comp.hidden_id(id)
    };
    let focused_shown = comp.focused().is_some_and(shown);
    let preferred = prefer.filter(|id| shown(*id));
    if focused_shown && preferred.is_none() {
        return;
    }
    let target = preferred.or_else(|| {
        lp.inner
            .drawable_order()
            .into_iter()
            .filter_map(|uuid| comp.registry.id_for_uuid(uuid))
            .find(|id| shown(*id))
    });
    match target {
        Some(id) => activate(lp, id),
        None => {
            lp.inner.set_activated_exclusive(None);
            if let Some(keyboard) = lp.state.seat.seat.get_keyboard() {
                keyboard.set_focus(&mut lp.state, None, SERIAL_COUNTER.next_serial());
            }
        }
    }
}

/// Whether a window would be drawn now (through the draw's own predicate).
pub fn drawn(lp: &Loop, window: &Window) -> bool {
    window.visible(lp) && protocols::window::ident::ident::is_drawn(window)
}

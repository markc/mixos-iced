//! Quoin's panel holders: the engine half.
//!
//! policy `panel` decides per edge; this feeds it what the engine sees
//! (which layer a token names and whose client it is, where the pointer and
//! the keyboard are, the hot corner the pointer is in, layer acks, presses),
//! carries out what it decides (`panel.command`s, liveness probes as re-sent
//! configures, the conceal marker on enforced layers, focus restoration) and
//! reports the next deadline. The host arms ONE timer at that deadline; with
//! no deadline there is none. Nothing polls.
//!
//! Engine plumbing: an owner's disconnect is seen at the next pass (its
//! client is gone from the display) rather than by a callback, and a panel's
//! layer is re-resolved on every pass rather than only after surface edges.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use serde_json::{Value, json};
use smithay::desktop::layer_map_for_output;
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::backend::ClientId;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::Serial;

use comp_model::observation::PanelRequest;
use comp_model::reply::ControlReply;
use comp_model::snapshot::EdgeCounts;
use dispatcher::state::state::RedrawReason;
use dispatcher::wire::trait_::surface_event::SurfaceHandle;
use policy::panel::{
    Claim, ENFORCE_GRACE, Membership, PROBE_TIMEOUT, PanelOp, PointerHold, PopupRestore, Probe, apply_panel_request,
    panel_claim, resolve_panel_surface,
};
use surfaces::{SurfaceId, SurfaceRole};
use world::comp::panels::set_concealed;
use world::state::Loop;

/// A live layer's mapped state and its Wayland client.
type LayerFacts = (bool, Option<ClientId>);
/// One edge's freshly resolved layer and what that layer may do to it.
type Resolved = ((String, String), Option<SurfaceId>, Option<Claim<ClientId>>);
/// A `panel.command` owed: `(output, edge, surface, reveal)`.
pub type PanelCommand = (String, String, String, bool);

/// `comp.panel.hold` / `comp.panel.mode`:
/// resolve the exact namespace on the named output, fence the incarnation by
/// Wayland client, apply the request, owe the verdict.
pub fn request(lp: &mut Loop, request: &PanelRequest) -> ControlReply {
    if world::comp::session_lock::active(lp) {
        return ControlReply::Locked;
    }
    if !live_outputs(lp).contains(&request.output) {
        return ControlReply::refused("unknown_output", json!({"output": request.output}));
    }
    let id = match resolve_panel_surface(candidates(lp, &request.surface, &request.output)) {
        Ok(id) => id,
        Err(code) => return ControlReply::refused(code, json!({"surface": request.surface})),
    };
    // A concealed panel has no layer; its mode still exists. A release also
    // stays valid after the popup's destruction overtakes its RPC.
    if id.is_none() && request.acquire == Some(true) {
        return ControlReply::refused("unknown_panel_surface", json!({"surface": request.surface}));
    }
    let key = (request.output.clone(), request.edge.clone());
    let (owner, reporter, current_generation) = lp
        .inner
        .comp
        .panels
        .holders
        .get(&key)
        .map_or((None, None, None), |panel| (panel.owner.clone(), panel.reporter.clone(), panel.generation));
    // The holder service is the first registered service to report the edge;
    // only it reports it after that.
    let from_reporter =
        !request.sender.is_empty() && reporter.as_deref().is_none_or(|reporter| reporter == request.sender);
    let reporting = request.mode.is_some() && from_reporter;
    let foreign_report = request.mode.is_some() && !reporting && reporter.is_some();
    // Generations only move forward.
    if reporting
        && let (Some(generation), Some(current)) = (request.generation, current_generation)
        && generation < current
    {
        return ControlReply::refused("stale_generation", json!({"generation": generation, "current": current}));
    }
    let may_adopt = reporting || (from_reporter && reporter.is_some());
    let claim = id.map(|id| claim_for(lp, owner.as_ref(), id, may_adopt));
    match &claim {
        Some(Claim::Refuse) => {
            return ControlReply::refused("panel_owner_mismatch", json!({"surface": request.surface}));
        }
        Some(Claim::Unowned) if request.acquire == Some(true) => {
            return ControlReply::refused("unknown_panel_surface", json!({"surface": request.surface}));
        }
        _ => {}
    }
    let bound = !foreign_report && claim.as_ref().is_some_and(Claim::binds);
    let id = id.filter(|_| bound);
    let panels = &mut lp.inner.comp.panels;
    let recorded = panels.holders.get(&key).map(|panel| (panel.surface.clone(), panel.id));
    if let Some(panel) = panels.holders.get_mut(&key) {
        // A replacement drops the dead incarnation before this request lands.
        if let Some(replace @ Claim::Replace(_)) = claim.clone()
            && bound
        {
            replace.apply(panel);
        }
        // A newer Bus generation of the holder: its predecessor's holds end.
        if reporting
            && let (Some(generation), Some(current)) = (request.generation, panel.generation)
            && generation > current
        {
            panel.drop_holds();
        }
    }
    if request.holder.as_deref() == Some("popup")
        && request.acquire == Some(true)
        && let Some(popup) = id
    {
        record_popup_focus(lp, popup);
    }
    let panels = &mut lp.inner.comp.panels;
    // Resynchronisation: a report lifts the exclusion; a conceal still owed
    // is owed again with a fresh grace.
    let owed = request.mode.is_some() && panels.holders.get(&key).is_some_and(|panel| panel.owed());
    let previous = panels.holders.get(&key).and_then(|panel| panel.verdict);
    let op = PanelOp {
        output: &request.output,
        edge: &request.edge,
        surface: &request.surface,
        holder: request.holder.as_deref(),
        acquire: request.acquire,
        mode: request.mode.as_deref(),
    };
    let verdict = apply_panel_request(&mut panels.holders, op, id);
    if let Some(panel) = panels.holders.get_mut(&key) {
        if foreign_report && let Some((surface, id)) = recorded {
            panel.surface = surface;
            panel.id = id;
        }
        if let Some(adopt @ Claim::Adopt(_)) = claim
            && bound
        {
            adopt.apply(panel);
        }
        if reporting {
            panel.reporter = Some(request.sender.clone());
            panel.reported_surface = Some(request.surface.clone());
            panel.generation = request.generation.or(panel.generation);
        }
        // The holder answered on the Bus: it is not stopped.
        if bound || reporting {
            panel.stalled = false;
        }
        if request.holder.as_deref() == Some("popup")
            && request.acquire == Some(true)
            && let Some(popup) = id
        {
            panel.popups.insert(popup);
        }
        if let Some(reveal) = verdict {
            panel.note_verdict(reveal, previous);
        }
        if owed && panel.hidden() && panel.verdict == Some(false) {
            panel.arm_pending = true;
        }
    }
    if let Some(reveal) = verdict {
        // Commands name the panel's own token when there is one.
        let surface = panels
            .holders
            .get(&key)
            .map_or_else(|| request.surface.clone(), |panel| panel.surface.clone());
        panels.commands.push((request.output.clone(), request.edge.clone(), surface, reveal));
    }
    ControlReply::Body(json!({"accepted": true, "surface": request.surface}))
}

/// One pass at a stable dispatch boundary (the holders and the popup
/// restoration after the focus edge). Returns the next deadline.
pub fn service(lp: &mut Loop, now: Instant) -> Option<Instant> {
    // Layer acks and keyboard focus changes since the last pass.
    let acks = std::mem::take(&mut lp.inner.comp.panels.layer_acks);
    for (id, serial) in acks {
        let client = layer_facts(lp, id).and_then(|(_, client)| client);
        note_layer_ack(lp, id, client, serial, now);
    }
    let changes = std::mem::take(&mut lp.inner.comp.panels.focus_changes);
    for (to, from) in changes {
        note_popup_focus(lp, to, from);
    }
    if lp.inner.comp.panels.holders.is_empty() {
        apply_enforcement(lp);
        service_popup_restores(lp);
        return None;
    }
    // An output that went away takes its edges' modes and holds with it.
    let live = live_outputs(lp);
    lp.inner.comp.panels.holders.retain(|(output, _), _| live.contains(output));
    // The owning Wayland client disconnected: its incarnation ends.
    let gone: Vec<(String, String)> = lp
        .inner
        .comp
        .panels
        .holders
        .iter()
        .filter(|(_, panel)| panel.owner.as_ref().is_some_and(|owner| !client_alive(lp, owner)))
        .map(|(key, _)| key.clone())
        .collect();
    for key in gone {
        if let Some(panel) = lp.inner.comp.panels.holders.get_mut(&key) {
            panel.drop_incarnation();
        }
    }
    // The token only finds the layer: it binds under the edge's incarnation
    // fencing, and an unowned edge is adopted only for the token a registered
    // holder service reported.
    let resolved: Vec<Resolved> = lp
        .inner
        .comp
        .panels
        .holders
        .iter()
        .map(|(key, panel)| {
            let id = resolve_panel_surface(candidates(lp, &panel.surface, &key.0)).ok().flatten();
            let may_adopt = panel.reporter.as_deref().is_some_and(|reporter| !reporter.is_empty())
                && panel.reported_surface.as_deref() == Some(panel.surface.as_str());
            let claim = id.map(|id| claim_for(lp, panel.owner.as_ref(), id, may_adopt));
            (key.clone(), id, claim)
        })
        .collect();
    for (key, id, claim) in resolved {
        if let Some(panel) = lp.inner.comp.panels.holders.get_mut(&key) {
            panel.id = match claim {
                Some(claim) if claim.binds() => {
                    claim.apply(panel);
                    id
                }
                _ => None,
            };
        }
    }
    let deadline = track(lp, now);
    apply_enforcement(lp);
    service_popup_restores(lp);
    deadline
}

/// Observe every panel's membership, emit the
/// verdicts that changed, run the stalled-owner checks and return the
/// earliest deadline.
fn track(lp: &mut Loop, now: Instant) -> Option<Instant> {
    let pointer = lp
        .state
        .seat
        .seat
        .get_pointer()
        .and_then(|pointer| pointer.current_focus())
        .and_then(|surface| lp.inner.comp.id_for_surface(&surface));
    let keyboard = lp
        .state
        .seat
        .seat
        .get_keyboard()
        .and_then(|keyboard| keyboard.current_focus())
        .and_then(|surface| lp.inner.comp.id_for_surface(&surface));
    // The hotspot the pointer is in, by output name; dwelling engages it.
    let corners = &lp.inner.comp.corners;
    let corner = corners
        .output()
        .zip(corners.contact())
        .map(|(output, corner)| (output.to_string(), corner, corners.engaged() == Some(corner)));
    // Every layer the holder state names, looked up once: `None` once gone.
    let ids: BTreeSet<SurfaceId> = lp
        .inner
        .comp
        .panels
        .holders
        .values()
        .flat_map(|panel| {
            panel
                .id
                .into_iter()
                .chain(panel.held.values().map(|(_, id)| *id))
                .chain(panel.popups.iter().copied())
                .chain(panel.pending.iter().copied())
                .chain(panel.enforced.iter().copied())
        })
        .collect();
    let layers: BTreeMap<SurfaceId, Option<LayerFacts>> =
        ids.into_iter().map(|id| (id, layer_facts(lp, id))).collect();
    let alive = |id: &SurfaceId| layers.get(id).is_some_and(Option::is_some);
    let mapped = |id: &SurfaceId| layers.get(id).is_some_and(|facts| facts.as_ref().is_some_and(|(mapped, _)| *mapped));
    let panels = &mut lp.inner.comp.panels;
    let user_input = std::mem::take(&mut panels.user_input);
    let mut commands = Vec::new();
    let mut probes = Vec::new();
    // At most one press-triggered probe per owner in flight.
    let mut probing: Vec<ClientId> = panels
        .holders
        .values()
        .filter(|panel| panel.probe.is_some())
        .filter_map(|panel| panel.owner.clone())
        .collect();
    let mut deadline: Option<Instant> = None;
    for (key, panel) in &mut panels.holders {
        let (output, edge) = key;
        // A popup's closing is its release, whatever Quoin has said yet.
        panel.held.retain(|kind, (_, id)| kind != "popup" || alive(&*id));
        panel.popups.retain(alive);
        // A layer its client unmapped or destroyed is hidden by the client.
        panel.enforced.retain(mapped);
        // A recorded layer unmapped: Quoin applied the conceal.
        if panel.pending.iter().any(|id| !mapped(id)) {
            panel.settle_owed();
        }
        // A probe went unanswered: the owner is stopped.
        if panel.probe.as_ref().is_some_and(|probe| probe.deadline <= now) {
            panel.probe = None;
            panel.stall();
        }
        let on_hotspot = corner
            .as_ref()
            .filter(|(corner_output, corner, _)| corner.summoned_edge() == edge.as_str() && corner_output == output);
        let over = |target: Option<SurfaceId>| {
            target.is_some_and(|target| panel.id == Some(target) || panel.held.values().any(|(_, id)| *id == target))
        };
        let seen = Membership {
            dwelled: on_hotspot.is_some_and(|(_, _, engaged)| *engaged),
            hotspot: on_hotspot.is_some(),
            surface: over(pointer),
            // A stopped owner's keyboard focus is about to be taken away.
            focused: !panel.stalled && over(keyboard),
        };
        panel.observe(seen, now);
        panel.expire(now);
        let previous = panel.verdict;
        if let Some(reveal) = panel.settle(false) {
            panel.note_verdict(reveal, previous);
            commands.push((output.clone(), edge.clone(), panel.surface.clone(), reveal));
        }
        // The owner's layers this edge shows: resolved identities only.
        let owner = panel.owner.clone();
        let showing: BTreeSet<SurfaceId> = panel
            .id
            .into_iter()
            .chain(panel.popups.iter().copied())
            .filter(|id| {
                owner.is_some()
                    && layers
                        .get(id)
                        .is_some_and(|facts| facts.as_ref().is_some_and(|(mapped, client)| *mapped && *client == owner))
            })
            .collect();
        let owes = panel.hidden() && panel.verdict == Some(false);
        if std::mem::take(&mut panel.arm_pending) && owes && !showing.is_empty() {
            panel.pending.clone_from(&showing);
            panel.enforce_at = Some(now + ENFORCE_GRACE);
        }
        if showing.is_empty() {
            panel.quiet = false;
        }
        if !owes {
            panel.pending.clear();
            panel.enforce_at = None;
        } else if panel.stalled && panel.enforced.is_empty() && !showing.is_empty() {
            // A stopped owner's conceal is enforced without another grace.
            panel.settle_owed();
            panel.enforced = showing.clone();
        } else if panel.pending.is_empty()
            && panel.enforce_at.is_none()
            && panel.probe.is_none()
            && panel.enforced.is_empty()
            && !panel.quiet
            && !showing.is_empty()
        {
            // Shown while the verdict says conceal: the same grace, then a probe.
            panel.enforce_at = Some(now + ENFORCE_GRACE);
        }
        if owes && panel.enforce_at.is_some_and(|at| at <= now) {
            panel.enforce_at = None;
            let candidates: Vec<SurfaceId> = if panel.pending.is_empty() {
                showing.iter().copied().collect()
            } else {
                panel.pending.iter().copied().filter(|id| showing.contains(id)).collect()
            };
            if !candidates.is_empty() && panel.probe.is_none() {
                probes.push((key.clone(), candidates));
            }
        }
        // Only a popup or focus hold keeps the edge revealed, and the user
        // pressed somewhere the owner does not own: is the owner still there?
        let only_popup_or_focus = panel.verdict == Some(true)
            && panel.pointer == PointerHold::Out
            && !panel.held.contains_key("pointer")
            && (panel.focused || panel.held.contains_key("popup") || panel.held.contains_key("focus"));
        if only_popup_or_focus
            && panel.probe.is_none()
            && !panel.stalled
            && !showing.is_empty()
            && panel.probe_rest_until.is_none_or(|at| now >= at)
            && user_input.iter().any(|client| *client != owner)
            && let Some(client) = owner.clone()
            && !probing.contains(&client)
        {
            probing.push(client);
            probes.push((key.clone(), showing.iter().copied().collect()));
        }
        let probe_at = panel.probe.as_ref().map(|probe| probe.deadline);
        for at in [panel.conceal_deadline(), panel.enforce_at, probe_at].into_iter().flatten() {
            deadline = Some(deadline.map_or(at, |current| current.min(at)));
        }
    }
    panels.commands.extend(commands);
    for (key, candidates) in probes {
        let serials: Vec<(SurfaceId, Serial)> = candidates
            .into_iter()
            .filter_map(|id| send_probe_configure(lp, id).map(|serial| (id, serial)))
            .collect();
        if serials.is_empty() {
            continue;
        }
        let at = now + PROBE_TIMEOUT;
        if let Some(panel) = lp.inner.comp.panels.holders.get_mut(&key) {
            panel.probe = Some(Probe { deadline: at, serials });
            deadline = Some(deadline.map_or(at, |current| current.min(at)));
        }
    }
    deadline
}

/// Publish the union of every edge's
/// enforced layers to the conceal marker. A stopped owner keeps no keyboard
/// focus on a concealed layer.
fn apply_enforcement(lp: &mut Loop) {
    let panels = &mut lp.inner.comp.panels;
    panels.stalled_owners = panels
        .holders
        .values()
        .filter(|panel| panel.stalled)
        .filter_map(|panel| panel.owner.clone())
        .collect();
    let enforced: BTreeSet<SurfaceId> = panels
        .holders
        .values()
        .flat_map(|panel| panel.enforced.iter().copied())
        .collect();
    if enforced == panels.enforced {
        return;
    }
    let previous = std::mem::replace(&mut panels.enforced, enforced.clone());
    for id in previous.difference(&enforced) {
        if let Some(surface) = surface_of(lp, *id) {
            set_concealed(&surface, false);
        }
    }
    for id in &enforced {
        if let Some(surface) = surface_of(lp, *id) {
            set_concealed(&surface, true);
        }
    }
    if let Some(keyboard) = lp.state.seat.seat.get_keyboard()
        && keyboard
            .current_focus()
            .and_then(|surface| lp.inner.comp.id_for_surface(&surface))
            .is_some_and(|id| enforced.contains(&id))
    {
        keyboard.set_focus(&mut lp.state, None, smithay::utils::SERIAL_COUNTER.next_serial());
    }
    lp.state.schedule_redraw(RedrawReason::Layer);
}

/// An ack at or after a probe's serial answers it;
/// any ack clears a stalled mark on its owner; an owner that answered rests.
fn note_layer_ack(lp: &mut Loop, id: SurfaceId, client: Option<ClientId>, serial: Serial, now: Instant) {
    let panels = &mut lp.inner.comp.panels;
    let mut answered = false;
    for panel in panels.holders.values_mut() {
        if panel
            .probe
            .as_ref()
            .is_some_and(|probe| probe.serials.iter().any(|(probed, sent)| *probed == id && serial >= *sent))
        {
            panel.settle_owed();
            panel.quiet = true;
            answered = true;
        }
        if client.is_some() && panel.owner == client {
            panel.stalled = false;
        }
    }
    if answered && client.is_some() {
        let rest = now + PROBE_TIMEOUT;
        for panel in panels.holders.values_mut() {
            if panel.owner == client {
                panel.probe_rest_until = Some(rest);
            }
        }
    }
}

/// A button or key press reached `client`: a
/// trigger for the press-time liveness check.
pub fn note_user_input(lp: &mut Loop, client: Option<ClientId>) {
    if !lp.inner.comp.panels.is_empty() {
        lp.inner.comp.panels.user_input.push(client);
    }
}

/// Popup bookkeeping for one keyboard focus change.
fn note_popup_focus(lp: &mut Loop, to: Option<u64>, from: Option<u64>) {
    if lp.inner.comp.panels.popup_restores.is_empty() {
        return;
    }
    let from_alive = from.is_some_and(|id| layer_alive(lp, SurfaceId(id)));
    let restores = &mut lp.inner.comp.panels.popup_restores;
    let to_held_popup = to.is_some_and(|to| restores.contains_key(&to));
    if let Some(to) = to
        && let Some(entry) = restores.get_mut(&to)
    {
        // Focus came back: whatever departure was recorded is undone.
        entry.departed_to = None;
        if entry.prior.is_none() {
            entry.prior = from;
        }
    }
    if let Some(from) = from
        && let Some(entry) = restores.get_mut(&from)
    {
        if !from_alive {
            entry.fallback = Some(to);
        } else if !to_held_popup {
            entry.departed_to = Some(to);
        }
    }
}

/// A closed popup hands keyboard focus back to
/// what it displaced, only when its destruction moved focus and focus is
/// still where the fallback put it.
fn service_popup_restores(lp: &mut Loop) {
    if lp.inner.comp.panels.popup_restores.is_empty() {
        return;
    }
    let closed: Vec<(u64, PopupRestore)> = lp
        .inner
        .comp
        .panels
        .popup_restores
        .iter()
        .filter(|(popup, _)| !layer_alive(lp, SurfaceId(**popup)))
        .map(|(popup, restore)| (*popup, *restore))
        .collect();
    for (popup, restore) in closed {
        lp.inner.comp.panels.popup_restores.remove(&popup);
        let (Some(prior), Some(fallback), None) = (restore.prior, restore.fallback, restore.departed_to) else {
            continue;
        };
        let Some(keyboard) = lp.state.seat.seat.get_keyboard() else { continue };
        let current = keyboard
            .current_focus()
            .and_then(|surface| lp.inner.comp.id_for_surface(&surface))
            .map(|id| id.0);
        if current != fallback || current == Some(prior) {
            continue;
        }
        let mapped = lp.inner.comp.registry.get(SurfaceId(prior)).is_some_and(|record| record.mapped());
        let Some(surface) = surface_of(lp, SurfaceId(prior)).filter(|_| mapped) else {
            continue;
        };
        keyboard.set_focus(&mut lp.state, Some(surface), smithay::utils::SERIAL_COUNTER.next_serial());
    }
}

/// Start tracking the focus a popup displaces.
fn record_popup_focus(lp: &mut Loop, popup: SurfaceId) {
    let current = lp
        .state
        .seat
        .seat
        .get_keyboard()
        .and_then(|keyboard| keyboard.current_focus())
        .and_then(|surface| lp.inner.comp.id_for_surface(&surface))
        .map(|id| id.0);
    let panels = &mut lp.inner.comp.panels;
    let prior = (current == Some(popup.0))
        .then(|| {
            panels
                .last_focus_change
                .filter(|(to, _)| *to == Some(popup.0))
                .and_then(|(_, from)| from)
        })
        .flatten()
        .filter(|prior| *prior != popup.0);
    // A popup that focus left for this one was not left deliberately: it is
    // the parent of a nested menu, restored when this one closes.
    for entry in panels.popup_restores.values_mut() {
        if entry.departed_to == Some(Some(popup.0)) {
            entry.departed_to = None;
        }
    }
    panels.popup_restores.entry(popup.0).or_insert(PopupRestore {
        prior,
        fallback: None,
        departed_to: None,
    });
}

/// A holder service that left the Bus takes its
/// explicit holds with it; the Wayland-side state stays with the client.
pub fn services_live(lp: &mut Loop, live: &BTreeSet<String>) {
    for panel in lp.inner.comp.panels.holders.values_mut() {
        if panel.reporter.as_ref().is_some_and(|reporter| !live.contains(reporter)) {
            panel.drop_holds();
            panel.reporter = None;
            panel.reported_surface = None;
            panel.generation = None;
        }
    }
}

/// Per edge, the enforced layers and the explicit
/// holds, summed over outputs (`input.corners.{enforced,held}.*`).
pub fn edge_counts(lp: &Loop) -> (EdgeCounts, EdgeCounts) {
    let mut enforced = EdgeCounts::default();
    let mut held = EdgeCounts::default();
    for ((_, edge), panel) in &lp.inner.comp.panels.holders {
        if let (Some(enforced), Some(held)) = (enforced.edge_mut(edge), held.edge_mut(edge)) {
            *enforced += panel.enforced.len() as u64;
            *held += panel.held.len() as u64;
        }
    }
    (enforced, held)
}

/// `compd.truth`'s `panels`: each edge's holder state.
pub fn truth(lp: &Loop) -> Value {
    lp.inner
        .comp
        .panels
        .holders
        .iter()
        .map(|((output, edge), panel)| {
            json!({
                "output": output,
                "edge": edge,
                "surface": panel.surface,
                "id": panel.id.map(|id| id.0),
                "mode": panel.mode,
                "verdict": panel.verdict,
                "held": panel.held.keys().collect::<Vec<_>>(),
                "pointer": match panel.pointer {
                    PointerHold::Out => "out",
                    PointerHold::Inside => "inside",
                    PointerHold::Lingering(_) => "lingering",
                },
                "focused": panel.focused,
                "owned": panel.owner.is_some(),
                "stalled": panel.stalled,
                "probing": panel.probe.is_some(),
                "enforced": panel.enforced.iter().map(|id| id.0).collect::<Vec<_>>(),
                // The panel layer's buffer commits: the frame-callback oracle.
                "commits": panel.id.map(|id| lp.inner.comp.commits(id)),
            })
        })
        .collect()
}

/// The `panel.command`s owed since the last call: `(output, edge, surface,
/// reveal)`.
pub fn take_commands(lp: &mut Loop) -> Vec<PanelCommand> {
    std::mem::take(&mut lp.inner.comp.panels.commands)
}

// ── lookups ──────────────────────────────────────────────────────────────────

/// The live outputs, by name.
fn live_outputs(lp: &Loop) -> BTreeSet<String> {
    lp.inner.space_state().state.outputs().map(|output| output.name()).collect()
}

/// Every layer whose namespace is `token`, flagged by whether it is on
/// `output`.
fn candidates(lp: &Loop, token: &str, output: &str) -> Vec<(SurfaceId, bool)> {
    let mut found = Vec::new();
    for candidate in lp.inner.space_state().state.outputs() {
        let map = layer_map_for_output(candidate);
        for layer in map.layers().filter(|layer| layer.namespace() == token) {
            if let Some(id) = lp.inner.comp.id_for_surface(layer.wl_surface()) {
                found.push((id, candidate.name() == output));
            }
        }
    }
    found
}

/// The live `wl_surface` behind a record.
fn surface_of(lp: &Loop, id: SurfaceId) -> Option<WlSurface> {
    let SurfaceHandle::Wl(object) = lp.inner.comp.registry.get(id)?.handle().clone() else {
        return None;
    };
    WlSurface::from_id(&lp.state.output.display_handle, object).ok()
}

/// The record is still a known layer (a destroyed layer is a closed
/// popup).
fn layer_alive(lp: &Loop, id: SurfaceId) -> bool {
    lp.inner.comp.registry.get(id).is_some_and(|record| record.role() == SurfaceRole::Layer)
}

/// A live layer's mapped state and its Wayland client; `None` once gone.
fn layer_facts(lp: &Loop, id: SurfaceId) -> Option<LayerFacts> {
    let record = lp.inner.comp.registry.get(id).filter(|record| record.role() == SurfaceRole::Layer)?;
    let client = surface_of(lp, id).and_then(|surface| surface.client()).map(|client| client.id());
    Some((record.mapped(), client))
}

fn client_alive(lp: &Loop, client: &ClientId) -> bool {
    lp.state.output.display_handle.backend_handle().get_client_data(client.clone()).is_ok()
}

/// Incarnation fencing by the layer's Wayland client.
fn claim_for(lp: &Loop, owner: Option<&ClientId>, id: SurfaceId, may_adopt: bool) -> Claim<ClientId> {
    let client = layer_facts(lp, id).and_then(|(_, client)| client);
    let owner_alive = owner.is_some_and(|owner| client_alive(lp, owner));
    panel_claim(owner, owner_alive, client, may_adopt)
}

/// Re-send a layer's current configure, unchanged: a live client answers
/// with `ack_configure`.
fn send_probe_configure(lp: &Loop, id: SurfaceId) -> Option<Serial> {
    let surface = surface_of(lp, id)?;
    lp.inner.space_state().state.outputs().find_map(|output| {
        let map = layer_map_for_output(output);
        map.layers()
            .find(|layer| layer.wl_surface() == &surface)
            .map(|layer| layer.layer_surface().send_configure())
    })
}

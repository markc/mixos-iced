// Requests parse through comp-model's PanelRequest and are applied as a
// PanelOp; holders are PanelHolders<u32, u32> (client, serial);
// `panel_claim` is the pure decision.

use super::*;
use comp_model::observation::{AffectedTopics, ObservationRecord, PanelRequest};
use serde_json::{Value, json};

type Holders = PanelHolders<u32, u32>;

fn op(request: &PanelRequest) -> PanelOp<'_> {
    PanelOp {
        output: &request.output,
        edge: &request.edge,
        surface: &request.surface,
        holder: request.holder.as_deref(),
        acquire: request.acquire,
        mode: request.mode.as_deref(),
    }
}

#[test]
fn hold_acquire_release_round_trip() {
    let mut panels: BTreeMap<(String, String), Holders> = BTreeMap::new();
    let request = |holder: &str, acquire: bool| PanelRequest::parse("comp.panel.hold", &json!({
        "output":"DP-1","edge":"left","surface":"quoin-panel-1",
        "holder":holder,"acquire":acquire,
    })).unwrap();
    for holder in ["pointer", "focus", "popup"] {
        let acquire = request(holder, true);
        assert_eq!(apply_panel_request(&mut panels, op(&acquire), Some(SurfaceId(7))), Some(true));
        assert_eq!(apply_panel_request(&mut panels, op(&acquire), Some(SurfaceId(7))), None, "idempotent");
        let release = request(holder, false);
        assert_eq!(apply_panel_request(&mut panels, op(&release), None), Some(false), "release after layer destruction");
        assert_eq!(apply_panel_request(&mut panels, op(&release), None), None);
    }
    assert_eq!(apply_panel_request(&mut panels, op(&request("focus", true)), Some(SurfaceId(7))), Some(true));
    assert_eq!(apply_panel_request(&mut panels, op(&request("popup", true)), Some(SurfaceId(7))), None);
    assert_eq!(apply_panel_request(&mut panels, op(&request("popup", false)), None), None);
    assert_eq!(apply_panel_request(&mut panels, op(&request("focus", false)), None), Some(false));
    for reveal in [true, false] {
        let record = ObservationRecord::PanelCommand {
            output: "DP-1".into(), edge: "left".into(), surface: "quoin-panel-1".into(),
            reveal, event_seq: 42,
        };
        assert_eq!(record.topic_suffix(), "panel.command");
        let wire = record.wire();
        let body: Value = serde_json::from_str(&wire.body).unwrap();
        assert_eq!(body["action"], if reveal { "reveal" } else { "conceal" });
        assert_eq!(body["surface"], "quoin-panel-1");
        assert_eq!(body["event_seq"], 42);
        let mut affected = AffectedTopics::default();
        affected.insert(record.topic_suffix());
        assert!(affected.contains("panel.command"));
    }
}

#[test]
fn mode_report_updates_comp_state() {
    let mut panels: BTreeMap<(String, String), Holders> = BTreeMap::new();
    let key = ("DP-1".to_owned(), "left".to_owned());
    // Revealed modes arrive with a live layer; a concealed (hidden) panel
    // has none, and its recreation carries a fresh token.
    for (mode, token, layer) in [
        ("pinned", "quoin-panel-1", Some(SurfaceId(3))),
        ("hidden", "quoin-panel-1", None),
        ("docked", "quoin-panel-2", Some(SurfaceId(4))),
        ("hidden", "quoin-panel-3", None),
    ] {
        let report = PanelRequest::parse("comp.panel.mode", &json!({
            "output":"DP-1","edge":"left","surface":token,"mode":mode,
        })).unwrap();
        // A hidden report re-states the compositor's verdict (nothing holds here);
        // persistent modes have no holders and draw no command.
        assert_eq!(
            apply_panel_request(&mut panels, op(&report), layer),
            (mode == "hidden").then_some(false)
        );
        let hold = PanelRequest::parse("comp.panel.hold", &json!({
            "output":"DP-1","edge":"left","surface":"menu-1","holder":"popup","acquire":true,
        })).unwrap();
        let revealed = apply_panel_request(&mut panels, op(&hold), Some(SurfaceId(7)));
        let panel = &panels[&key];
        assert_eq!(panel.mode, mode);
        assert_eq!(panel.surface, token, "the mode report rebinds the token");
        assert_eq!(panel.id, layer, "the mode report records its layer");
        assert_eq!(panel.held.is_empty(), mode != "hidden", "persistent modes ignore holds");
        assert_eq!(revealed, (mode == "hidden").then_some(true));
        let release = PanelRequest::parse("comp.panel.hold", &json!({
            "output":"DP-1","edge":"left","surface":"menu-1","holder":"popup","acquire":false,
        })).unwrap();
        apply_panel_request(&mut panels, op(&release), None);
    }
    // A persistent mode clears holds already recorded under hidden.
    let hold = PanelRequest::parse("comp.panel.hold", &json!({
        "output":"DP-1","edge":"left","surface":"menu-2","holder":"popup","acquire":true,
    })).unwrap();
    assert_eq!(apply_panel_request(&mut panels, op(&hold), Some(SurfaceId(8))), Some(true));
    let pin = PanelRequest::parse("comp.panel.mode", &json!({
        "output":"DP-1","edge":"left","surface":"quoin-panel-3","mode":"pinned",
    })).unwrap();
    apply_panel_request(&mut panels, op(&pin), Some(SurfaceId(5)));
    assert!(panels[&key].held.is_empty());
    // A release for an edge never seen invents no state.
    let stray = PanelRequest::parse("comp.panel.hold", &json!({
        "output":"DP-1","edge":"top","surface":"menu-9","holder":"popup","acquire":false,
    })).unwrap();
    assert_eq!(apply_panel_request(&mut panels, op(&stray), None), None);
    assert!(!panels.contains_key(&("DP-1".to_owned(), "top".to_owned())));
}


/// A hidden panel as Quoin reports it, with the pointer engaged on its
/// hotspot (so its first verdict, a reveal, is already out).
fn dwelled_panel(at: Instant) -> Holders {
    let mut panel = Holders::new("quoin-panel-1".into(), Some(SurfaceId(3)));
    assert_eq!(panel.settle(true), Some(false));
    panel.observe(Membership { dwelled: true, hotspot: true, ..Membership::default() }, at);
    assert_eq!(panel.settle(false), Some(true), "dwelling acquires the pointer");
    panel
}

fn popup(panel: &mut Holders, acquire: bool) {
    if acquire {
        panel.held.insert("popup".into(), ("menu-1".into(), SurfaceId(9)));
    } else {
        panel.held.remove("popup");
    }
}

#[test]
fn conceal_arms_only_on_last_holder_release() {
    let start = Instant::now();
    let ms = |n: u64| start + Duration::from_millis(n);
    let mut panel = dwelled_panel(ms(0));
    popup(&mut panel, true);
    // The pointer leaves while the popup still holds: no deadline.
    panel.observe(Membership::default(), ms(100));
    assert_eq!(panel.pointer, PointerHold::Lingering(ms(100)));
    assert_eq!(panel.conceal_deadline(), None, "the pointer is not the last holder");
    assert_eq!(panel.settle(false), None);
    // The popup releases inside the pointer's delay: the pointer is now
    // the last holder, and its deadline is where its own delay ends.
    popup(&mut panel, false);
    panel.observe(Membership::default(), ms(300));
    panel.expire(ms(300));
    assert_eq!(panel.conceal_deadline(), Some(ms(900)));
    assert_eq!(panel.settle(false), None, "still held until the deadline");
    panel.expire(ms(899));
    assert_eq!(panel.settle(false), None);
    panel.expire(ms(900));
    assert_eq!(panel.settle(false), Some(false), "the deadline conceals");
    assert_eq!(panel.conceal_deadline(), None, "and nothing re-arms");
    // A popup released after the pointer's delay ran out conceals at once.
    let mut panel = dwelled_panel(ms(0));
    popup(&mut panel, true);
    panel.observe(Membership::default(), ms(100));
    panel.expire(ms(2000));
    assert_eq!(panel.conceal_deadline(), None);
    assert_eq!(panel.settle(false), None);
    popup(&mut panel, false);
    assert_eq!(panel.settle(false), Some(false), "a popup's release is immediate");
    // Persistent panels arm nothing whatever the pointer does.
    let mut panel = dwelled_panel(ms(0));
    panel.mode = "pinned".into();
    panel.observe(Membership::default(), ms(100));
    assert_eq!(panel.conceal_deadline(), None);
    assert_eq!(panel.settle(false), None);
}

#[test]
fn reentry_within_delay_cancels_conceal() {
    let start = Instant::now();
    let ms = |n: u64| start + Duration::from_millis(n);
    let mut panel = dwelled_panel(ms(0));
    panel.observe(Membership::default(), ms(100));
    assert_eq!(panel.conceal_deadline(), Some(ms(900)));
    // Back into the hotspot, not yet dwelled: re-entry keeps the hold.
    panel.observe(Membership { hotspot: true, ..Membership::default() }, ms(500));
    assert_eq!(panel.pointer, PointerHold::Inside);
    assert_eq!(panel.conceal_deadline(), None, "re-entry cancels the deadline");
    panel.expire(ms(2000));
    assert_eq!(panel.settle(false), None);
    // Into the visible panel itself: the same.
    panel.observe(Membership::default(), ms(2000));
    panel.observe(Membership { surface: true, ..Membership::default() }, ms(2700));
    assert_eq!(panel.conceal_deadline(), None);
    panel.expire(ms(5000));
    assert_eq!(panel.settle(false), None);
    // A new departure starts a fresh delay.
    panel.observe(Membership::default(), ms(5000));
    assert_eq!(panel.conceal_deadline(), Some(ms(5800)));
    panel.expire(ms(5800));
    assert_eq!(panel.settle(false), Some(false));
    // Once released, crossing the hotspot without dwelling acquires nothing.
    panel.observe(Membership { hotspot: true, ..Membership::default() }, ms(6000));
    assert_eq!(panel.pointer, PointerHold::Out);
    assert_eq!(panel.settle(false), None);
}

#[test]
fn focus_holder_survives_pointer_departure() {
    let start = Instant::now();
    let ms = |n: u64| start + Duration::from_millis(n);
    let mut panel = dwelled_panel(ms(0));
    // A click inside the panel moved keyboard focus there.
    panel.observe(Membership { surface: true, focused: true, ..Membership::default() }, ms(50));
    // The pointer drifts away while the user types.
    panel.observe(Membership { focused: true, ..Membership::default() }, ms(100));
    assert_eq!(panel.conceal_deadline(), None, "focus holds: no timer");
    panel.expire(ms(10_000));
    assert_eq!(panel.pointer, PointerHold::Out);
    assert_eq!(panel.settle(false), None, "focus alone keeps the reveal");
    // Focus moving elsewhere is deliberate: the conceal is immediate.
    panel.observe(Membership::default(), ms(10_000));
    assert_eq!(panel.conceal_deadline(), None);
    assert_eq!(panel.settle(false), Some(false));
    // A hidden mode report re-states the verdict even when unchanged.
    assert_eq!(panel.settle(true), Some(false));
}

#[test]
fn panel_surface_resolution_is_order_independent() {
    let (a, b) = (SurfaceId(1), SurfaceId(2));
    assert_eq!(resolve_panel_surface(std::iter::empty()), Ok(None));
    assert_eq!(resolve_panel_surface([(a, true)]), Ok(Some(a)));
    assert_eq!(resolve_panel_surface([(a, false)]), Err("panel_output_mismatch"));
    // A token on two layers is refused whichever one iteration meets
    // first, including when only one of them is on the right output.
    for pair in [[(a, true), (b, false)], [(b, false), (a, true)], [(a, true), (b, true)]] {
        assert_eq!(resolve_panel_surface(pair), Err("ambiguous_panel_surface"));
    }
}

/// A conceal that ends a commanded reveal arms the recorded-set check; a
/// reveal lifts everything owed or enforced; a probe timing out stalls
/// the owner (its popup and focus holds end); a mode report lifts the
/// exclusion; the end of an incarnation takes everything with it.
#[test]
fn owed_conceals_follow_commanded_reveals_and_stall_drops_popup_and_focus() {
    let mut panel = Holders::new("quoin-panel-1".into(), Some(SurfaceId(3)));
    let settle = |panel: &mut Holders, restate: bool| {
        let previous = panel.verdict;
        let verdict = panel.settle(restate);
        if let Some(reveal) = verdict {
            panel.note_verdict(reveal, previous);
        }
        verdict
    };
    // A first (re-stated) conceal is not a commanded one.
    assert_eq!(settle(&mut panel, true), Some(false));
    assert!(!panel.arm_pending);
    // The compositor reveals, then conceals: the showing set is recorded next pass.
    let start = Instant::now();
    panel.observe(Membership { dwelled: true, hotspot: true, ..Membership::default() }, start);
    assert_eq!(settle(&mut panel, false), Some(true));
    panel.observe(Membership::default(), start);
    panel.expire(start + CONCEAL_DELAY);
    assert_eq!(settle(&mut panel, false), Some(false));
    assert!(panel.arm_pending);
    // Owed and enforced state; a reveal lifts all of it.
    panel.pending.insert(SurfaceId(3));
    panel.enforce_at = Some(start);
    panel.enforced.insert(SurfaceId(3));
    assert!(panel.owed());
    panel.note_verdict(true, Some(false));
    assert!(!panel.owed() && panel.pending.is_empty() && !panel.arm_pending);
    // Stalling drops the popup and focus holds, and only those.
    panel.held.insert("popup".into(), ("menu-1".into(), SurfaceId(9)));
    panel.held.insert("focus".into(), ("quoin-panel-1".into(), SurfaceId(3)));
    panel.held.insert("pointer".into(), ("quoin-panel-1".into(), SurfaceId(3)));
    panel.stall();
    assert!(panel.stalled);
    assert_eq!(panel.held.keys().collect::<Vec<_>>(), ["pointer"]);
    // A mode report lifts the exclusion (the caller re-arms a still owed
    // conceal); a persistent one also ends the holds.
    panel.enforced.insert(SurfaceId(3));
    panel.probe = Some(Probe { deadline: start, serials: Vec::new() });
    let key = ("DP-1".to_owned(), "left".to_owned());
    let mut panels = BTreeMap::from([(key.clone(), panel)]);
    let report = PanelRequest::parse("comp.panel.mode", &json!({
        "output":"DP-1","edge":"left","surface":"quoin-panel-1","mode":"hidden",
    })).unwrap();
    apply_panel_request(&mut panels, op(&report), Some(SurfaceId(3)));
    let panel = panels.get_mut(&key).unwrap();
    assert!(!panel.owed(), "the exclusion and the probe lift");
    assert!(panel.held.contains_key("pointer"), "a hidden report keeps holds");
    let pin = PanelRequest::parse("comp.panel.mode", &json!({
        "output":"DP-1","edge":"left","surface":"quoin-panel-1","mode":"pinned",
    })).unwrap();
    apply_panel_request(&mut panels, op(&pin), Some(SurfaceId(3)));
    let panel = panels.get_mut(&key).unwrap();
    assert!(panel.held.is_empty());
    // The owner's disconnect ends the incarnation.
    panel.mode = "hidden".into();
    panel.held.insert("focus".into(), ("quoin-panel-1".into(), SurfaceId(3)));
    panel.popups.insert(SurfaceId(9));
    panel.enforced.insert(SurfaceId(3));
    panel.quiet = true;
    panel.drop_incarnation();
    assert!(panel.held.is_empty() && panel.popups.is_empty() && !panel.stalled && !panel.quiet);
    assert!(!panel.owed() && panel.owner.is_none());
    // Bus-side cleanup ends holds only.
    panel.held.insert("focus".into(), ("quoin-panel-1".into(), SurfaceId(3)));
    panel.enforced.insert(SurfaceId(3));
    panel.drop_holds();
    assert!(panel.held.is_empty());
    assert_eq!(panel.enforced, BTreeSet::from([SurfaceId(3)]), "enforcement is Wayland-side");
}

/// Only a registered holder service's report adopts an unowned edge.
#[test]
fn claims_bind_only_what_the_holder_reported() {
    let adopt = |claim: Claim<u32>| claim.binds();
    assert!(!adopt(Claim::Unowned));
    assert!(!adopt(Claim::Refuse));
    let request = PanelRequest::parse("comp.panel.mode", &json!({
        "output":"DP-1","edge":"left","surface":"quoin-panel-1","mode":"hidden","generation":3,
    })).unwrap();
    assert_eq!((request.generation, request.sender.as_str()), (Some(3), ""), "the body never names the sender");
    let Err(reply) = PanelRequest::parse("comp.panel.hold", &json!({
        "output":"DP-1","edge":"left","surface":"quoin-panel-1","holder":"focus","acquire":true,
        "generation":3,
    })) else {
        panic!("a hold carries no generation");
    };
    let body: Value = serde_json::from_str(&reply.into_wire().1).unwrap();
    assert_eq!(body["field"], "generation");
}


/// The pure `panel_claim` decision over facts the engine reads from Wayland.
#[test]
fn claims_follow_the_owner_and_the_reporter() {
    assert_eq!(panel_claim::<u32>(None, false, None, true), Claim::Refuse, "an unnamed client");
    assert_eq!(panel_claim(Some(&1), true, Some(1), false), Claim::Accept);
    assert_eq!(panel_claim(None, false, Some(2), true), Claim::Adopt(2));
    assert_eq!(panel_claim(None, false, Some(2), false), Claim::Unowned);
    assert_eq!(panel_claim(Some(&1), true, Some(2), true), Claim::Refuse, "a copied token");
    assert_eq!(panel_claim(Some(&1), false, Some(2), true), Claim::Replace(2));
    assert_eq!(panel_claim(Some(&1), false, Some(2), false), Claim::Unowned);
    let mut panel = Holders::new("quoin-panel-1".into(), None);
    panel.owner = Some(1);
    panel.held.insert("focus".into(), ("quoin-panel-1".into(), SurfaceId(3)));
    Claim::Replace(2).apply(&mut panel);
    assert_eq!(panel.owner, Some(2));
    assert!(panel.held.is_empty(), "nothing of the last incarnation survives");
    Claim::Adopt(3).apply(&mut panel);
    assert_eq!(panel.owner, Some(3));
}

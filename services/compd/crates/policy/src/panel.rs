// Generic over the engine's client id `C` and configure serial `S`;
// `apply_panel_request` takes the parsed request's fields, so the policy
// needs no Bus types; `panel_claim` takes the client facts the engine reads
// from Wayland. Every deadline is a value (`conceal_deadline`), never a poll.

//! Quoin's panel holders: per `(output, edge)` panel,
//! the explicit holds Quoin requests (`comp.panel.hold`) plus the pointer
//! and focus membership the compositor tracks itself, the conceal timer and
//! the verdict (`panel.command` reveal/conceal) owed to Quoin.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use surfaces::SurfaceId;

/// The `input.corners.holders` capability (see comp-model).
pub use comp_model::observation::HOLDER_PLANE_AVAILABLE;

/// A pointer holder releases only after the pointer has
/// been away from the hotspot, the panel and its popups this long. Focus and
/// popup releases are deliberate and conceal at once.
pub const CONCEAL_DELAY: Duration = Duration::from_millis(800);
/// How long Quoin has to apply a conceal before the compositor checks
/// whether it is alive.
pub const ENFORCE_GRACE: Duration = Duration::from_millis(1000);
/// How long a liveness probe waits for its `ack_configure`.
pub const PROBE_TIMEOUT: Duration = Duration::from_millis(1000);

/// The holder sets of one `(output, edge)` panel. Pure state with an
/// injected clock; the timer and the Wayland lookups are the engine's.
#[derive(Debug)]
pub struct PanelHolders<C, S> {
    pub surface: String,
    /// Re-resolved from `surface` on layer lifecycle edges.
    pub id: Option<SurfaceId>,
    pub mode: String,
    /// Holder kind -> (token, layer id at acquire).
    pub held: BTreeMap<String, (String, SurfaceId)>,
    /// The pointer holder the compositor tracks from its hotspot and hit-testing.
    pub pointer: PointerHold,
    /// Keyboard focus is on the panel's layer or a held popup's.
    pub focused: bool,
    /// The last command sent for this edge; `None` while persistent and
    /// before the first verdict.
    pub verdict: Option<bool>,
    /// The Wayland client whose layers this edge belongs to (the Quoin
    /// incarnation).
    pub owner: Option<C>,
    /// Popup layers acquired for this edge under `owner`, while they live.
    pub popups: BTreeSet<SurfaceId>,
    /// The next step of an owed conceal: the grace Quoin has to apply it.
    pub enforce_at: Option<Instant>,
    /// The owner's layers that were showing at a conceal the compositor commanded.
    pub pending: BTreeSet<SurfaceId>,
    /// A conceal just ended a commanded reveal: record `pending` next pass.
    pub arm_pending: bool,
    /// An unanswered liveness probe of this edge's layers.
    pub probe: Option<Probe<S>>,
    /// The owner answered a probe for the current showing state.
    pub quiet: bool,
    /// No press-triggered probe of the owner before this.
    pub probe_rest_until: Option<Instant>,
    /// A probe went unanswered: the owner is stopped.
    pub stalled: bool,
    /// The owner's layers the compositor itself hides and excludes from input.
    pub enforced: BTreeSet<SurfaceId>,
    /// The registered Bus service whose mode report named this edge's token.
    pub reporter: Option<String>,
    /// The token the reporter itself named.
    pub reported_surface: Option<String>,
    /// The reporter's Bus connection generation, when it states one.
    pub generation: Option<u64>,
}

/// One liveness probe: an unchanged configure re-sent to each candidate
/// layer, answered by any acknowledgement at or after its serial.
#[derive(Clone, Debug)]
pub struct Probe<S> {
    pub deadline: Instant,
    pub serials: Vec<(SurfaceId, S)>,
}

/// The pointer holder. `Lingering` is a released pointer still inside its
/// conceal delay.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PointerHold {
    #[default]
    Out,
    Inside,
    Lingering(Instant),
}

/// Focus restoration for one held popup (surface ids).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PopupRestore {
    /// The focus the popup displaced when it took focus.
    pub prior: Option<u64>,
    /// Where focus went when the popup's destruction moved it.
    pub fallback: Option<Option<u64>>,
    /// Focus left the popup while it lived, to this surface.
    pub departed_to: Option<Option<u64>>,
}

/// What the compositor observed of one panel at a stable dispatch boundary.
#[derive(Clone, Copy, Debug, Default)]
pub struct Membership {
    /// The pointer dwelled in this edge's hotspot (the corner is engaged).
    pub dwelled: bool,
    /// The pointer is in this edge's hotspot, dwelled or not.
    pub hotspot: bool,
    /// The pointer is over the panel's layer or a held popup's.
    pub surface: bool,
    /// Keyboard focus is on the panel's layer or a held popup's.
    pub focused: bool,
}

impl<C, S> PanelHolders<C, S> {
    pub fn new(surface: String, id: Option<SurfaceId>) -> Self {
        Self {
            surface,
            id,
            mode: "hidden".into(),
            held: BTreeMap::new(),
            pointer: PointerHold::Out,
            focused: false,
            verdict: None,
            owner: None,
            popups: BTreeSet::new(),
            enforce_at: None,
            pending: BTreeSet::new(),
            arm_pending: false,
            probe: None,
            quiet: false,
            probe_rest_until: None,
            stalled: false,
            enforced: BTreeSet::new(),
            reporter: None,
            reported_surface: None,
            generation: None,
        }
    }

    pub fn hidden(&self) -> bool {
        self.mode == "hidden"
    }

    /// Follow a verdict just sent (`previous` is the one before it). A
    /// reveal lifts everything owed or enforced. A conceal that ends a reveal
    /// the compositor commanded owes Quoin's conceal.
    pub fn note_verdict(&mut self, reveal: bool, previous: Option<bool>) {
        if reveal {
            self.clear_enforcement();
            self.quiet = false;
        } else if previous == Some(true) {
            self.clear_enforcement();
            self.quiet = false;
            self.arm_pending = true;
        }
    }

    /// Something is owed or enforced on this edge.
    pub fn owed(&self) -> bool {
        !self.enforced.is_empty() || self.enforce_at.is_some() || self.probe.is_some()
    }

    /// Quoin applied the conceal, or answered a probe: nothing is owed.
    pub fn settle_owed(&mut self) {
        self.pending.clear();
        self.enforce_at = None;
        self.probe = None;
    }

    fn clear_enforcement(&mut self) {
        self.settle_owed();
        self.arm_pending = false;
        self.enforced.clear();
    }

    /// The popup and focus holds of a stopped owner end.
    pub fn stall(&mut self) {
        self.stalled = true;
        self.held.remove("popup");
        self.held.remove("focus");
    }

    /// The holder service's explicit holds end (it left the Bus, or a new
    /// Bus generation of it reported).
    pub fn drop_holds(&mut self) {
        self.held.clear();
    }

    /// The owning Wayland client is gone: every explicit hold, popup and
    /// enforcement of that incarnation goes with it.
    pub fn drop_incarnation(&mut self) {
        self.owner = None;
        self.drop_holds();
        self.popups.clear();
        self.stalled = false;
        self.quiet = false;
        self.clear_enforcement();
    }

    /// Fold one observation into the automatic holders. The pointer acquires
    /// by dwelling in the hotspot or by entering the panel or a popup it
    /// holds; any contact keeps it; leaving all of them starts its delay.
    pub fn observe(&mut self, seen: Membership, now: Instant) {
        let acquire = seen.dwelled || seen.surface;
        let contact = acquire || seen.hotspot;
        self.pointer = match self.pointer {
            PointerHold::Out if acquire => PointerHold::Inside,
            PointerHold::Out => PointerHold::Out,
            PointerHold::Inside | PointerHold::Lingering(_) if contact => PointerHold::Inside,
            PointerHold::Inside => PointerHold::Lingering(now),
            lingering @ PointerHold::Lingering(_) => lingering,
        };
        self.focused = seen.focused;
    }

    /// A lingering pointer whose delay has run out has released.
    pub fn expire(&mut self, now: Instant) {
        if let PointerHold::Lingering(since) = self.pointer
            && now >= since + CONCEAL_DELAY
        {
            self.pointer = PointerHold::Out;
        }
    }

    fn holding(&self) -> bool {
        self.pointer != PointerHold::Out || self.focused || !self.held.is_empty()
    }

    /// The one-shot conceal deadline. It exists only while the lingering
    /// pointer is the last holder of a hidden panel, so it is armed by the
    /// last release and cancelled by any holder returning.
    pub fn conceal_deadline(&self) -> Option<Instant> {
        match self.pointer {
            PointerHold::Lingering(since)
                if self.hidden() && !self.focused && self.held.is_empty() =>
            {
                Some(since + CONCEAL_DELAY)
            }
            _ => None,
        }
    }

    /// The command owed to Quoin: `Some(true)` reveal, `Some(false)` conceal.
    /// Emitted on a change of verdict, or always when `restate`.
    pub fn settle(&mut self, restate: bool) -> Option<bool> {
        if !self.hidden() {
            self.verdict = None;
            return None;
        }
        let holding = self.holding();
        (restate || self.verdict != Some(holding)).then(|| {
            self.verdict = Some(holding);
            holding
        })
    }
}

/// Exact namespace resolution. `candidates` are every layer whose namespace
/// is the token, flagged by whether it is on the requested output. All of
/// them are considered, so the verdict never depends on surface order.
pub fn resolve_panel_surface(
    candidates: impl IntoIterator<Item = (SurfaceId, bool)>,
) -> Result<Option<SurfaceId>, &'static str> {
    let mut candidates = candidates.into_iter();
    let Some((id, on_output)) = candidates.next() else {
        return Ok(None);
    };
    if candidates.next().is_some() {
        Err("ambiguous_panel_surface")
    } else if on_output {
        Ok(Some(id))
    } else {
        Err("panel_output_mismatch")
    }
}

/// One `comp.panel.hold` / `comp.panel.mode`, already parsed (comp-model
/// `PanelRequest` carries exactly these fields).
#[derive(Clone, Copy, Debug)]
pub struct PanelOp<'a> {
    pub output: &'a str,
    pub edge: &'a str,
    pub surface: &'a str,
    pub holder: Option<&'a str>,
    pub acquire: Option<bool>,
    pub mode: Option<&'a str>,
}

/// Idempotent explicit requests, returning the command owed to Quoin (see
/// [`PanelHolders::settle`]): a hidden mode report always re-states the
/// verdict, a hold only reports a change of it.
pub fn apply_panel_request<C, S>(
    panels: &mut BTreeMap<(String, String), PanelHolders<C, S>>,
    request: PanelOp<'_>,
    id: Option<SurfaceId>,
) -> Option<bool> {
    let key = (request.output.to_string(), request.edge.to_string());
    // A release for an edge with no state has nothing to release.
    if request.acquire == Some(false) && !panels.contains_key(&key) {
        return None;
    }
    let panel = panels
        .entry(key)
        .or_insert_with(|| PanelHolders::new(request.surface.to_string(), id));
    if let Some(mode) = request.mode {
        panel.surface = request.surface.to_string();
        panel.id = id;
        panel.mode = mode.to_string();
        // A report comes from a live Quoin: the compositor's exclusion lifts. A
        // persistent panel is meant to show, so its holds end.
        if mode != "hidden" {
            panel.drop_holds();
        }
        panel.clear_enforcement();
        return panel.settle(true);
    }
    if !panel.hidden() {
        return None;
    }
    if let (Some(holder), Some(acquire)) = (request.holder, request.acquire) {
        if acquire {
            if let Some(id) = id {
                panel
                    .held
                    .insert(holder.to_string(), (request.surface.to_string(), id));
            }
        } else if panel
            .held
            .get(holder)
            .is_some_and(|(surface, _)| surface == request.surface)
        {
            panel.held.remove(holder);
        }
    }
    panel.settle(false)
}

/// What a resolved layer may do to an edge's incarnation.
#[derive(Clone, Debug, PartialEq)]
pub enum Claim<C> {
    /// The layer is the owner's.
    Accept,
    /// The edge has no owner and a registered holder service's mode report
    /// named this layer's token: its client becomes the owner.
    Adopt(C),
    /// The owner is gone without its disconnect handled yet, and a
    /// registered holder service reported this layer: the next incarnation.
    Replace(C),
    /// No owner, and nothing entitles this layer to adopt the edge.
    Unowned,
    /// Another live client owns the edge (a copied token), or the compositor cannot
    /// name the layer's client.
    Refuse,
}

impl<C> Claim<C> {
    pub fn binds(&self) -> bool {
        matches!(self, Self::Accept | Self::Adopt(_) | Self::Replace(_))
    }

    pub fn apply<S>(self, panel: &mut PanelHolders<C, S>) {
        match self {
            Self::Accept => {}
            Self::Adopt(client) => panel.owner = Some(client),
            Self::Replace(client) => {
                panel.drop_incarnation();
                panel.owner = Some(client);
            }
            Self::Unowned | Self::Refuse => {}
        }
    }
}

/// Incarnation fencing by Wayland client. `layer_client` is the layer's
/// client (none: the compositor cannot name it, so it claims nothing);
/// `owner_alive` whether the recorded owner is still connected;
/// `may_adopt` whether a registered holder service's mode report named the
/// token.
pub fn panel_claim<C: Clone + PartialEq>(
    owner: Option<&C>,
    owner_alive: bool,
    layer_client: Option<C>,
    may_adopt: bool,
) -> Claim<C> {
    let Some(client) = layer_client else {
        return Claim::Refuse;
    };
    match owner {
        Some(owner) if *owner == client => Claim::Accept,
        None if may_adopt => Claim::Adopt(client),
        None => Claim::Unowned,
        Some(_) if owner_alive => Claim::Refuse,
        Some(_) if may_adopt => Claim::Replace(client),
        Some(_) => Claim::Unowned,
    }
}

#[cfg(test)]
#[path = "panel_tests.rs"]
mod tests;

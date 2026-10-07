//! The panel model (`edges::ShellModel`), one per
//! output, so an edge shows ONE page at a time with Quoin's modes, thickness
//! and motion.
//!
//! The scene store's page registry decides which pages exist and where
//! (`seat::PageRegistry`, the mount reservations); this module mirrors them
//! into each output's carousels, and answers the `shell.panel.*` and
//! `shell.props.get` reads and writes the scenes loader drives:
//! - a page is a carousel page on its edge; only the active page of a mapped
//!   edge is drawn, at the model's thickness, sliding with its visible
//!   fraction;
//! - fresh edges start hidden; Quoin's saved modes restore before pages
//!   register. Empty edges are suppressed,
//!   as Quoin's scene-only frame does;
//! - a change to any edge row or the dialog seat is published once as
//!   `<service>.panel.changed` with a new revision, never on a timer.
//!
//! Time is host-monotonic (`Instant` since the host started), as the model
//! requires.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use edges::{
    Carousel, Corner, CornerDetector, CornerDetectorConfig, CornerEvent, Edge as ShellEdge, LogicalPoint, LogicalSize, OutputKey,
    PanelConfigError, PanelInput, PanelMode, PanelSnapshot, PanelWake, PointerSample, ShellModel, resize_thickness_range,
};
use serde_json::{Value, json};

use crate::mount::{mount_config, page_id, scene_edge};
use crate::render::{EDGE_HEIGHT, EDGE_WIDTH};
use crate::seat::Edge;
use crate::store::SceneStore;

/// Quoin's timings.
const GRACE: Duration = Duration::from_millis(800);
const MOTION: Duration = Duration::from_millis(200);

pub fn shell_edge(edge: Edge) -> ShellEdge {
    match edge {
        Edge::Left => ShellEdge::Left,
        Edge::Right => ShellEdge::Right,
        Edge::Top => ShellEdge::Top,
        Edge::Bottom => ShellEdge::Bottom,
    }
}

/// `left` / `right` / `top` / `bottom`.
pub fn parse_edge(name: &str) -> Option<Edge> {
    match name {
        "left" => Some(Edge::Left),
        "right" => Some(Edge::Right),
        "top" => Some(Edge::Top),
        "bottom" => Some(Edge::Bottom),
        _ => None,
    }
}

fn seat_edge(edge: ShellEdge) -> Edge {
    match edge {
        ShellEdge::Left => Edge::Left,
        ShellEdge::Right => Edge::Right,
        ShellEdge::Top => Edge::Top,
        ShellEdge::Bottom => Edge::Bottom,
    }
}

/// The edge a `shell.corner.*` verb's `corner` summons (Quoin
/// `corner_edge`, `Corner::summoned_edge`: top-left → left, bottom-left →
/// bottom, bottom-right → right, top-right → top).
pub fn corner_edge(corner: &str) -> Option<Edge> {
    let corner = match corner {
        "top-left" => Corner::TopLeft,
        "bottom-left" => Corner::BottomLeft,
        "bottom-right" => Corner::BottomRight,
        "top-right" => Corner::TopRight,
        _ => return None,
    };
    Some(seat_edge(corner.summoned_edge()))
}

const EDGES: [Edge; 4] = [Edge::Left, Edge::Bottom, Edge::Right, Edge::Top];

/// What one edge draws now: its active page, thickness and visible fraction.
#[derive(Clone, Debug, PartialEq)]
pub struct Drawn {
    pub page: String,
    /// Logical px.
    pub thickness: f32,
    /// 0 (off screen) ..= 1 (fully in).
    pub fraction: f32,
}

/// Why a panel write did not apply (the `{error_code, message}` refusals).
#[derive(Clone, Debug, PartialEq)]
pub struct Refusal {
    pub code: &'static str,
    pub message: String,
}

pub struct Panels {
    epoch: Instant,
    models: BTreeMap<String, ShellModel>,
    preferences: crate::preferences::Preferences,
    /// Each model's logical size as last set.
    sizes: BTreeMap<String, (f32, f32)>,
    /// Page id → the (output, edge) its carousel page was registered on.
    registered: BTreeMap<String, (String, Edge)>,
    /// Each output's last published notice (no generation/revision).
    applied: BTreeMap<String, Value>,
    revision: u64,
    /// conf.mix's page order per edge (`shell.panel.order`), applied to every
    /// output's carousels and reported as each row's `declared`.
    declared: crate::conf::Declared,
    pointer: Option<Pointer>,
    /// Ownership survives departure/config changes until the matching release.
    presses: BTreeMap<u32, Option<HotspotPress>>,
    state: crate::state::StateStore,
    menu: Option<crate::menu::Menu>,
    menu_serial: u64,
    /// Keyboard and popup holders share the local core's hold input, but
    /// releasing one must preserve the other.
    keyboard: BTreeMap<String, Edge>,
    pending_focus: BTreeMap<String, Edge>,
    /// Loader-owned popup pages use the same command as the panel button.
    popup_routes: BTreeMap<(String, String), (String, String)>,
    popup_commands: Vec<(String, String)>,
    /// The current config is read each time a corner opens.
    pub menu_conf: Option<std::path::PathBuf>,
}

struct HotspotPress {
    output: String,
    corner: Corner,
    position: LogicalPoint,
    shift: bool,
    tolerance: f32,
}

/// One human pointer stream, independent of the output currently rendering.
struct Pointer {
    output: String,
    sample: PointerSample,
    detector: CornerDetector,
    /// Bounds at deliberate conceal: animation alone cannot count as a leave.
    reveal_latched: [Option<f32>; 4],
}

impl Default for Panels {
    fn default() -> Self {
        Self {
            epoch: Instant::now(),
            models: BTreeMap::new(),
            preferences: Default::default(),
            sizes: BTreeMap::new(),
            registered: BTreeMap::new(),
            applied: BTreeMap::new(),
            revision: 0,
            declared: Default::default(),
            pointer: None,
            presses: BTreeMap::new(),
            state: Default::default(),
            menu: None,
            menu_serial: 0,
            keyboard: BTreeMap::new(),
            pending_focus: BTreeMap::new(),
            popup_routes: BTreeMap::new(),
            popup_commands: Vec::new(),
            menu_conf: None,
        }
    }
}

/// A `shell.panel.*` / `shell.corner.*` semantic verb (Quoin
/// `semantic_shell_command`'s panel arms).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PanelVerb {
    Show,
    Hide,
    Toggle,
    Pin,
    Unpin,
    Dock,
}

impl PanelVerb {
    /// The core input it is (Quoin: show reveals transiently, toggle is
    /// `ToggleShown`, pin overlays without reserving, unpin releases a
    /// persistent mode into a transient reveal, dock reserves).
    fn input(self) -> PanelInput {
        match self {
            Self::Show => PanelInput::Reveal,
            Self::Hide => PanelInput::Hide,
            Self::Toggle => PanelInput::ToggleShown,
            Self::Pin => PanelInput::Pin,
            Self::Unpin => PanelInput::Release,
            Self::Dock => PanelInput::Dock,
        }
    }
}

impl Panels {
    /// Total activation of an already validated whole profile, before ACK.
    pub(crate) fn set_preferences(&mut self, preferences: crate::preferences::Preferences) {
        let now = self.now();
        let values = preferences.values();
        let mut concealed = Vec::new();
        for (output, model) in &mut self.models {
            let before = ShellEdge::ALL.map(|edge| model.panel(edge));
            let changed = model.set_preferences(values, now);
            for edge in ShellEdge::ALL {
                if changed[edge.index()] {
                    concealed.push((output.clone(), edge));
                    if let Some(pointer) = self.pointer.as_mut().filter(|pointer| &pointer.output == output) {
                        let panel = before[edge.index()];
                        let corner = pointer.detector.diagnostics(now);
                        if panel.pointer_inside || panel.corner_inside
                            || corner.candidate.or(corner.engaged).is_some_and(|corner| corner.summoned_edge() == edge) {
                            pointer.reveal_latched[edge.index()] = Some(panel.thickness_px * panel.visible_fraction.clamp(0.0, 1.0));
                        }
                    }
                }
            }
        }
        self.preferences = preferences;
        for (output, edge) in concealed {
            if self.pending_focus.get(&output) == Some(&seat_edge(edge)) { self.pending_focus.remove(&output); }
            if self.keyboard.get(&output) == Some(&seat_edge(edge)) {
                self.focus_at(&output, None, now);
            }
            if self.menu.as_ref().is_some_and(|menu| menu.output == output && menu.corner.summoned_edge() == edge) {
                self.close_menu_at(now);
            }
        }
        self.refresh_menu_mode();
    }

    /// Enable shared state once, before the host creates any output model.
    pub(crate) fn load_state(&mut self) {
        self.state = crate::state::StateStore::startup();
    }

    fn save_state(&mut self, output: &str) {
        if let Some(model) = self.models.get(output) { self.state.save(model); }
        self.refresh_menu_mode();
    }

    fn refresh_menu_mode(&mut self) {
        if let Some(menu) = self.menu.as_mut()
            && let Some(model) = self.models.get(&menu.output)
        {
            let mode = model.panel(menu.corner.summoned_edge()).mode;
            for item in &mut menu.items {
                if let crate::menu::Choice::Mode(item_mode) = item.choice {
                    item.disabled = model.preference(menu.corner.summoned_edge()).is_some();
                    item.checked = mode == item_mode
                        && !(item_mode == PanelMode::Hidden && model.panel(menu.corner.summoned_edge()).transient_revealed);
                }
            }
            if !menu.items[menu.selected].enabled() { menu.navigate(1); }
        }
    }

    fn now(&self) -> Duration {
        self.epoch.elapsed()
    }

    /// The model for `output`, created at `logical` (w, h) or re-sized to it.
    pub fn ensure(&mut self, output: &str, logical: (f32, f32)) {
        let Ok(size) = LogicalSize::new(logical.0.max(1.0), logical.1.max(1.0)) else { return };
        let now = self.now();
        let wanted = (size.width(), size.height());
        match self.models.get_mut(output) {
            Some(model) => {
                if self.sizes.get(output) != Some(&wanted) {
                    model.set_geometry(size);
                    self.sizes.insert(output.to_owned(), wanted);
                }
            }
            None => {
                let Ok(key) = OutputKey::new(output) else { return };
                let Ok(mut model) = ShellModel::new(key, size, now, GRACE, MOTION) else { return };
                model.suppress_empty_edges(true);
                // A new output's carousels take conf.mix's order (validated
                // when it was declared).
                for edge in ShellEdge::ALL {
                    let _ = model.carousel_mut(edge).redeclare(self.declared[edge.index()].iter().cloned());
                }
                self.state.restore(&mut model);
                model.set_preferences(self.preferences.values(), now);
                self.models.insert(output.to_owned(), model);
                self.sizes.insert(output.to_owned(), wanted);
            }
        }
        if let Some(menu) = self.menu.as_mut().filter(|menu| menu.output == output) {
            menu.fit(wanted, menu.inset);
        }
    }

    /// Mirror the store's edge pages into the carousels: register new pages
    /// (with their authored extent as the page minimum), drop departed ones.
    pub fn sync(&mut self, store: &SceneStore) {
        let mut live: BTreeMap<String, (String, Edge, f32, u64)> = BTreeMap::new();
        self.popup_routes.clear();
        for (name, entry) in store.scenes() {
            let tree = entry.tree();
            if crate::mount::is_dialog(tree) {
                continue;
            }
            let page = page_id(tree);
            let Some(seat) = store.pages().seat(&page) else { continue };
            let edge = scene_edge(tree);
            if matches!(edge, Edge::Left | Edge::Right)
                && let Some(owner) = entry.owner().filter(|owner| *owner == "scenes" || owner.starts_with("scenes@"))
            {
                self.popup_routes.insert((seat.output.clone(), page.clone()), (owner.into(), name.into()));
            }
            let config = mount_config(tree).unwrap_or_default();
            let extent = match edge {
                Edge::Left | Edge::Right => config["w"].as_f64().map_or(EDGE_WIDTH, |w| w as f32),
                Edge::Top | Edge::Bottom => config["h"].as_f64().map_or(EDGE_HEIGHT, |h| h as f32),
            };
            live.insert(page, (seat.output.clone(), edge, extent, seat.accepted_at));
        }
        let gone: Vec<String> = self
            .registered
            .iter()
            .filter(|(page, (output, edge))| live.get(*page).is_none_or(|(o, e, _, _)| o != output || e != edge))
            .map(|(page, _)| page.clone())
            .collect();
        for page in gone {
            if let Some((output, edge)) = self.registered.remove(&page)
                && let Some(model) = self.models.get_mut(&output)
            {
                let _ = model.carousel_mut(shell_edge(edge)).remove(&page);
                model.set_page_minimum_thickness(shell_edge(edge), &page, None);
            }
        }
        // New pages register in receipt order, as Quoin populates a carousel
        // (the first page an edge receives is the one it shows).
        let mut live: Vec<(String, (String, Edge, f32, u64))> = live.into_iter().collect();
        live.sort_by_key(|(page, (_, _, _, accepted_at))| (*accepted_at, page.clone()));
        for (page, (output, edge, extent, _)) in live {
            let Some(model) = self.models.get_mut(&output) else { continue };
            model.set_page_minimum_thickness(shell_edge(edge), &page, Some(extent));
            if !self.registered.contains_key(&page) && model.carousel_mut(shell_edge(edge)).register(&page).is_ok() {
                self.registered.insert(page, (output, edge));
            }
        }
        // A menu can precede its edge's first page. Empty-edge suppression
        // ignored MenuHold at open time; acquire it as soon as content lands.
        for model in self.models.values_mut() { model.reconcile_preferences(); }
        if let Some(menu) = &self.menu
            && let Some(model) = self.models.get_mut(&menu.output)
        {
            let _ = model.panel_input(menu.corner.summoned_edge(), model.last_update(), PanelInput::MenuHold(true));
        }
    }

    /// Advance every model to now. What the frame clock must do next:
    /// `Animate` (an edge is moving: draw the next frame), `WakeAt` (a grace
    /// or intro deadline), or `Idle`.
    pub fn tick(&mut self) -> Wake {
        let now = self.now();
        self.tick_at(now)
    }

    fn tick_at(&mut self, now: Duration) -> Wake {
        if let Some(pointer) = self.pointer.as_mut()
            && let Some(model) = self.models.get(&pointer.output)
        {
            Self::latch_conceal(model, pointer);
        }
        // Only a detector's exact dwell deadline re-samples a resting pointer.
        // No frame/poll timer is used to discover motion or start a dwell.
        if let Some(pointer) = self.pointer.as_mut()
            && pointer.detector.next_deadline().is_some_and(|at| at <= now)
        {
            pointer.sample.at = now;
            let events = pointer.detector.sample(pointer.sample).unwrap_or_default();
            if let Some(model) = self.models.get_mut(&pointer.output) {
                for event in events {
                    if let Some(edge) = Self::corner_event(model, pointer, now, event) {
                        self.pending_focus.insert(pointer.output.clone(), edge);
                    }
                }
            }
        }
        for model in self.models.values_mut() {
            let _ = model.tick(now);
        }
        // A sliding page can reach a stationary pointer. This runs only on
        // an already owed animation/deadline frame, using the saved sample;
        // it neither polls input nor samples the corner detector per frame.
        if let Some(pointer) = self.pointer.as_mut()
            && let Some(model) = self.models.get_mut(&pointer.output)
        {
            Self::pointer_membership(model, pointer, now);
        }
        self.pending_focus.retain(|output, edge| {
            self.models.get(output).is_some_and(|model| {
                let panel = model.panel(shell_edge(*edge));
                panel.mode != PanelMode::Hidden || panel.transient_revealed
            })
        });
        let mut wake = Wake::Idle;
        for model in self.models.values() {
            match model.wake() {
                PanelWake::Animate => wake = Wake::Animate,
                PanelWake::WakeAt(at) => {
                    if wake != Wake::Animate {
                        let at = self.epoch + at;
                        wake = match wake {
                            Wake::At(earlier) if earlier <= at => Wake::At(earlier),
                            _ => Wake::At(at),
                        };
                    }
                }
                PanelWake::Idle => {}
            }
        }
        if wake != Wake::Animate
            && let Some(at) = self.pointer.as_ref().and_then(|p| p.detector.next_deadline())
        {
            let at = self.epoch + at;
            wake = match wake {
                Wake::At(earlier) if earlier <= at => Wake::At(earlier),
                _ => Wake::At(at),
            };
        }
        wake
    }

    /// Output-local logical motion, before client input routing. Geometry,
    /// corner tuning and timestamps use the same units as Quoin's core.
    pub fn pointer(&mut self, output: &str, position: Option<(f32, f32)>, config: CornerDetectorConfig) {
        let now = self.now();
        self.pointer_at(output, position, config, now);
    }

    fn pointer_at(&mut self, output: &str, position: Option<(f32, f32)>, config: CornerDetectorConfig, now: Duration) {
        let position = position.filter(|(x, y)| x.is_finite() && y.is_finite());
        let changed = self.pointer.as_ref().is_some_and(|p| p.output != output || p.detector.config() != config);
        for press in self.presses.values_mut() {
            if press.as_ref().is_some_and(|press| {
                changed || press.output != output || position.is_none_or(|(x, y)| {
                    (x - press.position.x).hypot(y - press.position.y) > press.tolerance
                })
            }) {
                *press = None;
            }
        }
        if (position.is_none() || changed)
            && let Some(mut pointer) = self.pointer.take()
            && let Some(model) = self.models.get_mut(&pointer.output)
        {
            for event in pointer.detector.leave_output(now).unwrap_or_default() {
                let _ = model.corner_event(now, event);
            }
            for edge in ShellEdge::ALL {
                if model.panel(edge).pointer_inside {
                    let _ = model.panel_input(edge, now, PanelInput::PointerLeft);
                }
            }
        }
        let Some((x, y)) = position else { return };
        let Some(model) = self.models.get_mut(output) else { return };
        if let Some(pointer) = self.pointer.as_mut() {
            // Capture before ticking: the core can drop its hover latch when
            // the slide ends, before a candidate has emitted CornerEntered.
            Self::latch_conceal(model, pointer);
        }
        let _ = model.tick(now);
        let size = model.geometry();
        // Hardware accumulators may sit exactly on the right/bottom extent;
        // the core detector's output rectangle is half-open.
        let point = LogicalPoint::new(x.clamp(0.0, (size.width() - 0.001).max(0.0)), y.clamp(0.0, (size.height() - 0.001).max(0.0)));
        let sample = PointerSample::new(now, point, size);
        let pointer = self.pointer.get_or_insert_with(|| Pointer {
            output: output.to_owned(), sample, detector: CornerDetector::new(config),
            reveal_latched: [None; 4],
        });
        pointer.sample = sample;
        let events = pointer.detector.sample(sample).unwrap_or_default();
        let diagnostics = pointer.detector.diagnostics(now);
        let corner_edge = diagnostics.candidate.or(diagnostics.engaged).map(Corner::summoned_edge);
        for edge in ShellEdge::ALL {
            if let Some(extent) = pointer.reveal_latched[edge.index()] {
                let distance = match edge {
                    ShellEdge::Left => point.x, ShellEdge::Right => size.width() - point.x,
                    ShellEdge::Top => point.y, ShellEdge::Bottom => size.height() - point.y,
                };
                // Only hardware motion/departure releases this latch. Keep
                // the old panel bounds so its outgoing slide cannot do so.
                if corner_edge != Some(edge) && !(extent > 0.0 && distance <= extent) {
                    pointer.reveal_latched[edge.index()] = None;
                }
            }
        }
        for event in events {
            if let Some(edge) = Self::corner_event(model, pointer, now, event) {
                self.pending_focus.insert(output.into(), edge);
            }
        }
        Self::pointer_membership(model, pointer, now);
    }

    fn latch_conceal(model: &ShellModel, pointer: &mut Pointer) {
        for edge in ShellEdge::ALL {
            let panel = model.panel(edge);
            if panel.mode != PanelMode::Hidden || panel.transient_revealed {
                // An explicit reveal or mode change overrides a deliberate hide.
                pointer.reveal_latched[edge.index()] = None;
            } else if panel.hover_latched && (panel.pointer_inside || panel.corner_inside) {
                pointer.reveal_latched[edge.index()].get_or_insert(
                    panel.thickness_px * panel.visible_fraction.clamp(0.0, 1.0),
                );
            }
        }
    }

    fn corner_event(model: &mut ShellModel, pointer: &Pointer, now: Duration, event: CornerEvent) -> Option<Edge> {
        if let CornerEvent::Entered { corner, .. } = event
            && pointer.reveal_latched[corner.summoned_edge().index()].is_some()
        {
            return None;
        }
        let revealed = match event {
            CornerEvent::Entered { corner, .. } => {
                let edge = corner.summoned_edge();
                let panel = model.panel(edge);
                (panel.mode == PanelMode::Hidden && !panel.transient_revealed).then_some(edge)
            }
            _ => None,
        };
        let _ = model.corner_event(now, event);
        // Installed launcher scenes need not author window.autofocus. A new
        // hotspot reveal also requests the first field after the slide settles.
        revealed.filter(|edge| model.panel(*edge).transient_revealed).map(seat_edge)
    }

    fn pointer_membership(model: &mut ShellModel, pointer: &Pointer, now: Duration) {
        let diagnostics = pointer.detector.diagnostics(now);
        let corner_edge = diagnostics.candidate.or(diagnostics.engaged).map(Corner::summoned_edge);
        let point = pointer.sample.position;
        let size = model.geometry();
        for edge in ShellEdge::ALL {
            let panel = model.panel(edge);
            // A deliberate hide owns the old visible bounds until hardware
            // departure. Its outgoing slide must not reacquire a pointer hold.
            if pointer.reveal_latched[edge.index()].is_some() {
                if panel.pointer_inside {
                    let _ = model.panel_input(edge, now, PanelInput::PointerLeft);
                }
                continue;
            }
            let visible = panel.thickness_px * panel.visible_fraction.clamp(0.0, 1.0);
            let distance = match edge {
                ShellEdge::Left => point.x, ShellEdge::Right => size.width() - point.x,
                ShellEdge::Top => point.y, ShellEdge::Bottom => size.height() - point.y,
            };
            // Only the corner square can summon an edge. Membership retains
            // an exposed panel, including re-entry to its hotspot during grace;
            // PointerEntered alone never reveals a hidden panel before dwell.
            let inside = corner_edge == Some(edge) || (panel.mapped && visible > 0.0 && distance <= visible);
            if inside != panel.pointer_inside {
                let input = if inside { PanelInput::PointerEntered } else { PanelInput::PointerLeft };
                let _ = model.panel_input(edge, now, input);
            }
        }
    }

    /// Topmost hotspot button ownership; no dwell is required to press.
    /// Call after sampling the hardware pointer, before any client/furniture.
    pub fn pointer_button(&mut self, button: u32, pressed: bool, shift: bool) -> bool {
        self.pointer_button_at(button, pressed, shift, self.now())
    }

    fn pointer_button_at(&mut self, button: u32, pressed: bool, shift: bool, now: Duration) -> bool {
        if pressed {
            if self.presses.contains_key(&button) {
                return true;
            }
            if let Some(menu) = &self.menu {
                let inside = self.pointer.as_ref().is_some_and(|pointer| {
                    let p = pointer.sample.position;
                    menu.contains(&pointer.output, p.x, p.y)
                });
                if inside {
                    // The menu's iced buttons own this complete click.
                    return false;
                }
                self.close_menu_at(now);
                self.presses.insert(button, None);
                return true;
            }
            let Some(pointer) = self.pointer.as_ref() else { return false };
            // The core's deadzone is a square: BOTH adjacent edge distances
            // must be <= its logical size (default 10), never an edge band.
            let diagnostics = pointer.detector.diagnostics(now);
            let Some(corner) = diagnostics.candidate.or(diagnostics.engaged) else { return false };
            self.presses.insert(button, Some(HotspotPress {
                output: pointer.output.clone(), corner, position: pointer.sample.position,
                shift, tolerance: pointer.detector.config().deadzone_px(),
            }));
            return true;
        }
        let Some(press) = self.presses.remove(&button) else { return false };
        if let Some(press) = press {
            match button {
                0x110 => {
                    if !press.shift {
                        let route = self.models.get(&press.output)
                            .and_then(|model| model.carousel(press.corner.summoned_edge()).active_id())
                            .and_then(|page| self.popup_routes.get(&(press.output.clone(), page.into())))
                            .cloned();
                        if let Some(route) = route {
                            // Do not change mode first: that would make the
                            // loader save our pin as its previous mode, so its
                            // next close restores pinned and cannot conceal.
                            self.popup_commands.push(route);
                            return true;
                        }
                    }
                    if let Some(model) = self.models.get(&press.output) {
                        let edge = press.corner.summoned_edge();
                        let mode = model.panel(edge).mode;
                        let next = if press.shift {
                            if mode == PanelMode::Docked { PanelMode::Hidden } else { PanelMode::Docked }
                        } else if mode == PanelMode::Pinned {
                            PanelMode::Hidden
                        } else {
                            PanelMode::Pinned
                        };
                        // Explicit modes: the pinned core's legacy Clicked
                        // docks, and its PinToggle releases into transience.
                        let _ = self.set_mode_at(&press.output, seat_edge(edge), next, now);
                    }
                }
                0x111 => {
                    let _ = self.open_menu_at(&press.output, press.corner, now);
                }
                _ => {}
            }
        }
        true
    }

    pub(crate) fn popup_commands(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.popup_commands)
    }

    pub fn menu(&self) -> Option<&crate::menu::Menu> { self.menu.as_ref() }

    pub fn open_menu(&mut self, output: &str, corner: Corner) -> Result<(), Refusal> {
        self.open_menu_at(output, corner, self.now())
    }

    fn open_menu_at(&mut self, output: &str, corner: Corner, now: Duration) -> Result<(), Refusal> {
        if !self.models.contains_key(output) {
            return Err(Refusal { code: "UNKNOWN_OUTPUT", message: "corner menu requires a known output".into() });
        }
        self.close_menu_at(now);
        let edge = corner.summoned_edge();
        let path = self.menu_conf.clone().unwrap_or_else(crate::conf::conf_mix_path);
        let extras = crate::menu::read_extras(&path, edge.as_str()).unwrap_or_else(|error| {
            tracing::warn!("scene host: corner menu config {}: {error}", path.display());
            Vec::new()
        });
        let model = self.models.get_mut(output).expect("known output");
        // Acquire BEFORE advancing any grace deadline. MenuHold is separate
        // from mode and does not itself reveal a hidden panel.
        model.panel_input(edge, now, PanelInput::MenuHold(true))
            .map_err(|error| Refusal { code: "PANEL_NOT_APPLIED", message: error.to_string() })?;
        let size = model.geometry();
        self.menu_serial = self.menu_serial.checked_add(1).expect("menu serial exhausted");
        let inset = self.pointer.as_ref().map_or(12.0, |p| p.detector.config().deadzone_px() + 2.0);
        let mut menu = crate::menu::Menu::new(self.menu_serial, output, corner, model.panel(edge).mode,
            extras, (size.width(), size.height()), inset);
        if model.panel(edge).transient_revealed { menu.items[2].checked = false; }
        self.menu = Some(menu);
        self.refresh_menu_mode();
        Ok(())
    }

    pub fn close_menu(&mut self) { self.close_menu_at(self.now()); }

    fn close_menu_at(&mut self, now: Duration) {
        if let Some(menu) = self.menu.take()
            && let Some(model) = self.models.get_mut(&menu.output)
        {
            let held = self.keyboard.get(&menu.output).copied() == Some(seat_edge(menu.corner.summoned_edge()));
            let _ = model.panel_input(menu.corner.summoned_edge(), now, PanelInput::MenuHold(held));
        }
    }

    /// Human and Bus choices share this path. A serial fences queued iced
    /// events and agent choices against a dismissed/replaced menu.
    pub fn menu_input(&mut self, serial: u64, input: crate::menu::Input) -> Result<Option<crate::menu::Extra>, Refusal> {
        self.menu_input_at(serial, input, self.now())
    }

    fn menu_input_at(&mut self, serial: u64, input: crate::menu::Input, now: Duration) -> Result<Option<crate::menu::Extra>, Refusal> {
        use crate::menu::{Choice, Input, Item};
        let menu = self.menu.as_mut().filter(|menu| menu.serial == serial)
            .ok_or_else(|| Refusal { code: "STALE_MENU", message: "corner menu is closed or its serial changed".into() })?;
        let index = match input {
            Input::Close => { self.close_menu_at(now); return Ok(None) }
            Input::Move(direction) => { menu.navigate(direction.signum()); return Ok(None) }
            Input::Activate => menu.selected,
            Input::Choose(index) => index,
        };
        let item = menu.items.get(index).filter(|item| item.enabled()).cloned()
            .ok_or_else(|| Refusal { code: "BAD_ARGUMENT", message: "choose an enabled menu item index".into() })?;
        let (output, edge) = (menu.output.clone(), menu.corner.summoned_edge());
        let extra = match item.choice {
            Choice::Mode(mode) => {
                if self.models[&output].preference(edge).is_some() {
                    return Err(settings_managed());
                }
                if mode == PanelMode::Hidden {
                    // Dismiss the popup first: Hide is intentionally ignored
                    // while MenuHold is acquired. Keep the old visible bounds
                    // before either the core or its outgoing animation ticks.
                    if let Some(pointer) = self.pointer.as_mut().filter(|p| p.output == output) {
                        let panel = self.models[&output].panel(edge);
                        if panel.pointer_inside || panel.corner_inside {
                            pointer.reveal_latched[edge.index()] = Some(panel.thickness_px * panel.visible_fraction.clamp(0.0, 1.0));
                        }
                    }
                    self.focus_at(&output, None, now);
                    self.close_menu_at(now);
                    let model = self.models.get_mut(&output).expect("menu output");
                    let before = model.panel(edge).mode;
                    model.panel_input(edge, now, PanelInput::SetMode(PanelMode::Hidden))
                        .and_then(|_| model.panel_input(edge, now, PanelInput::Hide))
                        .map_err(|error| Refusal { code: "PANEL_NOT_APPLIED", message: error.to_string() })?;
                    if before != PanelMode::Hidden { self.save_state(&output); }
                    return Ok(None);
                }
                let model = self.models.get_mut(&output).expect("menu output");
                let before = model.panel(edge).mode;
                if mode != PanelMode::Hidden && model.carousel(edge).page_ids().is_empty() {
                    return Err(Refusal { code: "EMPTY_EDGE", message: "edge has no registered pages".into() });
                }
                // Apply while MenuHold is still acquired, then dismiss.
                model.panel_input(edge, now, PanelInput::SetMode(mode))
                    .map_err(|error| Refusal { code: "PANEL_NOT_APPLIED", message: error.to_string() })?;
                if model.panel(edge).mode != before { self.save_state(&output); }
                None
            }
            Choice::Extra(mut extra) => {
                if let Some(question) = extra.confirm.take() {
                    self.menu_serial = self.menu_serial.checked_add(1).expect("menu serial exhausted");
                    menu.serial = self.menu_serial;
                    menu.items = vec![
                        Item { label: question, checked: false, disabled: false, choice: Choice::Inert },
                        Item { label: extra.label.clone(), checked: false, disabled: false, choice: Choice::Extra(extra) },
                        Item { label: "Cancel".into(), checked: false, disabled: false, choice: Choice::Cancel },
                    ];
                    menu.selected = 1;
                    let size = self.sizes[&output];
                    menu.fit(size, menu.inset);
                    return Ok(None);
                }
                Some(extra)
            }
            Choice::Cancel | Choice::Inert => None,
        };
        self.close_menu_at(now);
        Ok(extra)
    }

    /// What `output`'s `edge` draws, if anything.
    pub fn drawn(&self, output: &str, edge: Edge) -> Option<Drawn> {
        let model = self.models.get(output)?;
        let panel = model.panel(shell_edge(edge));
        if !panel.mapped || panel.visible_fraction <= 0.0 {
            return None;
        }
        let page = model.carousel(shell_edge(edge)).active_id()?.to_owned();
        Some(Drawn { page, thickness: panel.thickness_px, fraction: panel.visible_fraction.clamp(0.0, 1.0) })
    }

    /// Size a resident page as if selected, without changing the carousel.
    /// Calendar and notes have different authored widths; using the active
    /// page's width for both would resize both renderers on every switch.
    pub(crate) fn page_thickness(&self, output: &str, edge: Edge, page: &str) -> Option<f32> {
        let model = self.models.get(output)?;
        if model.carousel(shell_edge(edge)).active_id() == Some(page) {
            return Some(model.panel(shell_edge(edge)).thickness_px);
        }
        let mut measuring = model.clone();
        measuring.carousel_mut(shell_edge(edge)).select_id(page);
        Some(measuring.panel(shell_edge(edge)).thickness_px)
    }

    /// The space each edge of `output` reserves (logical px; only a DOCKED
    /// edge reserves, as in Quoin), for the usable area.
    pub fn zones(&self, output: &str) -> Vec<(Edge, f32)> {
        let Some(model) = self.models.get(output) else { return Vec::new() };
        EDGES
            .into_iter()
            .map(|edge| (edge, model.panel(shell_edge(edge)).exclusive_zone_px))
            .filter(|(_, px)| *px > 0.0)
            .collect()
    }

    /// The `panels.<edge>` row (Quoin `panel_notice_snapshot`).
    fn row(&self, output: &str, model: &ShellModel, edge: Edge) -> Value {
        let panel: PanelSnapshot = model.panel(shell_edge(edge));
        let carousel = model.carousel(shell_edge(edge));
        let managed = self.preferences.record(shell_edge(edge)).map(|record| {
            let requested = record.preference.thickness();
            let range = resize_thickness_range(shell_edge(edge));
            let range_fitted = requested.clamp(*range.start(), *range.end());
            let mut constraints = Vec::new();
            if range_fitted != requested { constraints.push("edge_range"); }
            if panel.settled_thickness_px < range_fitted { constraints.push("output_budget"); }
            if panel.thickness_px > panel.settled_thickness_px { constraints.push("page_minimum"); }
            if carousel.page_ids().is_empty() { constraints.push("no_pages"); }
            json!({"record":record.id, "requested_px":requested,
                "fitted_px":panel.settled_thickness_px, "presented_px":panel.thickness_px,
                "reserved_px":panel.exclusive_zone_px, "constraints":constraints})
        });
        json!({
            "visible": panel.mapped,
            "pinned": panel.mode != PanelMode::Hidden,
            "mode": panel.mode.as_str(),
            "width_px": panel.settled_thickness_px,
            "page": carousel.active_id(),
            "pages": carousel.page_ids(),
            "declared": self.declared[shell_edge(edge).index()],
            "output": output,
            "settings": managed,
        })
    }

    /// Take conf.mix's page order (Quoin `redeclare_shell_pages`): every
    /// edge is validated first, then every output's carousels reorder without
    /// losing a registered page or the selection.
    pub fn declare(&mut self, declared: crate::conf::Declared) -> Result<(), Refusal> {
        for names in &declared {
            Carousel::declared(names.iter().cloned())
                .map_err(|error| Refusal { code: "INVALID_ARGUMENT", message: error.to_string() })?;
        }
        for model in self.models.values_mut() {
            for edge in ShellEdge::ALL {
                model
                    .carousel_mut(edge)
                    .redeclare(declared[edge.index()].iter().cloned())
                    .map_err(|error| Refusal { code: "INVALID_ARGUMENT", message: error.to_string() })?;
            }
        }
        self.declared = declared;
        Ok(())
    }

    /// `shell.panel.resize {edge, thickness_px}` on `output`, as Quoin's
    /// scripted `ResizeCommit`: start, resize, then complete (settling the new
    /// thickness, which the row reports as `width_px` and a docked edge
    /// reserves). Refusals leave the model untouched, in Quoin's shapes.
    pub fn resize(&mut self, output: &str, edge: Edge, thickness_px: f32) -> Result<(), Value> {
        let shell = shell_edge(edge);
        if let Some(model) = self.models.get(output) && model.preference(shell).is_some() {
            return if model.panel(shell).settled_thickness_px == thickness_px { Ok(()) }
                else { let refusal = settings_managed(); Err(json!({"error_code":refusal.code,"message":refusal.message})) };
        }
        let range = resize_thickness_range(shell);
        if !range.contains(&thickness_px) {
            return Err(resize_range_refusal(edge));
        }
        let now = self.now();
        let Some(model) = self.models.get_mut(output) else {
            return Err(json!({"error_code":"EMPTY_EDGE", "message":"edge has no registered pages", "edge":edge.as_str()}));
        };
        let max = model.max_thickness(shell);
        if thickness_px > max {
            return Err(json!({"error_code":"PANEL_THICKNESS_BUDGET", "error":"panel thickness exceeds output budget",
                "edge":edge.as_str(), "requested":thickness_px, "max":max}));
        }
        // Prepare the whole resize before applying it: a refusal must not
        // arm concealment deadlines that the caller will not schedule.
        let mut resized = model.clone();
        let _ = resized.panel_input(shell, now, PanelInput::ResizeStarted);
        match resized.resize_thickness(shell, thickness_px) {
            Ok(()) => {
                let _ = resized.panel_input(shell, now, PanelInput::ResizeCompleted);
                *model = resized;
                self.save_state(output);
                Ok(())
            }
            Err(error) => Err(resize_config_refusal(edge, thickness_px, max, error)),
        }
    }

    /// A semantic panel verb on `output`'s `edge`. An input that would show
    /// content on an edge with no pages refuses `EMPTY_EDGE` (Quoin's
    /// scene-only frame).
    pub fn input(&mut self, output: &str, edge: Edge, verb: PanelVerb) -> Result<(), Refusal> {
        self.input_at(output, edge, verb, self.now())
    }

    fn input_at(&mut self, output: &str, edge: Edge, verb: PanelVerb, now: Duration) -> Result<(), Refusal> {
        if let Some(model) = self.models.get(output) && model.preference(shell_edge(edge)).is_some() {
            let mode = model.panel(shell_edge(edge)).mode;
            match verb {
                PanelVerb::Pin => return if mode == PanelMode::Pinned { Ok(()) } else { Err(settings_managed()) },
                PanelVerb::Dock => return if mode == PanelMode::Docked { Ok(()) } else { Err(settings_managed()) },
                PanelVerb::Unpin => return if mode == PanelMode::Hidden { Ok(()) } else { Err(settings_managed()) },
                PanelVerb::Toggle if mode != PanelMode::Hidden => return Ok(()),
                _ => {}
            }
        }
        // A toggle decides against the actual reveal, including pointer and
        // keyboard holders. Release focus before asking the core to conceal.
        let closing = verb == PanelVerb::Hide || (verb == PanelVerb::Toggle && !self.hidden(output, edge));
        let input = if closing && verb == PanelVerb::Toggle && self.mode(output, edge) == "hidden" {
            PanelInput::Hide
        } else { verb.input() };
        if closing {
            self.pending_focus.remove(output);
        }
        if closing && self.focused(output) == Some(edge) {
            // A scene's explicit close (e.g. after launching an app) must
            // release its keyboard holder before the local Hide can apply.
            self.focus_at(output, None, now);
        }
        let empty = || Refusal { code: "EMPTY_EDGE", message: "edge has no registered pages".into() };
        let model = self.models.get_mut(output).ok_or_else(empty)?;
        if input.requires_content() && model.carousel(shell_edge(edge)).page_ids().is_empty() {
            return Err(empty());
        }
        let before = model.panel(shell_edge(edge)).mode;
        model
            .panel_input(shell_edge(edge), now, input)
            .map(|_| ())
            .map_err(|error| Refusal { code: "PANEL_NOT_APPLIED", message: error.to_string() })?;
        if let Some(pointer) = self.pointer.as_mut().filter(|p| p.output == output) {
            Self::latch_conceal(model, pointer);
            if !closing { pointer.reveal_latched[shell_edge(edge).index()] = None; }
        }
        if model.panel(shell_edge(edge)).mode != before { self.save_state(output); }
        if !closing && matches!(verb, PanelVerb::Show | PanelVerb::Toggle) {
            self.pending_focus.insert(output.into(), edge);
            self.focus_at(output, Some(edge), now);
        }
        Ok(())
    }

    pub(crate) fn focus_requested(&self, output: &str) -> Option<Edge> { self.pending_focus.get(output).copied() }
    pub(crate) fn focus_granted(&mut self, output: &str) { self.pending_focus.remove(output); }

    /// Whether a hide is observable: the edge is in `hidden` mode and no
    /// transient reveal holds it (Quoin `reply_panels`).
    pub fn hidden(&self, output: &str, edge: Edge) -> bool {
        self.models.get(output).is_none_or(|model| {
            let panel = model.panel(shell_edge(edge));
            panel.mode == PanelMode::Hidden && !panel.transient_revealed
        })
    }

    /// The edge's mode name (`hidden` with no model).
    pub fn mode(&self, output: &str, edge: Edge) -> &'static str {
        self.models.get(output).map_or("hidden", |model| model.panel(shell_edge(edge)).mode.as_str())
    }

    /// `shell.panel.state {edge}` on `output`.
    pub fn state(&self, output: &str, edge: Edge) -> Value {
        let Some(model) = self.models.get(output) else {
            return json!({"visible":false, "pinned":false, "mode":"hidden", "width_px":0, "page":null,
                "pages":[], "declared":[], "output":output, "keyboard_focused":false, "keyboard_requested":false});
        };
        let mut row = self.row(output, model, edge);
        row["keyboard_focused"] = json!(self.keyboard.get(output) == Some(&edge) && self.pending_focus.get(output) != Some(&edge));
        row["keyboard_requested"] = json!(self.keyboard.get(output) == Some(&edge) || self.pending_focus.get(output) == Some(&edge));
        row
    }

    pub(crate) fn focus(&mut self, output: &str, edge: Option<Edge>) {
        self.focus_at(output, edge, self.now());
    }

    pub(crate) fn focused(&self, output: &str) -> Option<Edge> { self.keyboard.get(output).copied() }

    fn focus_at(&mut self, output: &str, edge: Option<Edge>, now: Duration) {
        if self.pending_focus.get(output).copied() != edge { self.pending_focus.remove(output); }
        let previous = self.keyboard.get(output).copied();
        if previous == edge { return; }
        if let Some(edge) = edge { self.keyboard.insert(output.into(), edge); }
        else { self.keyboard.remove(output); }
        let Some(model) = self.models.get_mut(output) else { return };
        model.keyboard_focus_observed(edge.map(shell_edge));
        for edge in [previous, edge].into_iter().flatten() {
            let popup = self.menu.as_ref().is_some_and(|m| m.output == output && m.corner.summoned_edge() == shell_edge(edge));
            let focused = self.keyboard.get(output) == Some(&edge);
            let panel = model.panel(shell_edge(edge));
            // Keyboard focus retains only a transient reveal. Persistent
            // modes own visibility and must never inherit a focus holder.
            let held = focused && panel.mode == PanelMode::Hidden && panel.transient_revealed;
            let _ = model.panel_input(shell_edge(edge), now, PanelInput::MenuHold(popup || held));
            if !focused && !popup {
                // Focus loss is deliberate, so it does not wait for pointer
                // grace and latches against a pointer still on the panel.
                let _ = model.panel_input(shell_edge(edge), now, PanelInput::Hide);
            }
        }
        if let Some(pointer) = self.pointer.as_mut().filter(|p| p.output == output) {
            Self::latch_conceal(model, pointer);
        }
    }

    pub(crate) fn escape(&mut self, output: &str, edge: Edge) {
        let now = self.now();
        self.pending_focus.remove(output);
        self.focus_at(output, None, now);
        if let Some(model) = self.models.get_mut(output) {
            let _ = model.panel_input(shell_edge(edge), now, PanelInput::Escape);
            if let Some(pointer) = self.pointer.as_mut().filter(|p| p.output == output) {
                Self::latch_conceal(model, pointer);
            }
        }
    }

    /// `{dialog, panels}` for `output` (`shell.props.get` with no path).
    pub fn snapshot(&self, output: &str, dialog: Value) -> Value {
        let mut panels = serde_json::Map::new();
        if let Some(model) = self.models.get(output) {
            for edge in EDGES {
                panels.insert(edge.as_str().into(), self.row(output, model, edge));
            }
        } else {
            for edge in EDGES {
                panels.insert(edge.as_str().into(), self.state(output, edge));
            }
        }
        json!({"dialog": dialog, "panels": panels,
            "menu":self.menu.as_ref().map(crate::menu::Menu::snapshot)})
    }

    /// `shell.panel.mode {edge, mode}`: applied once the model reads that
    /// mode. An edge with no pages refuses (`EMPTY_EDGE`, Quoin's
    /// scene-only frame).
    pub fn set_mode(&mut self, output: &str, edge: Edge, mode: &str) -> Result<(), Refusal> {
        let Some(mode) = PanelMode::parse(mode) else {
            return Err(Refusal { code: "BAD_ARGUMENT", message: "mode must be hidden, pinned or docked".into() });
        };
        self.set_mode_at(output, edge, mode, self.now())
    }

    fn set_mode_at(&mut self, output: &str, edge: Edge, mode: PanelMode, now: Duration) -> Result<(), Refusal> {
        if let Some(model) = self.models.get(output) && model.preference(shell_edge(edge)).is_some() {
            return if model.panel(shell_edge(edge)).mode == mode { Ok(()) } else { Err(settings_managed()) };
        }
        // Validate before cancelling anything: a refused write has no effects.
        let model = self.model_with_pages(output, edge)?;
        let shell = shell_edge(edge);
        let before = model.panel(shell);
        if let Some(pointer) = self.pointer.as_mut().filter(|p| p.output == output) {
            let diagnostics = pointer.detector.diagnostics(now);
            let at_corner = diagnostics.candidate.or(diagnostics.engaged).is_some_and(|c| c.summoned_edge() == shell);
            if mode == PanelMode::Hidden && (before.pointer_inside || before.corner_inside || at_corner) {
                pointer.reveal_latched[shell.index()].get_or_insert(before.thickness_px * before.visible_fraction.clamp(0.0, 1.0));
            } else if mode != PanelMode::Hidden {
                pointer.reveal_latched[shell.index()] = None;
            }
        }
        if self.pending_focus.get(output) == Some(&edge) { self.pending_focus.remove(output); }
        if self.keyboard.get(output) == Some(&edge) {
            self.keyboard.remove(output);
            self.models.get_mut(output).expect("known output").keyboard_focus_observed(None);
        }
        if self.menu.as_ref().is_some_and(|m| m.output == output && m.corner.summoned_edge() == shell) {
            self.close_menu_at(now);
        }
        let model = self.models.get_mut(output).expect("known output");
        // Drop the old reveal's holders, then apply the persistent mode even
        // when it is unchanged. The host latch fences a pending corner dwell
        // and pointer re-entry during the outgoing hide animation.
        for input in [PanelInput::MenuHold(false), PanelInput::ResizeCancelled, PanelInput::PointerLeft, PanelInput::CornerLeft] {
            model.panel_input(shell, now, input)
                .map_err(|error| Refusal { code: "PANEL_NOT_APPLIED", message: error.to_string() })?;
        }
        model
            .set_mode(shell, now, mode)
            .map_err(|error| Refusal { code: "PANEL_NOT_APPLIED", message: error.to_string() })?;
        if mode == PanelMode::Hidden {
            // SetMode on an already-hidden, transiently revealed edge need
            // not change the core mode. An explicit restore still conceals.
            model.panel_input(shell, now, PanelInput::Hide)
                .map_err(|error| Refusal { code: "PANEL_NOT_APPLIED", message: error.to_string() })?;
        }
        if model.panel(shell).mode == mode {
            if before.mode != mode { self.save_state(output); }
            if mode != PanelMode::Hidden && matches!(edge, Edge::Left | Edge::Right) {
                // Ask the renderer to grant seat/widget focus only after the
                // final viewport settles; this is not a visibility holder.
                self.pending_focus.insert(output.into(), edge);
            }
            Ok(())
        } else {
            Err(Refusal { code: "PANEL_NOT_APPLIED", message: "panel command was superseded or could not apply".into() })
        }
    }

    /// `shell.panel.page.set {edge, id}`: select a registered page.
    pub fn page_set(&mut self, output: &str, edge: Edge, id: &str) -> Result<(), Refusal> {
        let model = self.model_with_pages(output, edge)?;
        let carousel = model.carousel_mut(shell_edge(edge));
        if !carousel.page_ids().iter().any(|page| page == id) {
            return Err(Refusal { code: "UNKNOWN_PAGE", message: "unknown page id for this edge".into() });
        }
        let changed = carousel.active_id() != Some(id);
        carousel.select_id(id);
        if carousel.active_id() == Some(id) {
            self.save_state(output);
            if changed && (self.focused(output) == Some(edge)
                || (self.mode(output, edge) != "hidden" && matches!(edge, Edge::Left | Edge::Right))) {
                self.pending_focus.insert(output.into(), edge);
            }
            Ok(())
        } else {
            Err(Refusal { code: "PANEL_NOT_APPLIED", message: "panel command was superseded or could not apply".into() })
        }
    }

    fn model_with_pages(&mut self, output: &str, edge: Edge) -> Result<&mut ShellModel, Refusal> {
        let model = self
            .models
            .get_mut(output)
            .ok_or_else(|| Refusal { code: "EMPTY_EDGE", message: "edge has no registered pages".into() })?;
        if model.carousel(shell_edge(edge)).page_ids().is_empty() {
            return Err(Refusal { code: "EMPTY_EDGE", message: "edge has no registered pages".into() });
        }
        Ok(model)
    }

    /// The `shell.panel.changed` wire when `output`'s rows or the dialog seat
    /// differ from the last published ones (Quoin `publish_panel_state`):
    /// `{dialog, panels, generation, revision}`. `None` when unchanged.
    pub fn notice(&mut self, output: &str, dialog: Value, generation: Option<u64>) -> Option<String> {
        let snapshot = self.snapshot(output, dialog);
        if self.applied.get(output) == Some(&snapshot) {
            return None;
        }
        self.revision = self.revision.saturating_add(1);
        let mut body = snapshot.clone();
        body["generation"] = json!(generation);
        body["revision"] = json!(self.revision);
        self.applied.insert(output.to_owned(), snapshot);
        Some(format!("---\ncommand: shell.panel.changed\n---\n{body}"))
    }

    /// Changed snapshots for every model, including the selected output.
    pub fn notices(&mut self, output: &str, dialog: Value, generation: Option<u64>) -> Vec<String> {
        let mut outputs: Vec<String> = self.models.keys().cloned().collect();
        if !self.models.contains_key(output) {
            outputs.push(output.to_owned());
        }
        outputs.into_iter().filter_map(|output| self.notice(&output, dialog.clone(), generation)).collect()
    }
}

fn settings_managed() -> Refusal {
    Refusal { code: "SETTINGS_MANAGED", message: "panel mode and thickness are owned by the settings profile".into() }
}

pub fn resize_range_refusal(edge: Edge) -> Value {
    let range = resize_thickness_range(shell_edge(edge));
    json!({
        "error": format!("thickness_px must be a number in {}..={} for the {} edge", range.start(), range.end(), edge.as_str()),
        "range_px": [range.start(), range.end()],
    })
}

fn resize_config_refusal(edge: Edge, requested: f32, max: f32, error: PanelConfigError) -> Value {
    match error {
        PanelConfigError::PageMinimum { minimum, .. } => json!({
            "error_code":"PAGE_MINIMUM", "message":format!("{error}; resize to at least {minimum} or show another page"),
            "minimum_px":minimum, "edge":edge.as_str(), "requested":requested,
        }),
        PanelConfigError::ThicknessBudget { requested, max, .. } => json!({
            "error_code":"PANEL_THICKNESS_BUDGET", "error":error.to_string(),
            "edge":edge.as_str(), "requested":requested, "max":max,
        }),
        _ => json!({"error_code":"PANEL_RESIZE_REJECTED", "error":error.to_string(),
            "edge":edge.as_str(), "requested":requested, "max":max}),
    }
}

/// What the frame clock owes the panels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wake {
    Idle,
    Animate,
    At(Instant),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panels_with(output: &str) -> Panels {
        let mut panels = Panels::default();
        panels.ensure(output, (1280.0, 800.0));
        panels
    }

    fn register(panels: &mut Panels, output: &str, edge: Edge, page: &str, extent: f32) {
        let model = panels.models.get_mut(output).unwrap();
        model.set_page_minimum_thickness(shell_edge(edge), page, Some(extent));
        model.carousel_mut(shell_edge(edge)).register(page).unwrap();
        model.reconcile_preferences();
        panels.registered.insert(page.into(), (output.into(), edge));
    }

    #[test]
    fn settings_policy_fans_out_and_reports_requested_and_presented_sizes() {
        let mut panels = panels_with("DP-1");
        panels.set_preferences(crate::preferences::Preferences::prepare(&settings::Shell::default()).unwrap());
        panels.ensure("DP-2", (1920.0, 1080.0));
        for output in ["DP-1", "DP-2"] {
            assert_eq!(panels.mode(output, Edge::Bottom), "docked");
            assert!(panels.zones(output).is_empty());
            register(&mut panels, output, Edge::Bottom, &format!("page-{output}"), 52.0);
            let row = panels.state(output, Edge::Bottom);
            assert_eq!(row["settings"]["requested_px"], 40.0);
            assert_eq!(row["settings"]["fitted_px"], 40.0);
            assert_eq!(row["settings"]["presented_px"], 52.0);
            assert_eq!(panels.zones(output), vec![(Edge::Bottom, 52.0)]);
        }
    }

    #[test]
    fn managed_refusals_and_idempotent_writes_preserve_menu_and_keyboard() {
        let mut panels = panels_with("DP-1");
        register(&mut panels, "DP-1", Edge::Bottom, "page", 52.0);
        let mut shell = settings::Shell::default();
        panels.set_preferences(crate::preferences::Preferences::prepare(&shell).unwrap());
        let now = panels.now();
        panels.focus_at("DP-1", Some(Edge::Bottom), now);
        panels.open_menu_at("DP-1", Corner::BottomLeft, now).unwrap();
        let menu = panels.menu.clone();
        let row = panels.state("DP-1", Edge::Bottom);
        assert_eq!(panels.set_mode("DP-1", Edge::Bottom, "pinned").unwrap_err().code, "SETTINGS_MANAGED");
        panels.set_mode("DP-1", Edge::Bottom, "docked").unwrap();
        panels.input("DP-1", Edge::Bottom, PanelVerb::Toggle).unwrap();
        panels.resize("DP-1", Edge::Bottom, 40.0).unwrap();
        assert_eq!(panels.resize("DP-1", Edge::Bottom, 80.0).unwrap_err()["error_code"], "SETTINGS_MANAGED");
        assert_eq!(panels.menu, menu);
        assert_eq!(panels.focused("DP-1"), Some(Edge::Bottom));
        assert_eq!(panels.state("DP-1", Edge::Bottom), row);
        assert!(panels.menu.as_ref().unwrap().items.iter().filter(|item| matches!(item.choice, crate::menu::Choice::Mode(_))).all(|item| !item.enabled()));
        shell.panels.get_mut("bottom").unwrap().thickness = 80;
        panels.set_preferences(crate::preferences::Preferences::prepare(&shell).unwrap());
        assert_eq!(panels.focused("DP-1"), Some(Edge::Bottom));
        assert_eq!(panels.menu.as_ref().unwrap().serial, menu.as_ref().unwrap().serial);
        shell.panels.get_mut("bottom").unwrap().mode = "hidden".into();
        panels.set_preferences(crate::preferences::Preferences::prepare(&shell).unwrap());
        assert_eq!(panels.focused("DP-1"), None);
        assert!(panels.menu.is_none());
    }

    #[test]
    fn startup_modes_are_hidden_and_no_edges_draw() {
        let mut panels = panels_with("DP-1");
        for edge in EDGES {
            assert_eq!(panels.mode("DP-1", edge), "hidden");
            assert!(panels.drawn("DP-1", edge).is_none(), "empty edges never draw");
            register(&mut panels, "DP-1", edge, shell_edge(edge).as_str(), 100.0);
        }
        panels.tick_at(Duration::from_secs(1));
        for edge in EDGES {
            assert_eq!(panels.mode("DP-1", edge), "hidden");
            assert!(panels.drawn("DP-1", edge).is_none(), "hidden edges never draw");
        }
    }

    #[test]
    fn a_hidden_edge_draws_its_active_page_once_pinned() {
        let mut panels = panels_with("DP-1");
        register(&mut panels, "DP-1", Edge::Right, "scene-calendar", 360.0);
        register(&mut panels, "DP-1", Edge::Right, "scene-notes", 360.0);
        panels.set_mode("DP-1", Edge::Right, "hidden").unwrap();
        let row = panels.state("DP-1", Edge::Right);
        assert_eq!((row["mode"].as_str(), row["page"].as_str()), (Some("hidden"), Some("scene-calendar")));
        assert_eq!(row["pages"], json!(["scene-calendar", "scene-notes"]));
        panels.page_set("DP-1", Edge::Right, "scene-notes").unwrap();
        panels.set_mode("DP-1", Edge::Right, "pinned").unwrap();
        // Motion: settle past the travel time.
        std::thread::sleep(MOTION + Duration::from_millis(50));
        panels.tick();
        let drawn = panels.drawn("DP-1", Edge::Right).expect("pinned edge draws");
        assert_eq!(drawn.page, "scene-notes", "one page per edge: the active one");
        assert!(drawn.thickness >= 360.0 && drawn.fraction >= 0.999, "{drawn:?}");
        assert_eq!(panels.state("DP-1", Edge::Right)["visible"], json!(true));
    }

    fn tuning() -> CornerDetectorConfig {
        CornerDetectorConfig::new(10.0, Duration::from_millis(200), 1500.0).unwrap()
    }

    /// The loader's relevant request/drive_popups transitions: save prior
    /// page+mode, select+pin on open, restore only owned state on close.
    fn loader_toggle(panels: &mut Panels, edge: Edge, page: &str,
        recovery: &mut Option<(String, String)>, now: Duration)
    {
        if let Some((previous, mode)) = recovery.take() {
            if panels.state("DP-1", edge)["page"] != previous {
                panels.page_set("DP-1", edge, &previous).unwrap();
            }
            if panels.mode("DP-1", edge) != mode {
                panels.set_mode_at("DP-1", edge, PanelMode::parse(&mode).unwrap(), now).unwrap();
            }
        } else {
            let state = panels.state("DP-1", edge);
            *recovery = Some((state["page"].as_str().unwrap().into(), state["mode"].as_str().unwrap().into()));
            panels.page_set("DP-1", edge, page).unwrap();
            panels.set_mode_at("DP-1", edge, PanelMode::Pinned, now).unwrap();
        }
    }

    #[test]
    fn corner_and_loader_button_share_popup_ownership_in_both_orders() {
        for (edge, name, point) in [(Edge::Left, "launcher", (1.0, 1.0)), (Edge::Right, "calendar", (1279.0, 799.0))] {
            for corner_first in [true, false] {
                let mut panels = panels_with("DP-1");
                let page = format!("scene-{name}");
                register(&mut panels, "DP-1", edge, &page, 440.0);
                panels.popup_routes.insert(("DP-1".into(), page.clone()), ("scenes".into(), name.into()));
                let mut recovery = None;
                let mut now = Duration::from_secs(1);
                for corner in [corner_first, !corner_first, corner_first, !corner_first] {
                    if corner {
                        panels.pointer_at("DP-1", Some(point), tuning(), now);
                        let before = panels.mode("DP-1", edge);
                        assert!(panels.pointer_button_at(0x110, true, false, now));
                        assert!(panels.pointer_button_at(0x110, false, false, now));
                        assert_eq!(panels.mode("DP-1", edge), before, "corner must not pre-pin the loader's saved mode");
                        assert_eq!(panels.popup_commands(), vec![("scenes".into(), name.into())]);
                    }
                    loader_toggle(&mut panels, edge, &page, &mut recovery, now);
                    now += MOTION + Duration::from_millis(1);
                    panels.tick_at(now);
                    assert_eq!(panels.mode("DP-1", edge) == "pinned", recovery.is_some());
                    if recovery.is_none() {
                        assert!(panels.hidden("DP-1", edge));
                        // A pointer resting in the hotspot cannot fight restore.
                        now += tuning().dwell();
                        panels.tick_at(now);
                        assert!(panels.hidden("DP-1", edge));
                    }
                    panels.pointer_at("DP-1", Some((640.0, 400.0)), tuning(), now);
                    now += GRACE + MOTION;
                }
            }
        }
    }

    #[test]
    fn dwell_then_loader_pin_and_restore_conceals_until_hardware_departure() {
        for (edge, page, point) in [(Edge::Left, "scene-launcher", (1.0, 1.0)), (Edge::Right, "scene-calendar", (1279.0, 799.0))] {
            let mut panels = panels_with("DP-1");
            register(&mut panels, "DP-1", edge, page, 440.0);
            let mut now = Duration::from_secs(1);
            panels.pointer_at("DP-1", Some(point), tuning(), now);
            now += tuning().dwell() + MOTION;
            panels.tick_at(now);
            assert!(!panels.hidden("DP-1", edge));
            let mut recovery = None;
            loader_toggle(&mut panels, edge, page, &mut recovery, now);
            loader_toggle(&mut panels, edge, page, &mut recovery, now);
            assert!(panels.hidden("DP-1", edge));
            panels.tick_at(now + GRACE + MOTION);
            assert!(panels.hidden("DP-1", edge));
            now += GRACE + MOTION;
            panels.pointer_at("DP-1", Some((640.0, 400.0)), tuning(), now);
            panels.pointer_at("DP-1", Some(point), tuning(), now);
            panels.tick_at(now + tuning().dwell());
            assert!(!panels.hidden("DP-1", edge), "a real departure allows the next reveal");
        }
    }

    #[test]
    fn middle_of_every_edge_never_reveals_or_claims_a_click() {
        for point in [(1.0, 400.0), (640.0, 799.0), (1279.0, 400.0), (640.0, 1.0), (10.5, 1.0), (1.0, 10.5)] {
            let mut panels = panels_with("DP-1");
            for edge in EDGES {
                register(&mut panels, "DP-1", edge, shell_edge(edge).as_str(), 100.0);
            }
            let start = Duration::from_secs(1);
            panels.pointer_at("DP-1", Some(point), tuning(), start);
            assert_eq!(panels.tick_at(start), Wake::Idle, "no corner dwell at {point:?}");
            panels.tick_at(start + GRACE + MOTION);
            assert!(EDGES.into_iter().all(|edge| panels.hidden("DP-1", edge)), "{point:?}");
            assert!(!panels.pointer_button_at(0x110, true, false, start + GRACE + MOTION));
            assert!(!panels.pointer_button_at(0x110, false, false, start + GRACE + MOTION));
        }
    }

    #[test]
    fn corner_dwells_and_the_page_holds_until_grace_after_leave() {
        let mut panels = panels_with("DP-1");
        register(&mut panels, "DP-1", Edge::Left, "scene-launcher", 440.0);
        let start = Duration::from_secs(1);
        panels.pointer_at("DP-1", Some((9.0, 9.0)), tuning(), start);
        assert!(panels.hidden("DP-1", Edge::Left), "no reveal before dwell");
        panels.tick_at(start + tuning().dwell() - Duration::from_millis(1));
        assert!(panels.hidden("DP-1", Edge::Left));
        let revealed = start + tuning().dwell();
        panels.tick_at(revealed);
        assert!(!panels.hidden("DP-1", Edge::Left));
        assert_eq!(panels.mode("DP-1", Edge::Left), "hidden", "dwell does not pin");
        assert_eq!(panels.focus_requested("DP-1"), Some(Edge::Left), "hotspot opening also requests search focus");
        panels.focus_granted("DP-1");
        panels.tick_at(revealed + MOTION);
        assert_eq!(panels.focus_requested("DP-1"), None, "a stationary pointer does not repeatedly request focus");
        // Leave the hotspot but stay over the exposed page: no conceal wake.
        panels.pointer_at("DP-1", Some((200.0, 400.0)), tuning(), revealed + MOTION);
        assert_eq!(panels.tick_at(revealed + MOTION), Wake::Idle);
        let left = revealed + MOTION + Duration::from_millis(1);
        panels.pointer_at("DP-1", Some((700.0, 400.0)), tuning(), left);
        assert_eq!(panels.tick_at(left), Wake::At(panels.epoch + left + GRACE));
        panels.tick_at(left + GRACE - Duration::from_millis(1));
        assert!(!panels.hidden("DP-1", Edge::Left));
        panels.tick_at(left + GRACE);
        assert!(panels.hidden("DP-1", Edge::Left));
        panels.tick_at(left + GRACE + MOTION);
        assert!(panels.drawn("DP-1", Edge::Left).is_none());
    }

    #[test]
    fn each_corner_dwells_then_summons_only_its_core_edge() {
        for (point, corner) in [
            ((1.0, 1.0), Corner::TopLeft), ((1.0, 799.0), Corner::BottomLeft),
            ((1279.0, 799.0), Corner::BottomRight), ((1279.0, 1.0), Corner::TopRight),
        ] {
            let mut panels = panels_with("DP-1");
            for edge in ShellEdge::ALL {
                panels.models.get_mut("DP-1").unwrap().restore_mode(edge, Duration::from_secs(1), PanelMode::Hidden).unwrap();
                register(&mut panels, "DP-1", seat_edge(edge), edge.as_str(), 100.0);
            }
            let start = Duration::from_secs(2);
            // First sample has no speed measurement. The core requires one
            // stationary follow-up, then its exact dwell deadline.
            panels.pointer_at("DP-1", Some(point), tuning(), start);
            assert!(EDGES.into_iter().all(|edge| panels.hidden("DP-1", edge)));
            let deadline = panels.pointer.as_ref().unwrap().detector.next_deadline().unwrap();
            assert_eq!(panels.tick_at(start), Wake::At(panels.epoch + deadline));
            panels.tick_at(deadline);
            for edge in ShellEdge::ALL {
                assert_eq!(!panels.hidden("DP-1", seat_edge(edge)), edge == corner.summoned_edge());
            }
            let left = deadline + MOTION;
            panels.pointer_at("DP-1", None, tuning(), left);
            panels.tick_at(left + GRACE);
            assert!(panels.hidden("DP-1", seat_edge(corner.summoned_edge())));
        }
    }

    #[test]
    fn crossing_outputs_releases_the_old_pointer_and_pinned_edges_stay_up() {
        let mut panels = panels_with("DP-1");
        register(&mut panels, "DP-1", Edge::Left, "scene-launcher", 440.0);
        register(&mut panels, "DP-1", Edge::Right, "scene-calendar", 360.0);
        // Edges start hidden (Quoin's fresh state); pin this one explicitly.
        panels.set_mode("DP-1", Edge::Right, "pinned").unwrap();
        panels.ensure("HDMI-A-1", (1920.0, 1080.0));
        let start = Duration::from_secs(1);
        panels.pointer_at("DP-1", Some((1.0, 1.0)), tuning(), start);
        panels.tick_at(start + tuning().dwell());
        let crossed = start + tuning().dwell() + MOTION;
        panels.pointer_at("HDMI-A-1", Some((500.0, 400.0)), tuning(), crossed);
        assert_eq!(panels.tick_at(crossed), Wake::At(panels.epoch + crossed + GRACE));
        panels.tick_at(crossed + GRACE + MOTION);
        assert!(panels.hidden("DP-1", Edge::Left));
        assert_eq!(panels.mode("DP-1", Edge::Right), "pinned");
        assert!(panels.drawn("DP-1", Edge::Right).is_some());
    }

    #[test]
    fn reentry_cancels_conceal_and_explicit_hide_latches_until_pointer_leave() {
        let mut panels = panels_with("DP-1");
        register(&mut panels, "DP-1", Edge::Left, "scene-launcher", 440.0);
        let start = Duration::from_secs(1);
        panels.pointer_at("DP-1", Some((1.0, 1.0)), tuning(), start);
        panels.tick_at(start + tuning().dwell());
        let settled = start + tuning().dwell() + MOTION;
        panels.pointer_at("DP-1", Some((700.0, 400.0)), tuning(), settled);
        let returned = settled + GRACE / 2;
        panels.pointer_at("DP-1", Some((100.0, 400.0)), tuning(), returned);
        assert_eq!(panels.tick_at(returned), Wake::Idle);
        panels.models.get_mut("DP-1").unwrap().panel_input(ShellEdge::Left, returned, PanelInput::Hide).unwrap();
        panels.pointer_at("DP-1", Some((1.0, 1.0)), tuning(), returned + MOTION);
        panels.tick_at(returned + MOTION + tuning().dwell());
        assert!(panels.hidden("DP-1", Edge::Left), "remaining inside cannot undo a deliberate hide");
        let left = returned + MOTION + tuning().dwell();
        panels.pointer_at("DP-1", None, tuning(), left);
        panels.pointer_at("DP-1", Some((1.0, 1.0)), tuning(), left + Duration::from_millis(1));
        panels.tick_at(left + Duration::from_millis(1) + tuning().dwell());
        assert!(!panels.hidden("DP-1", Edge::Left), "a real leave and reentry releases the latch");
    }

    #[test]
    fn hotspot_click_is_consumed_before_dwell_and_fires_on_release() {
        let mut panels = panels_with("DP-1");
        register(&mut panels, "DP-1", Edge::Left, "scene-launcher", 440.0);
        let start = Duration::from_secs(1);
        panels.pointer_at("DP-1", Some((9.0, 9.0)), tuning(), start);
        assert!(panels.pointer_button_at(0x110, true, false, start), "press consumed without dwell");
        assert_eq!(panels.mode("DP-1", Edge::Left), "hidden", "press does not fire");
        let released = start + Duration::from_millis(10);
        panels.pointer_at("DP-1", Some((8.0, 8.0)), tuning(), released);
        assert!(panels.pointer_button_at(0x110, false, false, released), "release consumed");
        assert_eq!(panels.mode("DP-1", Edge::Left), "pinned");
        assert!(panels.zones("DP-1").is_empty(), "LMB never docks");
        assert!(!panels.pointer_button_at(0x110, false, false, released), "ownership ended");
    }

    #[test]
    fn returning_to_the_hotspot_during_conceal_delay_keeps_the_reveal() {
        let mut panels = panels_with("DP-1");
        register(&mut panels, "DP-1", Edge::Left, "scene-launcher", 440.0);
        let start = Duration::from_secs(1);
        panels.pointer_at("DP-1", Some((9.0, 9.0)), tuning(), start);
        let revealed = start + tuning().dwell();
        panels.tick_at(revealed);
        let left = revealed + MOTION;
        panels.pointer_at("DP-1", Some((700.0, 400.0)), tuning(), left);
        let returned = left + GRACE - Duration::from_millis(1);
        panels.pointer_at("DP-1", Some((9.0, 9.0)), tuning(), returned);
        panels.tick_at(left + GRACE);
        assert!(!panels.hidden("DP-1", Edge::Left), "hotspot re-entry cancels conceal before a new dwell");
    }

    #[test]
    fn hotspot_clicks_obey_the_mode_table_and_capture_press_modifiers() {
        for (before, shift, after) in [
            (PanelMode::Hidden, false, "pinned"), (PanelMode::Pinned, false, "hidden"),
            (PanelMode::Docked, false, "pinned"), (PanelMode::Hidden, true, "docked"),
            (PanelMode::Pinned, true, "docked"), (PanelMode::Docked, true, "hidden"),
        ] {
            let mut panels = panels_with("DP-1");
            register(&mut panels, "DP-1", Edge::Left, "scene-launcher", 100.0);
            let start = Duration::from_secs(1);
            panels.models.get_mut("DP-1").unwrap().restore_mode(ShellEdge::Left, start, before).unwrap();
            panels.pointer_at("DP-1", Some((5.0, 5.0)), tuning(), start);
            assert!(panels.pointer_button_at(0x110, true, shift, start));
            assert!(panels.pointer_button_at(0x110, false, !shift, start));
            assert_eq!(panels.mode("DP-1", Edge::Left), after, "{before:?}, shift={shift}");
            if after == "hidden" {
                assert!(panels.hidden("DP-1", Edge::Left), "no transient reveal after explicit hide");
            }
        }
    }

    #[test]
    fn cancelled_drag_keeps_ownership_and_right_click_does_not_change_mode() {
        let mut panels = panels_with("DP-1");
        register(&mut panels, "DP-1", Edge::Left, "scene-launcher", 440.0);
        let start = Duration::from_secs(1);
        panels.pointer_at("DP-1", Some((1.0, 1.0)), tuning(), start);
        assert!(panels.pointer_button_at(0x110, true, false, start));
        // Return to the press point after exceeding tolerance: still cancelled.
        panels.pointer_at("DP-1", Some((50.0, 50.0)), tuning(), start);
        panels.pointer_at("DP-1", Some((1.0, 1.0)), tuning(), start);
        assert!(panels.pointer_button_at(0x110, false, false, start));
        assert_eq!(panels.mode("DP-1", Edge::Left), "hidden");
        assert!(panels.pointer_button_at(0x111, true, false, start));
        assert!(panels.menu().is_none(), "RMB fires on release only");
        assert!(panels.pointer_button_at(0x111, false, false, start));
        assert_eq!(panels.mode("DP-1", Edge::Left), "hidden");
        assert!(panels.menu().is_some());
        assert!(panels.pointer_button_at(0x110, true, false, start));
        panels.pointer_at("DP-1", None, tuning(), start);
        assert!(panels.pointer_button_at(0x110, false, false, start), "release after departure stays consumed");
        assert_eq!(panels.mode("DP-1", Edge::Left), "hidden");
    }

    #[test]
    fn menu_holds_a_reveal_past_grace_then_applies_each_mode_before_release() {
        use crate::menu::Input;
        for (index, mode) in [(0, PanelMode::Pinned), (1, PanelMode::Docked), (2, PanelMode::Hidden)] {
            let mut panels = panels_with("DP-1");
            register(&mut panels, "DP-1", Edge::Left, "scene-launcher", 100.0);
            let start = Duration::from_secs(1);
            panels.pointer_at("DP-1", Some((1.0, 1.0)), tuning(), start);
            let revealed = start + tuning().dwell();
            panels.tick_at(revealed);
            if mode == PanelMode::Hidden {
                panels.models.get_mut("DP-1").unwrap().panel_input(ShellEdge::Left, revealed, PanelInput::SetMode(PanelMode::Pinned)).unwrap();
            }
            panels.open_menu_at("DP-1", Corner::TopLeft, revealed).unwrap();
            let serial = panels.menu().unwrap().serial;
            assert_eq!(panels.snapshot("DP-1", Value::Null)["menu"]["edge"], "left");
            panels.pointer_at("DP-1", Some((700.0, 400.0)), tuning(), revealed);
            let after_grace = revealed + GRACE + MOTION;
            panels.tick_at(after_grace);
            assert!(!panels.hidden("DP-1", Edge::Left), "menu holds transient reveal");
            panels.menu_input_at(serial, Input::Choose(index), after_grace).unwrap();
            assert!(panels.menu().is_none());
            assert_eq!(panels.mode("DP-1", Edge::Left), mode.as_str());
            assert_eq!(panels.hidden("DP-1", Edge::Left), mode == PanelMode::Hidden);
            assert_eq!(panels.menu_input_at(serial, Input::Close, after_grace).unwrap_err().code, "STALE_MENU");
        }
    }

    #[test]
    fn menu_escape_and_outside_click_close_and_cancelled_rmb_never_opens() {
        use crate::menu::Input;
        let mut panels = panels_with("DP-1");
        register(&mut panels, "DP-1", Edge::Left, "scene-launcher", 100.0);
        let start = Duration::from_secs(1);
        panels.pointer_at("DP-1", Some((1.0, 1.0)), tuning(), start);
        assert!(panels.pointer_button_at(0x111, true, false, start));
        panels.pointer_at("DP-1", Some((100.0, 100.0)), tuning(), start);
        panels.pointer_at("DP-1", Some((1.0, 1.0)), tuning(), start);
        assert!(panels.pointer_button_at(0x111, false, false, start));
        assert!(panels.menu().is_none(), "movement permanently cancels RMB");
        panels.open_menu_at("DP-1", Corner::TopLeft, start).unwrap();
        let serial = panels.menu().unwrap().serial;
        panels.menu_input_at(serial, Input::Move(-1), start).unwrap();
        assert_eq!(panels.menu().unwrap().selected, 1, "checked Hide is skipped");
        panels.menu_input_at(serial, Input::Close, start).unwrap();
        assert!(panels.menu().is_none());
        panels.open_menu_at("DP-1", Corner::TopLeft, start).unwrap();
        panels.pointer_at("DP-1", Some((700.0, 400.0)), tuning(), start);
        assert!(panels.pointer_button_at(0x110, true, false, start));
        assert!(panels.menu().is_none());
        assert!(panels.pointer_button_at(0x110, false, false, start), "outside release stays consumed");
        assert_eq!(panels.mode("DP-1", Edge::Left), "hidden");
    }

    #[test]
    fn menu_hide_dismisses_a_transient_reveal_and_waits_for_a_real_leave() {
        use crate::menu::Input;
        let mut panels = Panels::default();
        panels.ensure("DP-1", (1280.0, 800.0));
        register(&mut panels, "DP-1", Edge::Left, "scene-launcher", 440.0);
        let start = Duration::from_secs(1);
        panels.pointer_at("DP-1", Some((0.0, 0.0)), tuning(), start);
        let revealed = start + tuning().dwell();
        panels.tick_at(revealed);
        panels.tick_at(revealed + MOTION);
        panels.open_menu_at("DP-1", Corner::TopLeft, revealed + MOTION).unwrap();
        assert!(panels.menu().unwrap().items[2].enabled(), "Hide dismisses a reveal even in hidden mode");
        let serial = panels.menu().unwrap().serial;
        panels.menu_input_at(serial, Input::Choose(2), revealed + MOTION).unwrap();
        assert!(panels.hidden("DP-1", Edge::Left));
        panels.tick_at(revealed + MOTION * 2);
        panels.pointer_at("DP-1", Some((2.0, 2.0)), tuning(), revealed + MOTION * 3);
        panels.tick_at(revealed + MOTION * 4);
        assert!(panels.hidden("DP-1", Edge::Left), "motion/animation in the old bounds cannot reopen");
        let left = revealed + MOTION * 5;
        panels.pointer_at("DP-1", Some((800.0, 400.0)), tuning(), left);
        panels.pointer_at("DP-1", Some((0.0, 0.0)), tuning(), left + Duration::from_millis(1));
        panels.tick_at(left + Duration::from_millis(1) + tuning().dwell());
        assert!(!panels.hidden("DP-1", Edge::Left));
    }

    #[test]
    fn keyboard_holder_survives_menu_close_then_focus_loss_dismisses() {
        let mut panels = Panels::default();
        panels.ensure("DP-1", (1280.0, 800.0));
        register(&mut panels, "DP-1", Edge::Left, "scene-launcher", 440.0);
        let start = Duration::from_secs(1);
        panels.models.get_mut("DP-1").unwrap().panel_input(ShellEdge::Left, start, PanelInput::Reveal).unwrap();
        panels.focus_at("DP-1", Some(Edge::Left), start);
        panels.open_menu_at("DP-1", Corner::TopLeft, start).unwrap();
        panels.close_menu_at(start);
        panels.tick_at(start + GRACE + MOTION);
        assert!(!panels.hidden("DP-1", Edge::Left), "focus remains after the popup holder releases");
        panels.focus_at("DP-1", None, start + GRACE + MOTION);
        assert!(panels.hidden("DP-1", Edge::Left), "focus loss dismisses immediately");
    }

    #[test]
    fn confirming_extra_keeps_the_holder_and_dispatches_only_the_confirmed_choice() {
        use crate::menu::{Choice, Extra, Input, Item};
        let mut panels = panels_with("DP-1");
        let start = Duration::from_secs(1);
        panels.open_menu_at("DP-1", Corner::TopLeft, start).unwrap();
        let extra = Extra { label: "Tools".into(), target: "tools".into(), verb: "tools.open".into(),
            args: vec!["main".into()], confirm: Some("Open tools?".into()) };
        let menu = panels.menu.as_mut().unwrap();
        let serial = menu.serial;
        menu.items.truncate(3);
        menu.items.push(Item { label: extra.label.clone(), checked: false, choice: Choice::Extra(extra) });
        assert!(panels.menu_input_at(serial, Input::Choose(3), start).unwrap().is_none());
        let menu = panels.menu().unwrap();
        assert_ne!(menu.serial, serial);
        assert_eq!(menu.items.len(), 3);
        assert_eq!(menu.selected, 1);
        let serial = menu.serial;
        let call = panels.menu_input_at(serial, Input::Choose(1), start).unwrap().unwrap();
        assert_eq!((call.target.as_str(), call.verb.as_str()), ("tools", "tools.open"));
        assert!(call.confirm.is_none());
        assert!(panels.menu().is_none());
    }

    #[test]
    fn an_animating_page_acquires_a_stationary_pointer_once_it_reaches_it() {
        let mut panels = panels_with("DP-1");
        register(&mut panels, "DP-1", Edge::Left, "scene-launcher", 440.0);
        let start = Duration::from_secs(1);
        panels.pointer_at("DP-1", Some((1.0, 1.0)), tuning(), start);
        let revealed = start + tuning().dwell();
        panels.tick_at(revealed);
        // The panel is still entirely offscreen; move off the corner into
        // its future bounds, then stop sending motion.
        panels.pointer_at("DP-1", Some((100.0, 400.0)), tuning(), revealed);
        panels.tick_at(revealed + MOTION);
        assert_eq!(panels.tick_at(revealed + GRACE), Wake::Idle);
        assert!(!panels.hidden("DP-1", Edge::Left));
        assert!(panels.models["DP-1"].panel(ShellEdge::Left).pointer_inside);
    }

    #[test]
    fn writes_refuse_an_empty_edge_an_unknown_page_and_a_bad_mode() {
        let mut panels = panels_with("DP-1");
        assert_eq!(panels.set_mode("DP-1", Edge::Left, "pinned").unwrap_err().code, "EMPTY_EDGE");
        register(&mut panels, "DP-1", Edge::Left, "scene-launcher", 440.0);
        assert_eq!(panels.page_set("DP-1", Edge::Left, "nope").unwrap_err().code, "UNKNOWN_PAGE");
        assert_eq!(panels.set_mode("DP-1", Edge::Left, "floating").unwrap_err().code, "BAD_ARGUMENT");
    }

    #[test]
    fn a_declared_order_reorders_the_carousel_and_the_row_reports_it() {
        let mut panels = panels_with("DP-1");
        register(&mut panels, "DP-1", Edge::Right, "scene-calendar", 360.0);
        register(&mut panels, "DP-1", Edge::Right, "settings.appearance", 360.0);
        let mut declared = crate::conf::Declared::default();
        declared[ShellEdge::Right.index()] = vec!["settings.appearance".into(), "scene-calendar".into()];
        panels.declare(declared.clone()).unwrap();
        let row = panels.state("DP-1", Edge::Right);
        assert_eq!(row["pages"], json!(["settings.appearance", "scene-calendar"]));
        assert_eq!(row["declared"], json!(["settings.appearance", "scene-calendar"]));
        // A model created afterwards takes the same order.
        panels.ensure("HDMI-A-1", (1920.0, 1080.0));
        register(&mut panels, "HDMI-A-1", Edge::Right, "scene-calendar", 360.0);
        register(&mut panels, "HDMI-A-1", Edge::Right, "settings.appearance", 360.0);
        assert_eq!(panels.state("HDMI-A-1", Edge::Right)["pages"], json!(["settings.appearance", "scene-calendar"]));
        // A repeated name is refused before anything changes.
        declared[ShellEdge::Left.index()] = vec!["dup".into(), "dup".into()];
        assert_eq!(panels.declare(declared).unwrap_err().code, "INVALID_ARGUMENT");
        assert_eq!(panels.state("DP-1", Edge::Left)["declared"], json!([]));
    }

    #[test]
    fn a_resize_settles_the_new_thickness_and_refuses_out_of_range_and_budget() {
        let mut panels = panels_with("DP-1");
        register(&mut panels, "DP-1", Edge::Bottom, "scene-panel", 52.0);
        let before = panels.state("DP-1", Edge::Bottom)["width_px"].as_f64().unwrap();
        panels.resize("DP-1", Edge::Bottom, before as f32 + 4.0).unwrap();
        assert_eq!(panels.state("DP-1", Edge::Bottom)["width_px"].as_f64(), Some(before + 4.0));
        let range = panels.resize("DP-1", Edge::Bottom, 10.0).unwrap_err();
        assert_eq!(range["range_px"], json!([24.0, 200.0]));
        assert_eq!(panels.resize("DP-1", Edge::Bottom, 30.0).unwrap_err(), json!({
            "error_code":"PAGE_MINIMUM",
            "message":"panel Bottom thickness 30 is below the shown page's extent 52; resize to at least 52 or show another page",
            "minimum_px":52.0, "edge":"bottom", "requested":30.0,
        }));
        assert_eq!(panels.resize("HDMI-A-1", Edge::Bottom, 60.0).unwrap_err()["error_code"], "EMPTY_EDGE");
    }

    #[test]
    fn other_resize_configuration_errors_have_quoins_rejection_shape() {
        assert_eq!(resize_config_refusal(Edge::Bottom, 30.0, 200.0, PanelConfigError::InvalidThickness(30.0)), json!({
            "error_code":"PANEL_RESIZE_REJECTED", "error":"panel thickness must be finite and positive, got 30",
            "edge":"bottom", "requested":30.0, "max":200.0,
        }));
    }

    #[test]
    fn a_refused_resize_leaves_a_transient_reveal_and_its_wake_untouched() {
        let mut panels = panels_with("DP-1");
        register(&mut panels, "DP-1", Edge::Bottom, "scene-panel", 52.0);
        panels.input("DP-1", Edge::Bottom, PanelVerb::Show).unwrap();
        let before = format!("{:?}", panels.models["DP-1"]);
        assert_eq!(panels.resize("DP-1", Edge::Bottom, 30.0).unwrap_err()["error_code"], "PAGE_MINIMUM");
        assert_eq!(format!("{:?}", panels.models["DP-1"]), before, "no deadline or motion changes on refusal");
    }

    #[test]
    fn redeclaring_publishes_every_output_once() {
        let mut panels = panels_with("DP-1");
        panels.ensure("HDMI-A-1", (1920.0, 1080.0));
        assert_eq!(panels.notices("DP-1", Value::Null, Some(1)).len(), 2);
        let mut declared = crate::conf::Declared::default();
        declared[ShellEdge::Right.index()] = vec!["scene-notes".into()];
        panels.declare(declared).unwrap();
        let notices = panels.notices("DP-1", Value::Null, Some(1));
        assert_eq!(notices.len(), 2);
        let outputs: Vec<String> = notices.iter().map(|wire| {
            let body: Value = serde_json::from_str(wire.split("---\n").nth(2).unwrap()).unwrap();
            assert_eq!(body["panels"]["right"]["declared"], json!(["scene-notes"]));
            body["panels"]["right"]["output"].as_str().unwrap().to_owned()
        }).collect();
        assert_eq!(outputs, ["DP-1", "HDMI-A-1"]);
        assert!(panels.notices("DP-1", Value::Null, Some(1)).is_empty());
        assert!(panels.notices("HDMI-A-1", Value::Null, Some(1)).is_empty());
    }

    #[test]
    fn corner_verbs_summon_their_edge_and_hide_conceals_only_a_reveal() {
        assert_eq!(
            ["top-left", "bottom-left", "bottom-right", "top-right"].map(corner_edge),
            [Some(Edge::Left), Some(Edge::Bottom), Some(Edge::Right), Some(Edge::Top)]
        );
        assert_eq!(corner_edge("middle"), None);
        let mut panels = panels_with("DP-1");
        assert_eq!(panels.input("DP-1", Edge::Left, PanelVerb::Show).unwrap_err().code, "EMPTY_EDGE");
        register(&mut panels, "DP-1", Edge::Left, "scene-launcher", 440.0);
        panels.input("DP-1", Edge::Left, PanelVerb::Show).unwrap();
        assert!(!panels.hidden("DP-1", Edge::Left), "a transient reveal holds it");
        panels.input("DP-1", Edge::Left, PanelVerb::Hide).unwrap();
        assert!(panels.hidden("DP-1", Edge::Left));
        panels.input("DP-1", Edge::Left, PanelVerb::Pin).unwrap();
        assert_eq!(panels.mode("DP-1", Edge::Left), "pinned");
        panels.input("DP-1", Edge::Left, PanelVerb::Hide).unwrap();
        assert!(!panels.hidden("DP-1", Edge::Left), "hide never changes a persistent mode");
    }

    #[test]
    fn semantic_toggle_dismisses_pointer_or_focus_reveals_and_latches_the_hotspot() {
        for focused in [false, true] {
            let mut panels = panels_with("DP-1");
            register(&mut panels, "DP-1", Edge::Left, "scene-launcher", 440.0);
            let start = Duration::from_secs(1);
            panels.pointer_at("DP-1", Some((1.0, 1.0)), tuning(), start);
            let revealed = start + tuning().dwell();
            panels.tick_at(revealed);
            if focused { panels.focus_at("DP-1", Some(Edge::Left), revealed); }
            panels.input_at("DP-1", Edge::Left, PanelVerb::Toggle, revealed).unwrap();
            assert!(panels.hidden("DP-1", Edge::Left), "toggle must release the keyboard holder first");
            panels.pointer_at("DP-1", Some((2.0, 2.0)), tuning(), revealed + MOTION);
            panels.tick_at(revealed + MOTION + tuning().dwell());
            assert!(panels.hidden("DP-1", Edge::Left), "hotspot cannot fight deliberate dismissal");
            let reopen = revealed + MOTION + tuning().dwell();
            panels.input_at("DP-1", Edge::Left, PanelVerb::Toggle, reopen).unwrap();
            assert!(!panels.hidden("DP-1", Edge::Left));
            assert_eq!(panels.mode("DP-1", Edge::Left), "hidden", "panel.toggle is transient");
            assert_eq!(panels.focus_requested("DP-1"), Some(Edge::Left));
            panels.focus_at("DP-1", None, reopen + MOTION);
            assert!(panels.hidden("DP-1", Edge::Left), "outside focus dismisses the transient reveal");
        }
    }

    #[test]
    fn persistent_modes_request_focus_without_a_transient_holder_and_escape_returns_focus() {
        for mode in ["pinned", "docked"] {
            let mut panels = panels_with("DP-1");
            register(&mut panels, "DP-1", Edge::Right, "scene-calendar", 360.0);
            panels.set_mode("DP-1", Edge::Right, mode).unwrap();
            assert_eq!(panels.mode("DP-1", Edge::Right), mode);
            assert_eq!(panels.focus_requested("DP-1"), Some(Edge::Right));
            panels.focus("DP-1", Some(Edge::Right));
            panels.focus_granted("DP-1");
            assert!(!panels.models["DP-1"].panel(ShellEdge::Right).transient_revealed);
            panels.escape("DP-1", Edge::Right);
            assert_eq!(panels.mode("DP-1", Edge::Right), mode);
            assert_eq!(panels.focused("DP-1"), None);
        }
    }

    #[test]
    fn loader_select_pin_restore_wins_over_hotspot_and_focus_on_both_popup_edges() {
        for (edge, corner, point, page) in [
            (Edge::Left, Corner::TopLeft, (1.0, 1.0), "scene-launcher"),
            (Edge::Right, Corner::BottomRight, (1279.0, 799.0), "scene-calendar"),
        ] {
            for previous in [PanelMode::Hidden, PanelMode::Pinned, PanelMode::Docked] {
                let mut panels = panels_with("DP-1");
                register(&mut panels, "DP-1", edge, "previous-page", 360.0);
                register(&mut panels, "DP-1", edge, page, 440.0);
                let start = Duration::from_secs(1);
                panels.set_mode_at("DP-1", edge, previous, start).unwrap();
                panels.pointer_at("DP-1", Some(point), tuning(), start);
                panels.tick_at(start + tuning().dwell());
                let revealed = start + tuning().dwell() + MOTION;
                panels.tick_at(revealed);
                panels.focus_at("DP-1", Some(edge), revealed);
                panels.open_menu_at("DP-1", corner, revealed).unwrap();
                let saved = panels.snapshot("DP-1", Value::Null)["panels"][edge.as_str()].clone();

                // The unchanged loader reads applied state between select and pin.
                panels.page_set("DP-1", edge, page).unwrap();
                assert_eq!(panels.state("DP-1", edge)["mode"], saved["mode"]);
                panels.set_mode_at("DP-1", edge, PanelMode::Pinned, revealed).unwrap();
                let row = panels.state("DP-1", edge);
                assert_eq!(row["page"], page);
                assert_eq!(row["mode"], "pinned");
                assert_eq!(row["visible"], true);
                let panel = panels.models["DP-1"].panel(shell_edge(edge));
                assert!(!panel.transient_revealed && !panel.pointer_inside && !panel.corner_inside);
                assert!(panels.menu().is_none());
                assert_eq!(panels.focused("DP-1"), None, "old focus holder was cancelled");
                assert_eq!(panels.focus_requested("DP-1"), Some(edge), "pinned autofocus still owed");
                let settled = revealed + MOTION;
                panels.tick_at(settled);
                panels.focus_at("DP-1", Some(edge), settled);
                panels.focus_granted("DP-1");
                // Late departures/focus loss cannot conceal the loader's pin.
                panels.pointer_at("DP-1", None, tuning(), settled);
                panels.focus_at("DP-1", None, settled);
                panels.tick_at(settled + GRACE + MOTION);
                assert_eq!(panels.state("DP-1", edge)["visible"], true);
                assert_eq!(panels.mode("DP-1", edge), "pinned");

                let restore = settled + GRACE + MOTION;
                panels.pointer_at("DP-1", Some(point), tuning(), restore);
                panels.page_set("DP-1", edge, saved["page"].as_str().unwrap()).unwrap();
                assert_eq!(panels.state("DP-1", edge)["mode"], "pinned");
                panels.set_mode_at("DP-1", edge, previous, restore).unwrap();
                panels.tick_at(restore + tuning().dwell() + MOTION + GRACE);
                assert_eq!(panels.state("DP-1", edge)["page"], saved["page"]);
                assert_eq!(panels.state("DP-1", edge)["mode"], saved["mode"]);
                assert_eq!(panels.hidden("DP-1", edge), previous == PanelMode::Hidden,
                    "a pending dwell must not race the loader's restore");
                assert!(!panels.models["DP-1"].panel(shell_edge(edge)).transient_revealed);
            }
        }
    }

    #[test]
    fn repeated_hidden_mode_cancels_holders_and_latches_old_bounds_until_hardware_leave() {
        let mut panels = panels_with("DP-1");
        register(&mut panels, "DP-1", Edge::Left, "scene-launcher", 440.0);
        let start = Duration::from_secs(1);
        panels.pointer_at("DP-1", Some((1.0, 1.0)), tuning(), start);
        let settled = start + tuning().dwell() + MOTION;
        panels.tick_at(start + tuning().dwell());
        panels.tick_at(settled);
        panels.pointer_at("DP-1", Some((100.0, 400.0)), tuning(), settled);
        panels.focus_at("DP-1", Some(Edge::Left), settled);
        panels.open_menu_at("DP-1", Corner::TopLeft, settled).unwrap();
        panels.set_mode_at("DP-1", Edge::Left, PanelMode::Hidden, settled).unwrap();
        assert!(panels.hidden("DP-1", Edge::Left));
        assert_eq!(panels.focus_requested("DP-1"), None);
        assert_eq!(panels.focused("DP-1"), None);
        assert!(panels.menu().is_none());
        panels.tick_at(settled + MOTION);
        panels.set_mode_at("DP-1", Edge::Left, PanelMode::Hidden, settled + MOTION).unwrap();
        panels.pointer_at("DP-1", Some((2.0, 2.0)), tuning(), settled + MOTION);
        panels.tick_at(settled + MOTION + tuning().dwell());
        assert!(panels.hidden("DP-1", Edge::Left), "the old pointer cannot resurrect the reveal");
        let left = settled + MOTION + tuning().dwell();
        panels.pointer_at("DP-1", Some((700.0, 400.0)), tuning(), left);
        panels.pointer_at("DP-1", Some((1.0, 1.0)), tuning(), left + Duration::from_millis(1));
        panels.tick_at(left + Duration::from_millis(1) + tuning().dwell());
        assert!(!panels.hidden("DP-1", Edge::Left), "a fresh hotspot dwell can reveal again");
        assert_eq!(panels.mode("DP-1", Edge::Left), "hidden", "hotspots never change persistent mode");
    }

    #[test]
    fn selecting_a_resident_pinned_page_requests_focus_without_changing_mode_or_other_edges() {
        let mut panels = panels_with("DP-1");
        register(&mut panels, "DP-1", Edge::Left, "scene-launcher", 440.0);
        register(&mut panels, "DP-1", Edge::Right, "scene-calendar", 360.0);
        register(&mut panels, "DP-1", Edge::Right, "scene-notes", 380.0);
        let start = Duration::from_secs(1);
        panels.set_mode_at("DP-1", Edge::Right, PanelMode::Pinned, start).unwrap();
        panels.focus_granted("DP-1");
        panels.page_set("DP-1", Edge::Right, "scene-notes").unwrap();
        assert_eq!(panels.mode("DP-1", Edge::Right), "pinned");
        assert_eq!(panels.focus_requested("DP-1"), Some(Edge::Right));
        panels.set_mode_at("DP-1", Edge::Left, PanelMode::Hidden, start).unwrap();
        assert_eq!(panels.focus_requested("DP-1"), Some(Edge::Right), "mode cancellation is edge-local");
        panels.focus_granted("DP-1");
        panels.page_set("DP-1", Edge::Right, "scene-notes").unwrap();
        assert_eq!(panels.focus_requested("DP-1"), None, "an unchanged page does not repeatedly steal focus");
    }

    #[test]
    fn a_change_publishes_once_with_a_new_revision() {
        let mut panels = panels_with("DP-1");
        let first = panels.notice("DP-1", Value::Null, Some(1)).expect("the first snapshot publishes");
        assert!(first.starts_with("---\ncommand: shell.panel.changed\n---\n"));
        assert_eq!(panels.notice("DP-1", Value::Null, Some(1)), None, "unchanged: nothing");
        register(&mut panels, "DP-1", Edge::Bottom, "scene-panel", 52.0);
        let next = panels.notice("DP-1", Value::Null, Some(1)).expect("a new page publishes");
        let body: Value = serde_json::from_str(next.split("---\n").nth(2).unwrap()).unwrap();
        assert_eq!((body["revision"].as_u64(), body["generation"].as_u64()), (Some(2), Some(1)));
        assert_eq!(body["panels"]["bottom"]["pages"], json!(["scene-panel"]));
    }
}

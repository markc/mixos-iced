// SPDX-License-Identifier: MIT OR Apache-2.0
//! The iced application, in the shape of ced's `app.rs`: state = the
//! [`DopusCore`] plus chrome; `view` composes the Places sidebar · the twin
//! [`panes::pane_column`](crate::view::panes) columns (pane header ·
//! [`location`](crate::view::location) bar · sort headers ·
//! [`rows::FileList`](crate::view::rows::FileList)) with the draggable
//! divider between them, and the status bar. A normal xdg toplevel,
//! `application_id = "dev.mixos.dopus"`, SingleThread executor, tiny-skia.
//!
//! Event flow (the app contract, `mixos-dopus-core`'s seven laws):
//! - worker replies arrive on the core's `mpsc::Receiver`; a pumper thread
//!   forwards each into the futures channel the subscription drains, and the
//!   UI thread feeds every one through `core.on_event` exactly once — law 2.
//! - maintenance runs after state events and at pending config, metadata or
//!   chord deadlines. A settled window has no heartbeat or redraw feedback.
//! - derived `ConfirmRequested`/`PromptRequested` join the modal queue
//!   ([`dialogs::ModalQueue`]; the core queues concurrent modals and the
//!   oldest renders first) and are answered through the dialog surface —
//!   law 3. While one is up the key router's modal scope suppresses every
//!   chord and hands Enter/Escape to the dialog.
//! - derived `OpenFile` spawns `xdg-open` detached (fire-and-forget, like
//!   filemgr's browser.rs:3270); a spawn failure becomes a status line —
//!   law 4.
//! - derived `Status` lands in the status bar (operation progress, errors);
//!   `InfoChanged` hands the line back to the core's info text.
//! - sort-column switches pass `ascending: true` — law 5 (the core toggles a
//!   same-column sort itself).
//! - the divider drives `set_split_ratio` — law 7 (persistence derives from
//!   core state only).
//!
//! Focus: the key router resolves against a real [`FocusContext`] — while a
//! location bar is being edited the router's `focus_editable` is on, so the
//! chord resolver routes keys into the editor (every default binding is
//! `allow_in_editable: false`), and while a dialog is up the router's modal
//! scope suppresses every chord and routes Enter/Escape into the dialog.
//! P1 resolved `global()` everywhere; P2 threads the focus.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use application::Element;
use application::cpu::Renderer;
use application::iced::futures::channel::mpsc::UnboundedReceiver;
use application::iced::{Size, Subscription, Task};
#[cfg(test)]
use application::presentation::native::Event as SettingsEvent;
use application::presentation::native::Ui as SettingsUi;

use actions::{ActionId, Keymap};
use design::{Mode, Scheme};
use dopus_core::{
    ConfigFile, ConfirmAnswer, CoreEvent, DOpusConfig, DopusCore, PaneId, SortColumn, VisibleRow,
};
use settings::Diagnostic;

use crate::bus::{self, BusHandle, Delivery};
use crate::dirs::AppDirs;
use crate::icons::{self, Icons};
use crate::keys::{self, ModalKey};
use crate::theme::{self, Theme};
use crate::verbs::{self, ActionRow, Served, ServerMeta};
use crate::view::{self, Look, dialogs, rows};

/// The Wayland application id.
pub const APP_ID: &str = "dev.mixos.dopus";

/// Validated output scale, captured with each settings preparation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PreparationContext {
    scale: f32,
}
impl Default for PreparationContext {
    fn default() -> Self {
        Self { scale: 1.0 }
    }
}
impl PreparationContext {
    pub fn new(scale: f32) -> Result<Self, Diagnostic> {
        if !scale.is_finite() || !(0.125..=16.0).contains(&scale) {
            return Err(Diagnostic::new(
                "unsupported_content",
                "output.scale",
                "invalid output scale",
            ));
        }
        Ok(Self { scale })
    }
    pub fn scale(self) -> f32 {
        self.scale
    }
}

/// Everything the app reacts to.
#[derive(Debug, Clone)]
pub enum Msg {
    /// From the bus thread.
    Bus(Delivery),
    /// Resolved chords and menu entries (nav icons, theme actions).
    Actions(Vec<ActionId>),
    /// A pane's listing widget: the pane id rides the message (the
    /// [`FileList`](rows::FileList) itself is pane-agnostic).
    PaneRows(PaneId, rows::RowsMsg),
    /// A pane-local control: activate `pane`, then act on it (the core's
    /// pane verbs act on the active pane; this is the activate-then-act
    /// contract the pane headers and sort headers publish).
    Pane(PaneId, PaneOp),
    /// Navigate `pane` to a path (Places clicks, the location bar's submit).
    Go(PaneId, PathBuf),
    /// Begin editing `pane`'s location bar (the bar was clicked).
    LocationEdit(PaneId),
    /// The location editor's text changed (the REAL path text).
    LocationInput(String),
    /// Enter in `pane`'s editor: navigate there.
    LocationSubmit(PaneId),
    /// Escape in the editor: cancel.
    LocationCancel,
    /// The divider moved (ratio clamped 0.1–0.9) or double-clicked (0.5).
    Split(f32),
    SidebarWidth(dopus_core::config::Sidebar, f32),
    /// A raw core event, back from the pumper (law 2's feed).
    Core(CoreEvent),
    /// Window edges (focus reloads the keymap; close quits).
    Window(
        application::iced::window::Id,
        application::iced::window::Event,
    ),
    /// A pending maintenance deadline expired.
    Tick(Instant),
    /// The open dialog's buttons (Yes/No, OK/Cancel, field input).
    Dialog(DialogMsg),
    /// Enter/Escape while a dialog is up, from the key router's modal
    /// capture.
    DialogKey(ModalKey),
    /// An explicit decision on pinned drag/drop paths.
    DropTransfer(PaneId, PathBuf, PathBuf, dopus_core::DropAction),
    Noop,
}

/// A dialog control ([`dialogs::Dialog`]'s buttons and field).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogMsg {
    /// A confirm answered: `true` = Yes (the confirming action), `false` =
    /// No. Both consume the reservation.
    Answer(bool),
    /// A prompt submitted (OK / Enter): the CORE re-validates — an invalid
    /// name is not a resolution, so this only fires when the live
    /// validator is satisfied.
    Submit,
    /// A prompt dismissed (Cancel / Escape / scrim): `prompt_text(token,
    /// None)` — fail-closed, nothing runs.
    Dismiss,
    /// The prompt field's text changed (live `validate_filename` feedback).
    Input(String),
}

/// A pane-local control, applied after activating its pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneOp {
    Sort(SortColumn),
}

/// Everything the settings worker prepares BEFORE the UI activates a stage:
/// the compiled theme plus the complete immutable icon set (every required
/// handle rasterised synchronously — no detached threads, no partial fills,
/// nothing the view must wait for). The view only borrows this; it performs
/// no I/O or raster work after construction.
pub struct Content {
    pub theme: Theme,
    pub icons: Icons,
}

impl Content {
    /// The settings worker builder: checked prepared appearance plus every
    /// required icon handle. Any missing input is a fault — the activation
    /// fails and the consumer retains its last good content.
    pub fn build(
        look: &appearance::settings::Prepared,
        snapshot: &settings::Snapshot,
    ) -> Result<Self, Diagnostic> {
        Self::build_contextual(look, snapshot, &PreparationContext::default())
    }
    pub fn build_contextual(
        look: &appearance::settings::Prepared,
        snapshot: &settings::Snapshot,
        _context: &PreparationContext,
    ) -> Result<Self, Diagnostic> {
        let theme = theme::from_settings(look, snapshot)?;
        let tints = [
            theme.tokens.palette.text,
            theme.tokens.palette.muted_text,
            theme.tokens.palette.selection_text,
        ];
        let icons = Icons::from_prepared(look, &tints)?;
        Ok(Self { theme, icons })
    }

    /// The generic interim presentation: no font discovery or raster I/O
    /// here — the first fenced activation replaces it.
    pub fn bootstrap(look: &appearance::settings::Prepared) -> Result<Self, Diagnostic> {
        Ok(Self {
            theme: theme::from_prepared(look)?,
            icons: Icons::lucide(),
        })
    }
}

/// Borrow the app-owned view caches for the synchronous pre-ACK activation.
/// The model, drafts, selection and operations remain with the application.
fn view_activation<'a>(
    core: &'a DopusCore,
    rows: &'a [Vec<VisibleRow>; 2],
    columns: &'a mut [rows::ColumnCache; 2],
    measurements: &'a std::cell::RefCell<view::Measurements>,
    drag: &'a view::drag::Shared,
    tint: &'a mut String,
    before: Look,
) -> impl FnMut(&application::presentation::Presentation<Content>) + 'a {
    move |presentation| {
        let content = presentation.content();
        let after = Look::from_theme(&content.theme);
        *tint = icons::tint_key(after.tokens.palette.text);
        *measurements.borrow_mut() = Default::default();
        if after != before {
            view::drag::lock(drag).cancel();
        }
        for (index, pane) in [PaneId::Left, PaneId::Right].into_iter().enumerate() {
            columns[index].refresh(after, core.pane(pane), &rows[index]);
        }
    }
}

pub struct Dopus {
    core: DopusCore,
    maintenance: std::sync::mpsc::Sender<Option<Instant>>,
    maintenance_deadline: Option<Instant>,
    /// The per-pane listing snapshot `view` draws; refreshed after every
    /// update so no core mutation can be drawn stale.
    rows: [Vec<VisibleRow>; 2],
    column_cache: [rows::ColumnCache; 2],
    measurements: std::cell::RefCell<view::Measurements>,
    /// The core's live split ratio, cached for the view (the core owns it;
    /// the divider writes through `set_split_ratio` — law 7).
    split_ratio: f32,
    /// The pane whose location bar is being edited, and the REAL path text
    /// (never display-sanitised — the sanitisation law covers display only).
    editing: Option<(PaneId, String)>,
    router: keys::SharedRouter,
    /// The settings endpoint: the shared consumer + worker lane bridge. The
    /// app never clones a handle or stores a copy of the prepared content —
    /// it borrows it through [`Dopus::content`].
    settings: SettingsUi<Content, PreparationContext>,
    window: Option<application::iced::window::Id>,
    /// The generic interim presentation until the first fenced activation.
    bootstrap: Content,
    /// The icon tint of the live content, cached for the view.
    tint: String,
    /// A transient message the next core status replaces.
    status: Option<String>,
    /// The dialog on screen (the OLDEST outstanding reservation), if any.
    dialog: Option<dialogs::Dialog>,
    /// Reservations that arrived while a dialog was up (the core queues
    /// them; this mirrors that order — oldest first).
    modal_queue: dialogs::ModalQueue<dialogs::Dialog>,
    bus: Option<BusHandle>,
    action_table: Vec<ActionRow>,
    dirs: Option<AppDirs>,
    service: String,
    quitting: bool,
    drag: view::drag::Shared,
    /// The window owns its Bus name (registration succeeded).
    registered: bool,
    /// Registration ended fatally (the reason rides the last delivery).
    registration_refused: bool,
    /// The initial registration race was lost to a duplicate: quit must
    /// never persist config over the registered owner.
    lost_race: bool,
    /// Launch `dopus.open` PATHs, applied only once the window owns its name
    /// (or keeps the window after a failed handoff).
    bootstrap_paths: Vec<String>,
    /// The human already used the window before the registration outcome.
    bootstrap_touched: bool,
    /// A single-instance forward is in flight.
    handoff_pending: bool,
    launched: Instant,
    /// Fresh operation ids for fenced appearance mutations.
    next_theme_op: u64,
}

/// Run the windowed app registered on the Bus as `service`. `paths` are the
/// forwarded `dopus.open` PATHs: the first navigates the left pane, the
/// second the right, extras are logged and ignored (verbs::apply_open_paths).
pub fn run(
    config: DOpusConfig,
    config_file: Option<ConfigFile>,
    dirs: Option<AppDirs>,
    service: &str,
    noded_url: &str,
    paths: &[String],
) -> anyhow::Result<()> {
    let launched = Instant::now();
    #[cfg(feature = "acceptance")]
    let (fixture, fixture_task) = crate::acceptance::setup().map_err(anyhow::Error::msg)?;
    #[cfg(feature = "acceptance")]
    let (mut bus, deliveries) = bus::spawn_settings_fixture(service, noded_url, fixture)
        .map_err(|e| anyhow::anyhow!("Dopus bootstrap: {e}"))?;
    #[cfg(not(feature = "acceptance"))]
    let (mut bus, deliveries) = bus::spawn_settings(service, noded_url)
        .map_err(|e| anyhow::anyhow!("Dopus bootstrap: {e}"))?;

    let keymap_path = dirs.as_ref().map(|d| d.keymap_file());
    let router = keys::initial(keymap_path.as_deref()).map_err(|e| anyhow::anyhow!("{e}"))?;
    let action_table = {
        let router = router
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        action_table(&router.keymap)
    };

    // The generic bootstrap presentation precedes any installed-font I/O:
    // the window exists before the first checked activation replaces it.
    let bootstrap_prepared = bus.take_bootstrap().expect("windowed settings bootstrap");
    let mut settings = bus.take_settings_ui().expect("windowed settings endpoint");
    settings.reconcile(bus.settings_generation());
    let content = Content::bootstrap(&bootstrap_prepared)
        .map_err(|e| anyhow::anyhow!("settings bootstrap: {}: {}", e.code, e.message))?;
    let ui_font = content.theme.ui_font;
    let tint = icons::tint_key(content.theme.tokens.palette.text);

    let (core, core_events) = DopusCore::new(config, config_file);
    // Startup `dopus.open` PATHs land in the panes only once the window owns
    // its name (or keeps the window after a failed handoff): an initial
    // refused duplicate must never navigate — and thereby settle config —
    // over the registered owner.
    let split_ratio = core.config_snapshot().split_ratio;
    let (maintenance, deadlines) = maintenance();
    let mut app = Dopus {
        core,
        maintenance,
        maintenance_deadline: None,
        rows: [Vec::new(), Vec::new()],
        column_cache: Default::default(),
        measurements: Default::default(),
        split_ratio,
        editing: None,
        router,
        settings,
        window: None,
        bootstrap: content,
        tint,
        status: None,
        dialog: None,
        modal_queue: dialogs::ModalQueue::default(),
        bus: Some(bus),
        action_table,
        dirs,
        service: service.to_owned(),
        quitting: false,
        drag: Default::default(),
        registered: false,
        registration_refused: false,
        lost_race: false,
        bootstrap_paths: paths.to_vec(),
        bootstrap_touched: false,
        handoff_pending: false,
        launched,
        next_theme_op: 0,
    };
    app.refresh_panes();

    // Built unconditionally: the core channel is pumped whether or not the
    // Bus is connected (a no-broker windowed run still hears the core —
    // law 2).
    let streams = Streams {
        deliveries,
        core_events: pump(core_events),
        deadlines,
    };
    if STREAMS.set(Mutex::new(Some(streams))).is_err() {
        anyhow::bail!("app::run called twice in one process");
    }

    #[cfg(not(feature = "acceptance"))]
    let fixture_task = Task::none();
    application::start(
        (app, fixture_task),
        Dopus::update,
        Dopus::view,
        application::Window::new(APP_ID, Size::new(980.0, 640.0), ui_font)
            .minimum(Size::new(420.0, 240.0))
            .defer_close(),
    )
    .title(Dopus::title)
    .frame_presentation(Dopus::frame_binding)
    .subscription(Dopus::subscription)
    .theme(|app: &Dopus| app.content().theme.iced_theme())
    .style(|app: &Dopus, _| application::iced::theme::Style {
        background_color: app.content().theme.tokens.palette.surface,
        text_color: app.content().theme.tokens.palette.text,
    })
    .run()
    .map_err(|e| anyhow::anyhow!("window: {e}"))
}

/// Forward raw core events into the UI thread's channel (law 2's transport;
/// the UI thread does the feeding). The receiver is drained forever: a dead
/// UI (channel closed) ends the pump.
fn pump(receiver: std::sync::mpsc::Receiver<CoreEvent>) -> UnboundedReceiver<CoreEvent> {
    let (tx, rx) = application::iced::futures::channel::mpsc::unbounded();
    std::thread::Builder::new()
        .name("dopus-core-events".to_owned())
        .spawn(move || {
            for event in receiver {
                if tx.unbounded_send(event).is_err() {
                    break;
                }
            }
        })
        .expect("spawning the core-event pump");
    rx
}

/// Messages that mean the human is already using the window. An initial
/// duplicate that was touched is theirs to keep: the handoff may still
/// forward the paths, but the window must not quit under the user.
fn is_user_input(msg: &Msg) -> bool {
    matches!(
        msg,
        Msg::Actions(_)
            | Msg::Pane(..)
            | Msg::PaneRows(..)
            | Msg::Go(..)
            | Msg::LocationEdit(_)
            | Msg::LocationInput(_)
            | Msg::LocationSubmit(_)
            | Msg::Split(_)
            | Msg::SidebarWidth(..)
            | Msg::DropTransfer(..)
            | Msg::Dialog(_)
    )
}

/// One cancellable deadline wait. With no pending work the thread blocks
/// indefinitely; new state replaces its wait rather than starting a timer.
fn maintenance() -> (
    std::sync::mpsc::Sender<Option<Instant>>,
    UnboundedReceiver<Instant>,
) {
    let (tx, rx) = application::iced::futures::channel::mpsc::unbounded();
    let (arm, waits) = std::sync::mpsc::channel::<Option<Instant>>();
    std::thread::Builder::new()
        .name("dopus-deadline".to_owned())
        .spawn(move || {
            let mut deadline = Some(Instant::now());
            loop {
                let request = if let Some(at) = deadline {
                    waits.recv_timeout(at.saturating_duration_since(Instant::now()))
                } else {
                    waits
                        .recv()
                        .map_err(|_| std::sync::mpsc::RecvTimeoutError::Disconnected)
                };
                match request {
                    Ok(next) => deadline = next,
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        deadline = None;
                        if tx.unbounded_send(Instant::now()).is_err() {
                            return;
                        }
                    }
                }
            }
        })
        .expect("spawning the deadline worker");
    (arm, rx)
}

#[test]
fn maintenance_wait_cancels_rearms_and_stays_quiet_after_expiry() {
    use application::iced::futures::StreamExt;
    use std::time::Duration;
    let (arm, mut wakes) = maintenance();
    application::iced::futures::executor::block_on(wakes.next()).expect("startup wake");
    arm.send(Some(Instant::now() + Duration::from_millis(20)))
        .unwrap();
    arm.send(None).unwrap();
    std::thread::sleep(Duration::from_millis(60));
    assert!(wakes.try_recv().is_err(), "cancelled wait must stay quiet");
    arm.send(Some(Instant::now() + Duration::from_millis(20)))
        .unwrap();
    std::thread::sleep(Duration::from_millis(60));
    assert!(wakes.try_recv().is_ok(), "rearmed wait must fire");
    std::thread::sleep(Duration::from_millis(60));
    assert!(
        wakes.try_recv().is_err(),
        "expired wait must not become a heartbeat"
    );
}

/// The receivers the subscription drains, handed over once.
struct Streams {
    deliveries: application::iced::futures::channel::mpsc::Receiver<Delivery>,
    core_events: UnboundedReceiver<CoreEvent>,
    deadlines: UnboundedReceiver<Instant>,
}

static STREAMS: OnceLock<Mutex<Option<Streams>>> = OnceLock::new();

/// Bus deliveries, core events and deadline wakes, merged. Built once: iced
/// keeps a `Subscription::run` alive for as long as it is returned.
fn streams() -> impl application::iced::futures::Stream<Item = Msg> {
    use application::iced::futures::StreamExt;
    let taken = STREAMS.get().and_then(|m| m.lock().ok()?.take());
    match taken {
        Some(s) => application::iced::futures::stream::select(
            application::iced::futures::stream::select(
                s.deliveries.map(Msg::Bus),
                s.core_events.map(Msg::Core),
            ),
            s.deadlines.map(Msg::Tick),
        )
        .boxed(),
        None => {
            tracing::error!(
                "dopus: the delivery streams were already taken; the window will not hear the core"
            );
            application::iced::futures::stream::empty().boxed()
        }
    }
}

/// `dopus.actions.list`'s table: the served actions with their effective chords.
fn action_table(keymap: &Keymap) -> Vec<ActionRow> {
    verbs::action_table(keymap)
}

impl Dopus {
    fn frame_binding(&self) -> Option<application::frames::FrameBinding> {
        let bus = self.bus.as_ref()?;
        self.settings.session().frame_stamp().map(|stamp| bus.frames.binding(stamp))
    }
    fn publish_frame_target(&self) {
        #[cfg(feature = "acceptance")]
        if let Some(bus) = &self.bus && let (Some(endpoint), Some(window)) = (&bus.fixture_frames, self.window)
            && let Err(error) = endpoint.publish(application::acceptance::frames::Target { window, stamp: self.settings.session().frame_stamp() }) {
            tracing::warn!(%error, "Dopus fixture frame target failed");
        }
    }
    fn title(&self) -> String {
        let pane = self.core.pane(self.core.active());
        format!(
            "{} — MixOS DOpus",
            dopus_core::sanitise_display_path(&pane.path)
        )
    }

    fn update(&mut self, msg: Msg) -> Task<Msg> {
        if !self.registered && !self.quitting && is_user_input(&msg) {
            self.bootstrap_touched = true;
        }
        let layout = self.drag_layout();
        let task = self.dispatch(msg);
        if self.quitting {
            return task;
        }
        let derived = self.core.tick(Instant::now());
        let maintenance = self.on_derived(derived);
        let next = self
            .core
            .next_deadline()
            .into_iter()
            .chain(keys::next_deadline(&self.router))
            .min();
        if next != self.maintenance_deadline {
            self.maintenance_deadline = next;
            let _ = self.maintenance.send(next);
        }
        // Bus actions can change pane geometry while the pointer is idle.
        // Retire the captured target bounds before drawing the new layout.
        if layout != self.drag_layout() {
            view::drag::lock(&self.drag).cancel();
        }
        // The view snapshot: refreshed on every message, so no core mutation
        // can be drawn stale.
        self.refresh_panes();
        Task::batch([task, maintenance])
    }

    fn drag_layout(&self) -> (Look, f32, [dopus_core::config::SidebarConfig; 2]) {
        use dopus_core::config::Sidebar;
        (
            self.look(),
            self.core.config_snapshot().split_ratio,
            [
                self.core.sidebar(Sidebar::Places),
                self.core.sidebar(Sidebar::Properties),
            ],
        )
    }

    fn dispatch(&mut self, msg: Msg) -> Task<Msg> {
        match msg {
            Msg::Bus(delivery) => self.on_delivery(delivery),
            Msg::Actions(actions) => self.on_actions(&actions),
            Msg::PaneRows(pane, msg) => self.on_rows(pane, msg),
            Msg::Pane(pane, op) => self.on_pane_op(pane, op),
            Msg::Go(pane, path) => {
                // Clicking away ends a location edit (same path as
                // LocationCancel) before the navigation lands.
                self.stop_editing();
                self.core.navigate(pane, path);
                Task::none()
            }
            Msg::LocationEdit(pane) => self.begin_edit(pane),
            Msg::LocationInput(text) => {
                if let Some((_, current)) = &mut self.editing {
                    *current = text;
                }
                Task::none()
            }
            Msg::LocationSubmit(pane) => {
                let text = self
                    .editing
                    .as_ref()
                    .map(|(_, text)| text.clone())
                    .unwrap_or_default();
                self.stop_editing();
                // An empty submit is a cancel: the bar was cleared, not
                // aimed anywhere — navigating to "" would be a permanent
                // error status.
                if text.is_empty() {
                    return Task::none();
                }
                // Leading `~` expands to home; a file path lands on its
                // parent (verbs::navigable); the core re-lists and
                // status-lines a path it cannot read.
                self.core
                    .navigate(pane, verbs::navigable(crate::dirs::expand_tilde(&text)));
                Task::none()
            }
            Msg::LocationCancel => {
                self.stop_editing();
                Task::none()
            }
            Msg::Split(ratio) => {
                self.stop_editing();
                // Law 7: persistence derives from core state only — the core
                // settles the changed ratio through its own debounce.
                self.core
                    .set_split_ratio(ratio.clamp(view::panes::SPLIT_MIN, view::panes::SPLIT_MAX));
                Task::none()
            }
            Msg::SidebarWidth(sidebar, width) => {
                self.stop_editing();
                self.core.set_sidebar_width(sidebar, width);
                Task::none()
            }
            Msg::Core(event) => {
                // Law 2: every raw event through on_event exactly once.
                let derived = self.core.on_event(event);
                self.on_derived(derived)
            }
            Msg::Window(id, event) => self.on_window(id, event),
            Msg::Tick(_now) => {
                let actions = keys::poll_timeout(&self.router);
                self.on_actions(&actions)
            }
            Msg::Dialog(msg) => self.on_dialog(msg),
            Msg::DialogKey(key) => self.on_dialog_key(key),
            Msg::DropTransfer(pane, source, destination, action) => {
                if let Err(error) =
                    self.core
                        .transfer_paths(pane, vec![source], destination, action)
                {
                    self.status = Some(error);
                }
                Task::none()
            }
            Msg::Noop => Task::none(),
        }
    }

    /// Refresh the per-pane view snapshots (listings + the split ratio).
    fn refresh_panes(&mut self) {
        self.rows = [
            self.core.visible_rows(PaneId::Left),
            self.core.visible_rows(PaneId::Right),
        ];
        let look = self.look();
        for (index, pane) in [PaneId::Left, PaneId::Right].into_iter().enumerate() {
            self.column_cache[index].refresh(look, self.core.pane(pane), &self.rows[index]);
        }
        self.split_ratio = self.core.config_snapshot().split_ratio;
        let mut drag = view::drag::lock(&self.drag);
        if let Some(gesture) = drag.active.as_ref().or(drag.pending.as_ref())
            && (self.dialog.is_some()
                || self.core.availability().operation_running
                || self.core.pane(gesture.pane).path != gesture.source_root
                || gesture.target.as_ref().is_some_and(|target| {
                    let pane = gesture.pane.other();
                    self.core.pane(pane).path != target.root
                        || (target.path != target.root
                            && !self.rows[pane.index()]
                                .iter()
                                .any(|row| row.entry.is_dir && row.entry.path == target.path))
                })
                || !self.rows[gesture.pane.index()]
                    .iter()
                    .any(|row| row.entry.path == gesture.source))
        {
            drag.cancel();
        }
    }

    fn on_rows(&mut self, pane: PaneId, msg: rows::RowsMsg) -> Task<Msg> {
        match msg {
            // Any press lands in the listing: clicking a pane makes it the
            // active one (the divider/keyboard follow the same core state).
            // A click away from a location bar also ends its edit (the
            // editor's own clicks swap panes cleanly before this).
            rows::RowsMsg::Press => {
                self.stop_editing();
                self.core.set_active_pane(pane);
            }
            rows::RowsMsg::Select(path) => self.core.select_path(pane, Some(path)),
            rows::RowsMsg::SelectModified(path, ctrl, shift) => {
                self.core.select_modified(pane, path, ctrl, shift);
            }
            rows::RowsMsg::ContextMenu(path, _) => {
                self.stop_editing();
                self.core.set_active_pane(pane);
                if path
                    .as_ref()
                    .is_none_or(|path| !self.core.pane(pane).selected_paths.contains(path))
                {
                    self.core.select_path(pane, path);
                } else if let Some(path) = path {
                    self.core.focus_selected_path(pane, path);
                }
            }
            rows::RowsMsg::Toggle(path) => self.core.toggle_expand(pane, &path),
        }
        Task::none()
    }

    /// Activate `pane`, then act on it (the core's pane verbs act on the
    /// active pane). Law 5: a sort-column switch passes `ascending: true`.
    fn on_pane_op(&mut self, pane: PaneId, op: PaneOp) -> Task<Msg> {
        self.stop_editing();
        self.core.set_active_pane(pane);
        match op {
            PaneOp::Sort(column) => self.core.set_sort(column, true),
        }
        Task::none()
    }

    /// Enter edit mode on `pane`'s location bar: activate the pane (the bar
    /// was clicked, so that pane takes the keyboard), seed the editor with
    /// the pane's REAL path (the sanitisation law covers display text only)
    /// and hand keyboard focus to the field.
    fn begin_edit(&mut self, pane: PaneId) -> Task<Msg> {
        self.core.set_active_pane(pane);
        let path = pane_path_text(&self.core, pane);
        self.editing = Some((pane, path));
        keys::set_focus_editable(&self.router, true);
        Task::batch([
            application::iced::widget::operation::focus(view::location::location_id(pane)),
            application::iced::widget::operation::select_all(view::location::location_id(pane)),
        ])
    }

    /// Leave edit mode (submit, cancel) and give the chords back.
    fn stop_editing(&mut self) {
        if self.editing.take().is_some() {
            keys::set_focus_editable(&self.router, false);
        }
    }

    /// Law 3 and law 4, applied to the core's derived events: dialogs join
    /// the modal queue, `OpenFile` spawns `xdg-open` detached (fire and
    /// forget, filemgr's browser.rs:3270 shape; only the spawn FAILURE is
    /// reported — a status line), and core status lines surface in the
    /// status bar until the next info change. `RefreshAll` needs no arm: the
    /// core re-lists both panes itself before emitting it, and the view
    /// re-snapshots after every message.
    fn on_derived(&mut self, events: Vec<CoreEvent>) -> Task<Msg> {
        let mut task = Task::none();
        for event in events {
            match event {
                CoreEvent::ConfirmRequested { token, message } => {
                    // A dialog takes the keyboard from anything else (the
                    // core queued it; the oldest renders first).
                    self.stop_editing();
                    self.modal_queue.offer(&mut self.dialog, dialogs::Dialog::Confirm { token, message });
                    self.sync_modal();
                }
                CoreEvent::PromptRequested { token, kind, initial } => {
                    self.stop_editing();
                    self.modal_queue.offer(&mut self.dialog, dialogs::Dialog::prompt(token, kind, initial));
                    self.sync_modal();
                    if matches!(self.dialog, Some(dialogs::Dialog::Prompt { .. })) {
                        task = focus_prompt();
                    }
                }
                CoreEvent::OpenFile(path) => {
                    match std::process::Command::new("xdg-open").arg(&path).spawn() {
                        Ok(mut child) => {
                            // Reap off the UI thread: an unreaped Child stays
                            // a zombie until dopus exits. One detached wait
                            // per open; the UI stays fire-and-forget. If the
                            // reaper thread cannot start (thread limit), the
                            // child merely stays a zombie until exit — the
                            // filemgr precedent — never a UI-thread panic
                            // over an open (round-2 finding).
                            if std::thread::Builder::new()
                                .name("dopus-xdg-open-reap".to_owned())
                                .spawn(move || drop(child.wait()))
                                .is_err()
                            {
                                tracing::warn!("dopus: xdg-open reaper did not start; the child stays unreaped until exit");
                            }
                        }
                        Err(error) => {
                            self.status = Some(format!(
                                "Opening {}: {error}",
                                dopus_core::sanitise_display_path(&path)
                            ));
                        }
                    }
                }
                CoreEvent::Status { kind: dopus_core::StatusKind::Message, text, .. } => {
                    self.status = Some(text);
                }
                CoreEvent::Status { kind: dopus_core::StatusKind::Summary, .. } => {}
                // The core's info line (operation results among them) is
                // authoritative again.
                CoreEvent::InfoChanged => self.status = None,
                CoreEvent::SelectionChanged { .. } => {}
                CoreEvent::ListingStarted { .. }
                | CoreEvent::ListingArrived { .. }
                | CoreEvent::CountArrived { .. }
                | CoreEvent::PropertiesArrived { .. }
                | CoreEvent::OperationArrived { .. }
                // The view re-renders from the core after every message, so
                // "config persisted" and "both panes stale" need no reaction.
                | CoreEvent::ConfigSettled(_)
                | CoreEvent::RefreshAll => {}
            }
        }
        task
    }

    /// Mirror the dialog state into the key router: while a dialog is up its
    /// modal scope suppresses every chord and the router hands Enter/Escape
    /// to the dialog.
    fn sync_modal(&mut self) {
        keys::set_modal(&self.router, self.dialog.is_some());
    }

    /// A dialog control: the confirm's buttons, or the prompt's field and
    /// buttons.
    fn on_dialog(&mut self, msg: DialogMsg) -> Task<Msg> {
        match msg {
            DialogMsg::Answer(yes) => {
                let Some(dialogs::Dialog::Confirm { token, .. }) = &self.dialog else {
                    return Task::none();
                };
                let token = *token;
                self.core.confirm(
                    token,
                    if yes {
                        ConfirmAnswer::Yes
                    } else {
                        ConfirmAnswer::No
                    },
                );
                self.advance_dialog()
            }
            DialogMsg::Input(text) => {
                if let Some(dialog) = &mut self.dialog {
                    dialog.input(text);
                }
                Task::none()
            }
            DialogMsg::Submit => self.submit_prompt(),
            DialogMsg::Dismiss => {
                // A scrim press lands on BOTH dialog kinds (dialogs.rs's
                // frame wraps them alike): fail-closed each way — a
                // dismissed confirm is No (the dialogs.rs law), a dismissed
                // prompt is `prompt_text(token, None)`. Nothing runs either
                // way.
                match &self.dialog {
                    Some(dialogs::Dialog::Confirm { token, .. }) => {
                        let token = *token;
                        self.core.confirm(token, ConfirmAnswer::No);
                        self.advance_dialog()
                    }
                    Some(dialogs::Dialog::Prompt { token, .. }) => {
                        let token = *token;
                        self.core.prompt_text(token, None);
                        self.advance_dialog()
                    }
                    None => Task::none(),
                }
            }
        }
    }

    /// Enter/Escape under a dialog (the router's modal capture): confirm/
    /// submit and dismiss respectively. A prompt only submits while its live
    /// validation is satisfied — an invalid name is not a resolution.
    fn on_dialog_key(&mut self, key: ModalKey) -> Task<Msg> {
        let Some(dialog) = &self.dialog else {
            return Task::none();
        };
        match (dialog, key) {
            (dialogs::Dialog::Confirm { token, .. }, ModalKey::Confirm) => {
                let token = *token;
                self.core.confirm(token, ConfirmAnswer::Yes);
                self.advance_dialog()
            }
            (dialogs::Dialog::Confirm { token, .. }, ModalKey::Dismiss) => {
                let token = *token;
                self.core.confirm(token, ConfirmAnswer::No);
                self.advance_dialog()
            }
            (dialogs::Dialog::Prompt { .. }, ModalKey::Confirm) => self.submit_prompt(),
            (dialogs::Dialog::Prompt { token, .. }, ModalKey::Dismiss) => {
                let token = *token;
                self.core.prompt_text(token, None);
                self.advance_dialog()
            }
        }
    }

    /// Submit the front prompt: gated on the live validator (the core
    /// re-validates; this keeps the field from ever reaching it invalid).
    fn submit_prompt(&mut self) -> Task<Msg> {
        let Some(dialogs::Dialog::Prompt { token, text, .. }) = &self.dialog else {
            return Task::none();
        };
        let (token, text) = (*token, text.clone());
        let Ok(name) = dopus_core::validate_filename(&text) else {
            // The field already shows the validator's message; the
            // reservation stays open for a correction or a dismissal.
            return Task::none();
        };
        self.core.prompt_text(token, Some(name));
        self.advance_dialog()
    }

    /// The front dialog was answered: consume it and show the next queued
    /// one (the keyboard follows whatever is on screen).
    fn advance_dialog(&mut self) -> Task<Msg> {
        self.dialog = None;
        self.modal_queue.next(&mut self.dialog);
        self.sync_modal();
        if matches!(self.dialog, Some(dialogs::Dialog::Prompt { .. })) {
            return focus_prompt();
        }
        Task::none()
    }

    fn on_delivery(&mut self, delivery: Delivery) -> Task<Msg> {
        // EVERY Bus delivery drains the settings mailbox: a prepared stage
        // may already be waiting behind a coalesced wake, and the drain
        // fences it against the live connection generation on the UI loop.
        let before = self.look();
        let changed = self.settings.drain_with(
            || self.bus.as_ref().and_then(|bus| bus.settings_generation()),
            view_activation(
                &self.core,
                &self.rows,
                &mut self.column_cache,
                &self.measurements,
                &self.drag,
                &mut self.tint,
                before,
            ),
        );
        if !changed.is_empty() {
            tracing::info!(
                elapsed_ms = self.launched.elapsed().as_millis(),
                "DOpus settings activated"
            );
        }
        self.publish_frame_target();
        match delivery {
            Delivery::Command(command) => self.serve(&command),
            Delivery::Connected => {
                tracing::info!("Bus connected as `{}`", self.service);
                Task::none()
            }
            Delivery::Disconnected => {
                tracing::warn!("Bus disconnected; reconnecting in the background");
                Task::none()
            }
            // The mailbox was already drained above; the wake itself needs
            // no further action.
            Delivery::Settings => Task::none(),
            Delivery::Registered => {
                if !self.registered {
                    self.registered = true;
                    self.registration_refused = false;
                    let paths = std::mem::take(&mut self.bootstrap_paths);
                    verbs::apply_open_paths(&mut self.core, &paths);
                }
                Task::none()
            }
            Delivery::RegistrationFailed(error) => {
                self.registration_refused = true;
                let name_taken = matches!(&error, bus::StartError::NameTaken);
                if name_taken {
                    // An initial refused duplicate must never persist its
                    // config over the registered owner.
                    self.lost_race = true;
                }
                self.status = Some(format!("Bus: {error}"));
                if !self.registered
                    && !self.bootstrap_touched
                    && !self.handoff_pending
                    && name_taken
                {
                    // The single-instance forward: only a TYPED NameTaken
                    // (the shared client's classification — no text match
                    // here), only before this instance ever owned the name.
                    self.handoff_pending = true;
                    if let Some(bus) = &self.bus {
                        bus.forward_open(self.bootstrap_paths.clone());
                    }
                } else if !self.registered && !self.handoff_pending {
                    // A non-duplicate refusal (or an untouched window): the
                    // window is ours for good — apply the launch paths.
                    let paths = std::mem::take(&mut self.bootstrap_paths);
                    verbs::apply_open_paths(&mut self.core, &paths);
                }
                Task::none()
            }
            Delivery::Forwarded(result) => {
                if !self.handoff_pending {
                    return Task::none();
                }
                self.handoff_pending = false;
                match result {
                    Ok(())
                        if !self.registered
                            && !self.bootstrap_touched
                            && self
                                .bus
                                .as_ref()
                                .is_none_or(|bus| bus.registration_generation() == 0) =>
                    {
                        self.quit()
                    }
                    Ok(()) => Task::none(),
                    Err(error) => {
                        // No answer: the window is ours — apply the launch
                        // paths locally and keep running.
                        self.status = Some(error);
                        let paths = std::mem::take(&mut self.bootstrap_paths);
                        verbs::apply_open_paths(&mut self.core, &paths);
                        Task::none()
                    }
                }
            }
            Delivery::ThemeApplied(result) => match result {
                Ok((scheme, mode)) => {
                    self.status = Some(format!("Appearance: {scheme} · {mode}"));
                    Task::none()
                }
                Err(message) => {
                    self.status = Some(message);
                    Task::none()
                }
            },
            Delivery::Stopped { faults } => {
                for fault in faults {
                    tracing::warn!("{fault}");
                }
                Task::none()
            }
        }
    }

    fn server_meta(&self) -> ServerMeta {
        let content = self.content();
        ServerMeta {
            service: self.service.clone(),
            headless: false,
            location_focus_available: self.dialog.is_none() && !self.quitting,
            config_path: self
                .dirs
                .as_ref()
                .map(|d| d.config_dir().join("config.conf.mix").display().to_string()),
            theme_scheme: content.theme.scheme.name().to_owned(),
            theme_mode: content.theme.mode.name().to_owned(),
            appearance: crate::verbs::AppearanceState {
                icons: content.icons.mode().to_owned(),
                asset_set: content.icons.asset_set().map(str::to_owned),
                font_ui: content.theme.ui.0.clone(),
                font_mono: content.theme.mono.0.clone(),
                font_ui_weight: content.theme.ui_font.weight.value(),
                icon_weight: content.icons.weight(),
            },
            actions: self.action_table.clone(),
        }
    }

    /// Answer one Bus command through the shared serving layer.
    fn serve(&mut self, command: &bus::Command) -> Task<Msg> {
        let Some(bus) = &self.bus else {
            return Task::none();
        };
        let handle = bus.clone();
        if !handle.is_current(&command.id) {
            return Task::none();
        }
        // Reconcile the consumer with the ACTUAL live connection generation
        // before any command-native info is built: dopus.state/app.describe
        // report what the connection really carries, never a stale sample.
        self.settings.reconcile(handle.settings_generation());
        let mut meta = self.server_meta();
        meta.service = handle.service_name().into();
        let info = buildinfo::build_info!();
        let mut tasks = Vec::new();
        for served in verbs::serve_command(command, &mut self.core, &meta, &info) {
            match served {
                Served::ToggleSidebar {
                    id,
                    sidebar,
                    action,
                } => {
                    self.toggle_sidebar(sidebar);
                    handle.respond(
                        id,
                        0,
                        serde_json::to_string(&verbs::ActionReply {
                            id: action,
                            ok: true,
                            result: None,
                        })
                        .unwrap_or_default(),
                    );
                }
                Served::Reply { id, rc, body } => {
                    let (rc, body) = if command.verb == "app.describe" && rc == 0 {
                        let mut value: serde_json::Value =
                            serde_json::from_str(&body).expect("typed description");
                        match application::describe::complete_native(
                            &mut value,
                            application::describe::Identity {
                                app_id: Some(APP_ID),
                                version: env!("CARGO_PKG_VERSION"),
                                pid: std::process::id(),
                                service: handle.service_name(),
                            },
                            self.settings.session(),
                        ) {
                            Ok(()) => (0, value.to_string()),
                            Err(error) => (10, verbs::describe_refusal(&error)),
                        }
                    } else if command.verb == "dopus.state" && rc == 0 {
                        // The canonical settings evidence (reconciled at the
                        // top of serve) joins the actual client state.
                        match serde_json::from_str::<serde_json::Value>(&body) {
                            Ok(mut value) if value.is_object() => {
                                value["settings"] = serde_json::to_value(
                                    self.settings.session().host().consumer().evidence(),
                                )
                                .expect("settings evidence serialises");
                                value["settings_cache"] =
                                    serde_json::json!(self.settings.session().cache_evidence());
                                value["ui"] = serde_json::json!({
                                    "location_draft": self.editing.as_ref().map(|(pane, text)| {
                                        serde_json::json!({
                                            "pane": match pane { PaneId::Left => "left", PaneId::Right => "right" },
                                            "text": text,
                                        })
                                    }),
                                });
                                (rc, value.to_string())
                            }
                            _ => (rc, body),
                        }
                    } else {
                        (rc, body)
                    };
                    handle.respond(id, rc, body);
                }
                Served::LocationFocus { id, pane } => {
                    tasks.push(self.serve_location_focus(id, pane))
                }
                Served::ThemeSet { id, scheme, mode } => {
                    tasks.push(self.theme_request(Some(id), scheme.as_deref(), mode.as_deref()));
                }
                Served::ThemeAction { id, action } => match action {
                    verbs::ThemeAction::Scheme(name) => {
                        tasks.push(self.theme_request(Some(id), Some(&name), None));
                    }
                    // Mode-toggle resolves against the live applied
                    // selection first (the keyboard path's rule).
                    verbs::ThemeAction::ModeToggle => {
                        let mode = match self.content().theme.mode {
                            Mode::Dark => Mode::Light,
                            _ => Mode::Dark,
                        };
                        tasks.push(self.theme_request(Some(id), None, Some(mode.name())));
                    }
                },
                Served::Quit { id } => {
                    handle.respond(
                        id,
                        0,
                        serde_json::to_string(&verbs::QuitReply { quitting: true })
                            .unwrap_or_default(),
                    );
                    // Quit terminates the batch: the whole Vec is processed
                    // IN ORDER and everything after the FIRST Quit is
                    // dropped — nothing past a quitting process's last
                    // reply is answerable (serve_command never appends
                    // after Quit today; a future verb doing so loses only
                    // what could not have been answered anyway).
                    return self.quit();
                }
            }
        }
        Task::batch(tasks)
    }

    /// Window performer for Bus location.focus; keyboard uses the same editor.
    fn serve_location_focus(&mut self, id: bus::Request, pane: PaneId) -> Task<Msg> {
        // Repeated Bus focus must not replace the human's unfinished draft.
        // Switching panes still starts an editor seeded from the new path.
        let task = if self
            .editing
            .as_ref()
            .is_some_and(|(editing_pane, _)| *editing_pane == pane)
        {
            Task::none()
        } else {
            self.begin_edit(pane)
        };
        if let Some(bus) = &self.bus {
            bus.respond(
                id,
                0,
                serde_json::to_string(&verbs::ActionReply {
                    id: actions::location::FOCUS.to_string(),
                    ok: true,
                    result: None,
                })
                .unwrap_or_default(),
            );
        }
        task
    }

    /// The theme performer, shared by the `dopus.theme.set` verb, the
    /// `dopus.action theme.*` actions and the keyboard path: a FENCED
    /// appearance mutation through the shared settings authority. The
    /// binding, incarnation and revision are captured from the CONFIRMED
    /// consumer read; the bus worker validates then applies and reports the
    /// real receipt (the reply carries it; the status line shows the
    /// outcome). Without a confirmed read (or a Bus) the request is refused
    /// as unsupported — never faked as success, never an app-local override.
    fn theme_request(
        &mut self,
        id: Option<bus::Request>,
        scheme: Option<&str>,
        mode: Option<&str>,
    ) -> Task<Msg> {
        let Some(bus) = &self.bus else {
            return Task::none();
        };
        let refuse = |message: String| {
            if let Some(id) = &id {
                bus.respond(
                    id.clone(),
                    10,
                    serde_json::to_string(&verbs::Refusal {
                        error_code: verbs::code::UNAVAILABLE.to_owned(),
                        message: message.clone(),
                        reason: Some("unsupported".to_owned()),
                    })
                    .unwrap_or_default(),
                );
            }
            message
        };
        let snapshot = match self.settings.session().host().consumer().current() {
            Some(snapshot) => snapshot.clone(),
            None => {
                let message = refuse(
                    "appearance settings are not available (no confirmed settings read)".into(),
                );
                self.status = Some(message);
                return Task::none();
            }
        };
        let mut changes = std::collections::BTreeMap::new();
        for (path, name) in [("appearance.scheme", scheme), ("appearance.mode", mode)] {
            let Some(name) = name else { continue };
            let known = match path {
                "appearance.scheme" => Scheme::from_name(name).is_some(),
                _ => Mode::from_name(name).is_some(),
            };
            if !known {
                let message = refuse(format!("unknown selection {name:?} for {path}"));
                self.status = Some(message);
                return Task::none();
            }
            changes.insert(path.to_owned(), serde_json::json!(name));
        }
        if changes.is_empty() {
            // A no-op theme request reports the live applied selection — a
            // real result, not a faked write.
            let reply = verbs::ThemeSetReply {
                scheme: self.content().theme.scheme.name().to_owned(),
                mode: self.content().theme.mode.name().to_owned(),
            };
            if let Some(id) = &id {
                bus.respond(
                    id.clone(),
                    0,
                    serde_json::to_string(&reply).unwrap_or_default(),
                );
            }
            return Task::none();
        }
        // The resulting selection for the reply: the applied names are the
        // requested ones (an `unchanged` receipt counts as applied).
        let current = &self.content().theme;
        let scheme_name = scheme
            .map(str::to_owned)
            .unwrap_or_else(|| current.scheme.name().to_owned());
        let mode_name = mode
            .map(str::to_owned)
            .unwrap_or_else(|| current.mode.name().to_owned());
        self.next_theme_op += 1;
        let request = bus::ThemeRequest {
            reply_id: id,
            binding: snapshot.binding.clone(),
            expected_incarnation: snapshot.incarnation.clone(),
            expected_revision: snapshot.revision,
            operation_id: format!("dopus-theme-{}-{}", std::process::id(), self.next_theme_op),
            changes,
            scheme: scheme_name,
            mode: mode_name,
        };
        if let Err(error) = bus.theme_apply(request) {
            self.status = Some(error);
        }
        Task::none()
    }

    /// The keyboard/menu path: `theme.*` here, everything else through the
    /// shared [`verbs::apply_action`].
    fn on_actions(&mut self, actions: &[ActionId]) -> Task<Msg> {
        let mut quit = false;
        let mut tasks = Vec::new();
        for action in actions {
            if *action == actions::filemgr::NAV_SWITCH_PANE {
                self.stop_editing();
            }
            if *action == actions::theme::MODE_TOGGLE {
                let mode = match self.content().theme.mode {
                    Mode::Dark => Mode::Light,
                    _ => Mode::Dark,
                };
                tasks.push(self.theme_request(None, None, Some(mode.name())));
                continue;
            }
            if let Some(name) = verbs::scheme_action(*action) {
                // The action names are exactly the scheme names; the fenced
                // request validates them against the authority's vocabulary.
                tasks.push(self.theme_request(None, Some(name), None));
                continue;
            }
            match verbs::apply_action(*action, &mut self.core) {
                Ok(verbs::Applied::Done) => {}
                Ok(verbs::Applied::ToggleSidebar(sidebar)) => self.toggle_sidebar(sidebar),
                Ok(verbs::Applied::LocationFocus(pane)) => tasks.push(self.begin_edit(pane)),
                // Unreachable from this path (the theme pre-filter above
                // consumed every theme id) but the shared layer must stay
                // exhaustive: perform the selection the same way the Bus
                // arm does.
                Ok(verbs::Applied::Theme(action)) => match action {
                    verbs::ThemeAction::Scheme(name) => {
                        tasks.push(self.theme_request(None, Some(&name), None));
                    }
                    verbs::ThemeAction::ModeToggle => {
                        let mode = match self.content().theme.mode {
                            Mode::Dark => Mode::Light,
                            _ => Mode::Dark,
                        };
                        tasks.push(self.theme_request(None, None, Some(mode.name())));
                    }
                },
                Ok(verbs::Applied::Quit) => quit = true,
                Err(refusal) => self.status = Some(refusal.message),
            }
        }
        if quit {
            return self.quit();
        }
        Task::batch(tasks)
    }

    fn on_window(
        &mut self,
        id: application::iced::window::Id,
        event: application::iced::window::Event,
    ) -> Task<Msg> {
        if self.window.is_some_and(|window| window != id) {
            return Task::none();
        }
        match event {
            application::iced::window::Event::Opened { scale_factor, .. } => {
                self.window = Some(id);
                self.output_scale(scale_factor);
                self.publish_frame_target();
            }
            application::iced::window::Event::Rescaled(scale) => {
                if self.window == Some(id) {
                    self.output_scale(scale);
                    self.publish_frame_target();
                }
            }
            application::iced::window::Event::Focused => {
                // filemgr's `reload_keymap_on_focus` rule: pick up keymap
                // edits, cancel a pending chord either way.
                let keymap_path = self.dirs.as_ref().map(|d| d.keymap_file());
                keys::reload(&self.router, keymap_path.as_deref());
                {
                    let router = self
                        .router
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    self.action_table = action_table(&router.keymap);
                }
            }
            application::iced::window::Event::CloseRequested => return self.quit(),
            application::iced::window::Event::Closed => {
                if self.window == Some(id) && let Some(bus) = &self.bus { bus.frames.close(); }
            }
            application::iced::window::Event::Unfocused => {
                keys::cancel(&self.router);
                view::drag::lock(&self.drag).cancel()
            }
            application::iced::window::Event::Resized(_) => view::drag::lock(&self.drag).cancel(),
            _ => {}
        }
        Task::none()
    }

    fn output_scale(&mut self, scale: f32) {
        let result = PreparationContext::new(scale).and_then(|context| {
            self.settings
                .set_context(
                    context,
                    self.bus.as_ref().and_then(BusHandle::settings_generation),
                )
                .map(|_| ())
        });
        if let Err(error) = result {
            self.status = Some(error.message);
        }
    }

    fn quit(&mut self) -> Task<Msg> {
        if self.quitting {
            return Task::none();
        }
        self.quitting = true;
        // Persist the latest settings even before their settle deadline —
        // but an initial refused duplicate must not overwrite the config the
        // registered owner is using.
        if !self.lost_race {
            let derived = self.core.flush_config();
            let _ = self.on_derived(derived);
        }
        if let Some(bus) = &self.bus {
            bus.quit();
            // The worker drains accepted replies and the settings cache
            // within its single two-second budget; its done receipt is
            // authoritative (a silent timeout is reported, never hidden).
            match bus.wait_done(std::time::Duration::from_secs(3)) {
                Ok(faults) => {
                    eprintln!("DOPUS_SHUTDOWN {}", serde_json::json!({ "faults": faults }));
                }
                Err(error) => {
                    eprintln!(
                        "DOPUS_SHUTDOWN {}",
                        serde_json::json!({ "faults": [error] })
                    );
                }
            }
        }
        application::iced::exit()
    }

    fn subscription(&self) -> Subscription<Msg> {
        Subscription::batch([
            Subscription::run(streams),
            application::iced::event::listen_with(|event, _status, window| match event {
                application::iced::Event::Window(
                    e @ (application::iced::window::Event::Opened { .. }
                    | application::iced::window::Event::Rescaled(_)
                    | application::iced::window::Event::Resized(_)
                    | application::iced::window::Event::Focused
                    | application::iced::window::Event::Unfocused
                    | application::iced::window::Event::Closed
                    | application::iced::window::Event::CloseRequested),
                ) => Some(Msg::Window(window, e)),
                _ => None,
            }),
        ])
    }

    /// Borrow the live prepared content: the activated presentation, or the
    /// generic bootstrap until the first fenced activation. The app never
    /// stores a copy — the session owns the checked [`Prepared`]
    /// (appearance::settings::Prepared) and this is its only lookup.
    fn content(&self) -> &Content {
        self.settings
            .session()
            .host()
            .presentation()
            .map(|presentation| presentation.content())
            .unwrap_or(&self.bootstrap)
    }

    /// The persistent provenance/fault status: which settings generation
    /// the window presents, and what its Bus registration state is.
    fn persistent_status(&self) -> String {
        use settings::fallback::PresentationKind;
        let kind = match self.settings.session().host().consumer().evidence().kind {
            Some(PresentationKind::Current) => "settings-current",
            Some(PresentationKind::Cached) => "settings-cached",
            Some(PresentationKind::Embedded) => "settings-embedded",
            Some(PresentationKind::Retained) => "settings-retained",
            Some(PresentationKind::LastGood) => "settings-last-good",
            None => "settings-bootstrap",
        };
        let connection = if self.bus.as_ref().is_some_and(|bus| bus.connected()) {
            "bus-connected"
        } else if self.registration_refused {
            "bus-refused"
        } else if self.registered {
            "bus-disconnected"
        } else {
            "bus-connecting"
        };
        format!(
            "{} · {}",
            crate::strings::label(kind),
            crate::strings::label(connection)
        )
    }

    fn look(&self) -> Look {
        Look::from_theme(&self.content().theme)
    }

    fn toggle_sidebar(&mut self, sidebar: dopus_core::config::Sidebar) {
        self.stop_editing();
        self.core.toggle_sidebar(sidebar);
    }

    fn context_items(&self) -> Vec<toolkit::menu::Item<Msg>> {
        use actions::filemgr;
        use toolkit::menu::Item;
        let pane = self.core.pane(self.core.active());
        let busy = self.core.availability().operation_running;
        let item = |action: ActionId, label: &str| {
            let keys = self
                .action_table
                .iter()
                .find(|row| row.id == action.as_str())
                .map(|row| row.keys.join(" / "))
                .unwrap_or_default();
            Item::action(label, Msg::Actions(vec![action]))
                .accelerator(keys)
                .enabled(view::toolbar::enabled(pane, busy, action))
        };
        vec![
            item(filemgr::FILE_OPEN, "Open"),
            Item::separator(),
            item(filemgr::FILE_COPY, "Copy to other pane"),
            item(filemgr::FILE_MOVE, "Move to other pane"),
            item(filemgr::FILE_RENAME, "Rename"),
            item(filemgr::FILE_DELETE, "Delete"),
            Item::separator(),
            item(filemgr::FILE_NEW_FOLDER, "New folder"),
            item(filemgr::VIEW_REFRESH, "Refresh"),
            item(
                filemgr::VIEW_TOGGLE_HIDDEN,
                if pane.show_hidden {
                    "Hide hidden files"
                } else {
                    "Show hidden files"
                },
            ),
        ]
    }

    fn view(&self) -> Element<'_, Msg, application::iced::Theme, Renderer> {
        let info = self.status.as_deref().unwrap_or(self.core.info());
        let provenance = self.persistent_status();
        let editing = self
            .editing
            .as_ref()
            .map(|(pane, text)| (*pane, text.as_str()));
        let content = view::root(
            self.look(),
            &self.measurements,
            &self.content().icons,
            &self.tint,
            self.core.active(),
            self.split_ratio,
            self.core.pane(PaneId::Left),
            self.core.pane(PaneId::Right),
            &self.rows[0],
            &self.rows[1],
            editing,
            info,
            &provenance,
            self.dialog.as_ref(),
            self.core.places(),
            self.core.properties(self.core.active()),
            self.core.sidebar(dopus_core::config::Sidebar::Places),
            self.core.sidebar(dopus_core::config::Sidebar::Properties),
            &self.action_table,
            [
                self.column_cache[0].get(self.look()),
                self.column_cache[1].get(self.look()),
            ],
            self.drag.clone(),
            self.core.availability().operation_running,
        );
        #[cfg(feature = "acceptance")]
        let content = if self.bus.as_ref().is_some_and(|bus| bus.fixture_frames.is_some()) {
            application::iced::widget::container(content).width(application::iced::Fill).height(application::iced::Fill)
                .id(crate::acceptance::VIEWPORT_ID).into()
        } else { content };
        // The router wraps everything: it sees every key before its children
        // and publishes resolved actions (never `event::listen`, which drops
        // keys under load — the ced/term rule). While a dialog is up it
        // resolves nothing (the modal scope) and hands Enter/Escape to the
        // dialog instead.
        let mut routed = keys::router(content, self.router.clone(), Msg::Actions)
            .modal(self.dialog.is_some())
            .on_pending(Msg::Noop);
        if self.dialog.is_some() {
            routed = routed.on_modal_key(Msg::DialogKey);
        } else if let Some((pane, _)) = self.editing.as_ref() {
            routed = routed.on_edit_cancel(view::location::location_id(*pane), Msg::LocationCancel);
        }
        let content: Element<'_, Msg, application::iced::Theme, Renderer> = if self.dialog.is_none()
        {
            toolkit::menu::Menu::context(routed, self.context_items())
                .style(self.look().tokens.menu_style())
                .into()
        } else {
            routed.into()
        };
        let content = Element::new(view::drag::Layer::new(
            content,
            self.drag.clone(),
            self.look(),
            &self.content().icons,
            &self.tint,
        ));
        #[cfg(feature = "acceptance")]
        let content = if self.bus.as_ref().is_some_and(|bus| bus.fixture_frames.is_some()) {
            application::iced::widget::container(content).width(application::iced::Fill).height(application::iced::Fill)
                .id(crate::acceptance::ROOT_ID).into()
        } else { content };
        content
    }
}

/// The pane's REAL path as editor text (`to_string_lossy`: paths are OsStr;
/// non-UTF-8 bytes cannot be edited in a text field and round-trip as the
/// lossy form).
fn pane_path_text(core: &DopusCore, pane: PaneId) -> String {
    core.pane(pane).path.to_string_lossy().into_owned()
}

/// Hand keyboard focus to the prompt dialog's field, selecting its seeded
/// text (filemgr's name-edit shape: the whole initial name is selected, so
/// typing replaces it).
fn focus_prompt() -> Task<Msg> {
    Task::batch([
        application::iced::widget::operation::focus(dialogs::PROMPT_INPUT),
        application::iced::widget::operation::select_all(dialogs::PROMPT_INPUT),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "acceptance")]
    #[test]
    fn fixture_target_follows_only_the_owned_typed_window() {
        let (_dir, mut app, _lane) = fixture();
        let (mut bus, _responses) = BusHandle::response_sink();
        bus.fixture_frames = Some(application::acceptance::frames::Endpoint::new(bus.frames.clone()));
        let endpoint = bus.fixture_frames.as_ref().unwrap().clone();
        let frames = bus.frames.clone();
        app.bus = Some(bus);
        let window = application::iced::window::Id::unique();
        let foreign = application::iced::window::Id::unique();
        let opened = || application::iced::window::Event::Opened {
            position: None, size: Size::new(980.0, 640.0), scale_factor: 1.0,
        };
        let _ = app.on_window(window, opened());
        assert_eq!(endpoint.target().unwrap().window, window);
        assert!(endpoint.target().unwrap().stamp.is_none(), "bootstrap does not claim activation");
        assert!(app.frame_binding().is_none());
        let _ = app.on_window(foreign, opened());
        let _ = app.on_window(foreign, application::iced::window::Event::Closed);
        assert_eq!(app.window, Some(window));
        assert_eq!(endpoint.target().unwrap().window, window);
        assert!(!frames.snapshot().closed);
        assert!(frames.snapshot().last_presented.is_none());
        let _ = app.on_window(window, application::iced::window::Event::Closed);
        assert!(frames.snapshot().closed);
    }

    #[test]
    fn native_scale_events_are_ordered_window_owned_and_validated() {
        let (_dir, mut app, _lane) = fixture();
        let window = application::iced::window::Id::unique();
        let foreign = application::iced::window::Id::unique();
        let _ = app.on_window(
            window,
            application::iced::window::Event::Opened {
                position: None,
                size: Size::new(980.0, 640.0),
                scale_factor: 1.5,
            },
        );
        assert_eq!(app.window, Some(window));
        let first = app.settings.session().preparation_evidence().desired;
        let _ = app.on_window(foreign, application::iced::window::Event::Rescaled(2.0));
        assert_eq!(app.settings.session().preparation_evidence().desired, first);
        let _ = app.on_window(window, application::iced::window::Event::Rescaled(2.0));
        let second = app.settings.session().preparation_evidence().desired;
        assert!(second.get() > first.get());
        let _ = app.on_window(window, application::iced::window::Event::Rescaled(f32::NAN));
        assert_eq!(
            app.settings.session().preparation_evidence().desired,
            second
        );
        assert!(app.status.is_some());
    }

    #[test]
    fn contextual_scale_preparation_retains_old_handles_and_file_state_until_activation() {
        let (dir, mut app, mut lane) = fixture();
        let _ = activate(&mut app, &mut lane, settings::Desktop::default());
        app.editing = Some((PaneId::Left, "unfinished path".into()));
        let stamp = app.settings.session().frame_stamp().unwrap();
        let old = app
            .content()
            .icons
            .get(icons::Icon::Folder, &app.tint, icons::RASTER_PX)
            .unwrap();
        app.settings
            .set_context(PreparationContext::new(1.5).unwrap(), Some(1))
            .unwrap();
        assert_eq!(app.settings.session().frame_stamp(), Some(stamp));
        assert_eq!(
            app.content()
                .icons
                .get(icons::Icon::Folder, &app.tint, icons::RASTER_PX),
            Some(old.clone())
        );
        assert!(!app.settings.session().preparation_evidence().current);
        drive(&mut lane, 2);
        let _ = drain(&mut app);
        let installed = app.settings.session().frame_stamp().unwrap();
        assert!(installed.local_revision > stamp.local_revision);
        assert!(app.settings.session().preparation_evidence().current);
        let new = app
            .content()
            .icons
            .get(icons::Icon::Folder, &app.tint, icons::RASTER_PX)
            .unwrap();
        assert_ne!(new, old);
        let application::iced::widget::image::Handle::Rgba { width, height, .. } = new else {
            panic!("embedded ready image")
        };
        let side = (app.look().chrome.icon * 1.5).ceil() as u32;
        assert_eq!((width, height), (side, side));
        assert_eq!(app.core.pane(PaneId::Left).path, dir.path());
        assert_eq!(app.editing, Some((PaneId::Left, "unfinished path".into())));
    }

    #[cfg(feature = "acceptance")]
    #[test]
    fn fixture_target_keeps_installed_stamp_while_context_preparation_is_pending() {
        let (_dir, mut app, mut lane) = fixture();
        let _ = activate(&mut app, &mut lane, settings::Desktop::default());
        let stamp = app.settings.session().frame_stamp().unwrap();
        let (mut bus, _responses) = BusHandle::response_sink();
        let endpoint = application::acceptance::frames::Endpoint::new(bus.frames.clone());
        bus.fixture_frames = Some(endpoint.clone());
        let frames = bus.frames.clone();
        app.bus = Some(bus);
        let window = application::iced::window::Id::unique();
        app.window = Some(window);
        app.publish_frame_target();
        assert_eq!(endpoint.target().unwrap().stamp, Some(stamp));
        app.settings.set_context(PreparationContext::new(1.5).unwrap(), Some(1)).unwrap();
        app.publish_frame_target();
        assert_eq!(endpoint.target().unwrap().stamp, Some(stamp));
        drive(&mut lane, 2);
        let _ = drain(&mut app);
        app.publish_frame_target();
        let installed = app.settings.session().frame_stamp().unwrap();
        assert!(installed.local_revision > stamp.local_revision);
        assert_eq!(endpoint.target().unwrap().window, window);
        assert_eq!(endpoint.target().unwrap().stamp, Some(installed));
        assert!(app.frame_binding().is_some());
        assert!(frames.snapshot().last_presented.is_none(), "publication is not a presented receipt");
    }

    #[test]
    fn sidebar_bus_actions_reach_the_window_and_refuse_headless_or_busy() {
        use dopus_core::config::Sidebar;
        let (_dir, mut app, mut _lane) = fixture();
        let info = buildinfo::build_info!();
        for (action, sidebar) in [
            (actions::view::TOGGLE_PLACES, Sidebar::Places),
            (actions::view::TOGGLE_PROPERTIES, Sidebar::Properties),
        ] {
            let command = bus::Command {
                id: 43.into(),
                verb: "dopus.action".into(),
                body: format!(r#"{{"id":"{action}","pane":"right"}}"#),
                caller_key: "mesh:caller@example".into(),
            };
            let meta = app.server_meta();
            let before = app.core.sidebar(sidebar);
            let served = verbs::serve_command(&command, &mut app.core, &meta, &info);
            let [
                Served::ToggleSidebar {
                    sidebar: target, ..
                },
            ] = served.as_slice()
            else {
                panic!("missing window performer")
            };
            assert_eq!(*target, sidebar);
            assert_eq!(
                app.core.sidebar(sidebar),
                before,
                "dispatch alone must not mutate"
            );
            app.toggle_sidebar(*target);
            assert_eq!(app.core.sidebar(sidebar).open, !before.open);
            assert_eq!(app.core.active(), PaneId::Left);
            for headless in [true, false] {
                let mut meta = app.server_meta();
                meta.headless = headless;
                meta.location_focus_available = false;
                let before = app.core.sidebar(sidebar);
                let served = verbs::serve_command(&command, &mut app.core, &meta, &info);
                let [Served::Reply { rc: 10, body, .. }] = served.as_slice() else {
                    panic!("must refuse")
                };
                let refusal: verbs::Refusal = serde_json::from_str(body).unwrap();
                assert_eq!(refusal.error_code, "UNAVAILABLE");
                assert_eq!(
                    refusal.reason.as_deref(),
                    Some(if headless { "headless" } else { "window_busy" })
                );
                assert_eq!(app.core.sidebar(sidebar), before);
            }
        }
    }

    fn fixture_with(
        build: impl Fn(
            &appearance::settings::Prepared,
            &settings::Snapshot,
        ) -> Result<Content, Diagnostic>
        + Send
        + Sync
        + 'static,
    ) -> (
        tempfile::TempDir,
        Dopus,
        application::presentation::native::Lane<Content, PreparationContext>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let mut config = DOpusConfig::default();
        config.left.path = dir.path().to_owned();
        config.right.path = dir.path().to_owned();
        let (core, _events) = DopusCore::new(config, None);
        let binding = settings::Binding {
            instance: "fixture".into(),
            profile: "default".into(),
        };
        let consumer = settings::consumer::Consumer::for_app(binding, "dopus").unwrap();
        let (settings, lane) = application::presentation::native::bridge(
            application::presentation::native::Session::with_context(
                consumer,
                PreparationContext::default(),
            ),
            application::presentation::native::Worker::contextual(
                move |look, snapshot, _: &PreparationContext| build(look, snapshot),
            )
            .with_contextual_resource_requirements(icons::requirements),
        );
        let bootstrap = Content::bootstrap(&appearance::settings::bootstrap().unwrap()).unwrap();
        let tint = icons::tint_key(bootstrap.theme.tokens.palette.text);
        let app = Dopus {
            core,
            maintenance: std::sync::mpsc::channel().0,
            maintenance_deadline: None,
            rows: [Vec::new(), Vec::new()],
            column_cache: Default::default(),
            measurements: Default::default(),
            split_ratio: 0.5,
            editing: None,
            router: keys::initial(None).unwrap(),
            settings,
            window: None,
            bootstrap,
            tint,
            status: None,
            dialog: None,
            modal_queue: dialogs::ModalQueue::default(),
            bus: None,
            action_table: verbs::action_table(&keys::load(None).unwrap()),
            dirs: None,
            service: "dopus-test".into(),
            quitting: false,
            drag: Default::default(),
            registered: true,
            registration_refused: false,
            lost_race: false,
            bootstrap_paths: Vec::new(),
            bootstrap_touched: false,
            handoff_pending: false,
            launched: Instant::now(),
            next_theme_op: 0,
        };
        (dir, app, lane)
    }

    fn fixture() -> (
        tempfile::TempDir,
        Dopus,
        application::presentation::native::Lane<Content, PreparationContext>,
    ) {
        fixture_with(Content::build)
    }

    fn snapshot(
        binding: &settings::Binding,
        desktop: settings::Desktop,
        revision: u64,
    ) -> settings::Snapshot {
        settings::Snapshot {
            schema: settings::SCHEMA,
            binding: binding.clone(),
            incarnation: "fixture".into(),
            revision: settings::Revision(revision),
            design_revision: settings::Revision(revision),
            source_digest: settings::source_digest(settings::EMBEDDED_DEFAULT_SOURCE),
            effective: settings::resolve(&desktop).expect("desktop resolves"),
            desktop,
        }
    }

    /// Drive the settings lane through at most `steps` progress transitions
    /// (a resource preparation needs two: the jobs replacement, then the
    /// completed preparation). Runs the lane's own worker on a test runtime.
    fn drive(
        lane: &mut application::presentation::native::Lane<Content, PreparationContext>,
        steps: usize,
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            for _ in 0..steps {
                match tokio::time::timeout(std::time::Duration::from_secs(5), lane.drive())
                    .await
                    .expect("bounded fixture progress")
                {
                    application::presentation::native::Progress::Wake
                    | application::presentation::native::Progress::Updated => {}
                    application::presentation::native::Progress::UiClosed => {
                        panic!("the settings lane closed")
                    }
                }
            }
        });
    }

    /// Drain the lane's published events on the UI loop, exactly the
    /// `Delivery::Settings` arm: the content swap, the tint and the
    /// measurement-cache reset.
    fn drain(app: &mut Dopus) -> Vec<settings::domains::ChangePlan> {
        let before = app.look();
        app.settings.drain_with(
            || Some(1),
            view_activation(
                &app.core,
                &app.rows,
                &mut app.column_cache,
                &app.measurements,
                &app.drag,
                &mut app.tint,
                before,
            ),
        )
    }

    /// Drive the fixture's consumer to a confirmed snapshot and activate it
    /// through the REAL production path: connect, offline embedded fallback
    /// (prepared on the lane's worker), subscribe, authority read, prepare,
    /// fence and acknowledge — synchronously. Returns the change plan of a
    /// current successful activation.
    fn activate(
        app: &mut Dopus,
        lane: &mut application::presentation::native::Lane<Content, PreparationContext>,
        desktop: settings::Desktop,
    ) -> Option<settings::domains::ChangePlan> {
        let generation = 1;
        if app
            .settings
            .session()
            .host()
            .consumer()
            .generation()
            .is_none()
        {
            // Bootstrap: connect; while the subscribe runs, the offline
            // fallback prepares the embedded presentation.
            let _ = app
                .settings
                .handle_with(SettingsEvent::Wake, Some(generation), |_| {});
            drive(lane, 2);
            let changed = drain(app);
            assert!(!changed.is_empty(), "the embedded fallback activates");
        }
        // The subscribe completes, then the authority read installs the
        // requested desktop and the worker prepares its content.
        let work = app
            .settings
            .session()
            .host()
            .consumer()
            .current_work()
            .cloned()
            .expect("subscribe work");
        let _ =
            app.settings
                .handle_with(SettingsEvent::Rpc(work, Ok(None)), Some(generation), |_| {});
        drive(lane, 1);
        let work = app
            .settings
            .session()
            .host()
            .consumer()
            .current_work()
            .cloned()
            .expect("read work");
        let binding = app.settings.session().host().consumer().binding().clone();
        let snapshot = snapshot(&binding, desktop, 1);
        let changed = app.settings.handle_with(
            SettingsEvent::Rpc(work, Ok(Some(snapshot))),
            Some(generation),
            |_| {},
        );
        let mut changes: Vec<_> = changed.into_iter().collect();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    let consumer = app.settings.session().host().consumer();
                    if consumer.applied().is_some_and(|applied| {
                        applied.revision == settings::Revision(1)
                            && applied.incarnation == "fixture"
                    }) || consumer.fault().is_some()
                    {
                        break;
                    }
                    assert!(!matches!(
                        lane.drive().await,
                        application::presentation::native::Progress::UiClosed
                    ));
                    changes.extend(drain(app));
                }
            })
            .await
            .expect("confirmed activation or explicit preparation fault");
        });
        changes.into_iter().next()
    }

    fn pin_pending_drop(app: &mut Dopus, source: PathBuf) {
        let root = app.core.pane(PaneId::Left).path.clone();
        app.core.on_event(CoreEvent::ListingArrived {
            pane: PaneId::Left,
            generation: app.core.pane(PaneId::Left).generation,
            path: root.clone(),
            root: true,
            result: Ok(vec![dopus_core::FileEntry {
                name: "source".into(),
                path: source.clone(),
                is_dir: false,
                size: Some(1),
                child_count: None,
                modified: None,
            }]),
        });
        app.refresh_panes();
        let bounds = application::iced::Rectangle {
            x: 300.0,
            y: 0.0,
            width: 300.0,
            height: 300.0,
        };
        let target = app.core.pane(PaneId::Right).path.clone();
        view::drag::lock(&app.drag).pending = Some(view::drag::Gesture {
            pane: PaneId::Left,
            source_root: root,
            source,
            is_dir: false,
            pointer: application::iced::Point::new(400.0, 80.0),
            target: Some(view::drag::Target {
                path: target.clone(),
                root: target,
                bounds,
                highlight: bounds,
            }),
        });
    }

    #[test]
    fn right_click_preserves_a_group_and_retargets_an_unselected_item() {
        let (dir, mut app, mut _lane) = fixture();
        let paths: Vec<_> = ["a", "b", "c"].map(|name| dir.path().join(name)).into();
        let pane = PaneId::Right;
        app.core.on_event(CoreEvent::ListingArrived {
            pane,
            generation: app.core.pane(pane).generation,
            path: dir.path().to_owned(),
            root: true,
            result: Ok(paths
                .iter()
                .map(|path| dopus_core::FileEntry {
                    name: path.file_name().unwrap().to_string_lossy().into_owned(),
                    path: path.clone(),
                    is_dir: false,
                    size: Some(1),
                    child_count: None,
                    modified: None,
                })
                .collect()),
        });
        let _ = app.on_rows(pane, rows::RowsMsg::Select(paths[0].clone()));
        let _ = app.on_rows(
            pane,
            rows::RowsMsg::SelectModified(paths[1].clone(), true, false),
        );
        let right_click = |path| rows::RowsMsg::ContextMenu(path, application::iced::Point::ORIGIN);
        let _ = app.on_rows(pane, right_click(Some(paths[0].clone())));
        assert_eq!(app.core.active(), pane);
        assert_eq!(app.core.selected_paths(pane), paths[..2]);
        assert_eq!(app.core.pane(pane).selected.as_ref(), Some(&paths[0]));
        app.core.open_selection();
        let events = app.core.tick(Instant::now());
        assert!(
            events
                .iter()
                .any(|event| matches!(event, CoreEvent::OpenFile(path) if path == &paths[0]))
        );
        let items = app.context_items();
        assert!(
            items
                .iter()
                .find(|item| item.label() == "Delete")
                .unwrap()
                .is_enabled()
        );
        assert!(
            !items
                .iter()
                .find(|item| item.label() == "Rename")
                .unwrap()
                .is_enabled()
        );
        assert!(verbs::apply_action(actions::filemgr::FILE_RENAME, &mut app.core).is_err());
        let _ = app.on_rows(pane, right_click(Some(paths[2].clone())));
        assert_eq!(app.core.selected_paths(pane), vec![paths[2].clone()]);
        let _ = app.on_rows(pane, right_click(None));
        assert!(app.core.selected_paths(pane).is_empty());
        assert!(
            !app.context_items()
                .iter()
                .find(|item| item.label() == "Delete")
                .unwrap()
                .is_enabled()
        );
    }

    #[test]
    fn context_popup_survives_pane_retarget_and_owns_navigation_keys() {
        use application::iced::{Event, keyboard, mouse};
        let (dir, mut app, mut _lane) = fixture();
        for sidebar in [
            dopus_core::config::Sidebar::Places,
            dopus_core::config::Sidebar::Properties,
        ] {
            if app.core.sidebar(sidebar).open {
                app.core.toggle_sidebar(sidebar);
            }
        }
        let paths: Vec<_> = ["a", "b"].map(|name| dir.path().join(name)).into();
        app.core.on_event(CoreEvent::ListingArrived {
            pane: PaneId::Right,
            generation: app.core.pane(PaneId::Right).generation,
            path: dir.path().to_owned(),
            root: true,
            result: Ok(paths
                .iter()
                .map(|path| dopus_core::FileEntry {
                    name: path.file_name().unwrap().to_string_lossy().into_owned(),
                    path: path.clone(),
                    is_dir: false,
                    size: Some(1),
                    child_count: None,
                    modified: None,
                })
                .collect()),
        });
        app.core.select_path(PaneId::Right, Some(paths[0].clone()));
        app.core
            .select_modified(PaneId::Right, paths[1].clone(), true, false);
        app.core.set_active_pane(PaneId::Left);
        app.refresh_panes();
        let mut renderer = Renderer::new(application::iced::advanced::renderer::Settings {
            default_font: app.look().ui_font,
            default_text_size: application::iced::Pixels(app.look().px),
            ..Default::default()
        });
        let cursor = mouse::Cursor::Available(application::iced::Point::new(450.0, 110.0));
        let size = application::iced::Size::new(600.0, 300.0);
        let mut ui = application::runtime::UserInterface::build(
            app.view(),
            size,
            application::runtime::user_interface::Cache::new(),
            &mut renderer,
        );
        let mut messages = Vec::new();
        crate::test_support::update_ui(
            &mut ui,
            &[Event::Mouse(mouse::Event::ButtonPressed(
                mouse::Button::Right,
            ))],
            cursor,
            &mut renderer,
            &mut messages,
        );
        let cache = ui.into_cache();
        assert!(messages.iter().any(|message| matches!(
            message,
            Msg::PaneRows(PaneId::Right, rows::RowsMsg::ContextMenu(Some(_), _))
        )));
        for message in messages.drain(..) {
            let _ = app.update(message);
        }
        assert_eq!(app.core.active(), PaneId::Right);
        assert_eq!(app.core.selected_paths(PaneId::Right), paths);
        let focused = app.core.pane(PaneId::Right).selected.clone();
        let mut ui =
            application::runtime::UserInterface::build(app.view(), size, cache, &mut renderer);
        let key = |named| {
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(named),
                modified_key: keyboard::Key::Named(named),
                physical_key: keyboard::key::Physical::Unidentified(
                    keyboard::key::NativeCode::Unidentified,
                ),
                location: keyboard::Location::Standard,
                modifiers: keyboard::Modifiers::empty(),
                text: None,
                repeat: false,
            })
        };
        let (_, statuses) = crate::test_support::update_ui(
            &mut ui,
            &[
                key(keyboard::key::Named::ArrowDown),
                key(keyboard::key::Named::Enter),
            ],
            cursor,
            &mut renderer,
            &mut messages,
        );
        assert!(
            statuses
                .iter()
                .all(|status| *status == application::iced::event::Status::Captured)
        );
        assert!(matches!(messages.as_slice(), [Msg::Actions(actions)]
            if actions == &[actions::filemgr::FILE_OPEN]));
        drop(ui);
        assert_eq!(app.core.pane(PaneId::Right).selected, focused);
        assert_eq!(app.core.selected_paths(PaneId::Right), paths);
    }

    #[test]
    fn pending_drop_cancels_when_actions_change_the_pane_layout() {
        use dopus_core::config::Sidebar;
        let command = |action: ActionId| {
            Msg::Bus(Delivery::Command(bus::Command {
                id: 7.into(),
                verb: "dopus.action".into(),
                body: serde_json::json!({"id": action.as_str()}).to_string(),
                caller_key: "mesh:caller@example".into(),
            }))
        };
        let mutations = [
            Msg::Split(0.7),
            Msg::SidebarWidth(Sidebar::Places, 0.24),
            Msg::SidebarWidth(Sidebar::Properties, 0.22),
            Msg::Actions(vec![actions::view::TOGGLE_PLACES]),
            command(actions::view::TOGGLE_PLACES),
            command(actions::view::TOGGLE_PROPERTIES),
        ];
        for mutation in mutations {
            let (dir, mut app, mut _lane) = fixture();
            let mutation = match mutation {
                Msg::Split(_) => Msg::Split(if app.core.config_snapshot().split_ratio >= 0.5 {
                    0.3
                } else {
                    0.7
                }),
                Msg::SidebarWidth(sidebar, _) => Msg::SidebarWidth(
                    sidebar,
                    if app.core.sidebar(sidebar).width >= 0.2 {
                        0.1
                    } else {
                        0.3
                    },
                ),
                message => message,
            };
            let is_command = matches!(&mutation, Msg::Bus(Delivery::Command(_)));
            let (handle, mut responses) = BusHandle::response_sink();
            app.bus = Some(handle);
            let source = dir.path().join("source");
            std::fs::write(&source, b"source").unwrap();
            pin_pending_drop(&mut app, source.clone());
            let _ = app.update(Msg::Noop);
            assert!(
                view::drag::lock(&app.drag).pending.is_some(),
                "unchanged layout preserves choice"
            );
            let before = app.drag_layout();
            let _ = app.update(mutation);
            if is_command {
                assert!(
                    matches!(
                        responses.try_recv(),
                        Ok(bus::Effect::Respond { id: 7, rc: 0, .. })
                    ),
                    "Bus action must run its performer and return success"
                );
            }
            assert_ne!(
                app.drag_layout(),
                before,
                "action must actually change layout/theme"
            );
            assert!(
                view::drag::lock(&app.drag).pending.is_none(),
                "stale target geometry must retire"
            );
            assert!(source.exists());
            assert!(!app.core.availability().operation_running);
        }
    }

    #[test]
    fn prepared_typography_activation_cancels_a_pending_drop_and_keeps_content() {
        let (dir, mut app, mut lane) = fixture();
        let source = dir.path().join("source");
        std::fs::write(&source, b"source").unwrap();
        pin_pending_drop(&mut app, source.clone());
        let _ = app.update(Msg::Noop);
        assert!(view::drag::lock(&app.drag).pending.is_some());
        let before = app.drag_layout();
        // A text-scale change must reshape the view: stale drag geometry
        // retires while the file content it pointed at is untouched.
        let mut desktop = settings::Desktop::default();
        desktop.ui.text_scale = 2.0;
        let changed = activate(&mut app, &mut lane, desktop);
        assert!(changed.is_some(), "typography must be a render change");
        // The layout comparison that retires stale geometry lives in the
        // update path, exactly where a real settings delivery lands.
        let _ = app.update(Msg::Noop);
        assert_ne!(app.drag_layout(), before);
        assert!(view::drag::lock(&app.drag).pending.is_none());
        assert!(source.exists(), "drag cancel preserves file content");
    }

    #[test]
    fn pane_controls_and_split_changes_dismiss_location_editing() {
        let (_dir, mut app, mut _lane) = fixture();
        for msg in [
            Msg::Pane(PaneId::Right, PaneOp::Sort(SortColumn::Size)),
            Msg::Split(0.7),
            Msg::LocationCancel, // outside press, including a stationary divider
        ] {
            let _ = app.begin_edit(PaneId::Left);
            assert!(app.editing.is_some());
            assert!(app.router.lock().unwrap().focus_editable);
            let _ = app.update(msg);
            assert!(app.editing.is_none());
            assert!(!app.router.lock().unwrap().focus_editable);
        }
        assert_eq!(app.core.pane(PaneId::Right).sort, SortColumn::Size);
        assert_eq!(app.core.config_snapshot().split_ratio, 0.7);
    }

    #[test]
    fn toolbar_actions_follow_the_active_pane() {
        let (_dir, mut app, mut _lane) = fixture();
        let left_hidden = app.core.pane(PaneId::Left).show_hidden;
        let right_hidden = app.core.pane(PaneId::Right).show_hidden;
        app.core.set_active_pane(PaneId::Right);
        let _ = app.update(Msg::Actions(vec![actions::filemgr::VIEW_TOGGLE_HIDDEN]));
        assert_eq!(app.core.pane(PaneId::Left).show_hidden, left_hidden);
        assert_eq!(app.core.pane(PaneId::Right).show_hidden, !right_hidden);
        app.core.switch_pane();
        let _ = app.update(Msg::Actions(vec![actions::filemgr::VIEW_TOGGLE_HIDDEN]));
        assert_eq!(app.core.pane(PaneId::Left).show_hidden, !left_hidden);
        assert_eq!(app.core.pane(PaneId::Right).show_hidden, !right_hidden);
    }

    #[test]
    fn switching_panes_dismisses_location_editing_without_submitting_the_draft() {
        let (_dir, mut app, mut _lane) = fixture();
        let original = app.core.pane(PaneId::Left).path.clone();
        let _ = app.begin_edit(PaneId::Left);
        let _ = app.update(Msg::LocationInput("unsubmitted-draft".into()));
        let _ = app.update(Msg::Actions(vec![actions::filemgr::NAV_SWITCH_PANE]));
        assert_eq!(app.core.active(), PaneId::Right);
        assert_eq!(app.core.pane(PaneId::Left).path, original);
        assert!(app.editing.is_none());
        assert!(!app.router.lock().unwrap().focus_editable);
    }

    #[test]
    fn bus_location_focus_preserves_a_same_pane_draft() {
        let (_dir, mut app, mut _lane) = fixture();
        let _ = app.begin_edit(PaneId::Left);
        let draft = "~/unfinished draft".to_owned();
        let _ = app.update(Msg::LocationInput(draft.clone()));
        let _ = app.serve_location_focus(1.into(), PaneId::Left);
        assert_eq!(app.editing, Some((PaneId::Left, draft)));
        assert_eq!(app.core.active(), PaneId::Left);
        assert!(app.router.lock().unwrap().focus_editable);
    }

    #[test]
    fn bus_location_focus_switches_from_another_panes_draft() {
        let (dir, mut app, mut _lane) = fixture();
        let right = dir.path().join("right");
        std::fs::create_dir(&right).unwrap();
        app.core.navigate(PaneId::Right, right);
        let _ = app.begin_edit(PaneId::Left);
        let _ = app.update(Msg::LocationInput("~/unfinished draft".into()));
        let _ = app.serve_location_focus(1.into(), PaneId::Right);
        assert_eq!(
            app.editing,
            Some((PaneId::Right, pane_path_text(&app.core, PaneId::Right)))
        );
        assert_eq!(app.core.active(), PaneId::Right);
        assert!(app.router.lock().unwrap().focus_editable);
    }

    #[test]
    fn bus_location_focus_reaches_the_window_editor_and_reports_availability() {
        let (_dir, mut app, mut _lane) = fixture();
        let command = bus::Command {
            id: 42.into(),
            verb: "dopus.action".into(),
            body: r#"{"id":"location.focus","pane":1}"#.into(),
            caller_key: "mesh:caller@example".into(),
        };
        let info = buildinfo::build_info!();
        let meta = app.server_meta();
        let served = verbs::serve_command(&command, &mut app.core, &meta, &info);
        let [Served::LocationFocus { id, pane }] = served.as_slice() else {
            panic!("windowed location.focus must reach its window performer");
        };
        assert_eq!(id.id, 42);
        assert_eq!(*pane, PaneId::Right);
        let _ = app.serve_location_focus(id.clone(), *pane);
        assert_eq!(
            app.editing,
            Some((PaneId::Right, pane_path_text(&app.core, PaneId::Right)))
        );
        assert!(app.router.lock().unwrap().focus_editable);
        assert_eq!(app.core.active(), PaneId::Right);

        for (headless, available) in [(false, true), (false, false), (true, true)] {
            let mut meta = app.server_meta();
            meta.headless = headless;
            meta.location_focus_available = available;
            let listed = bus::Command {
                verb: "dopus.actions.list".into(),
                body: "{}".into(),
                ..command.clone()
            };
            let served = verbs::serve_command(&listed, &mut app.core, &meta, &info);
            let [Served::Reply { rc: 0, body, .. }] = served.as_slice() else {
                panic!("actions.list must reply");
            };
            let reply: verbs::ActionsReply = serde_json::from_str(body).unwrap();
            let row = reply
                .actions
                .iter()
                .find(|row| row.id == "location.focus")
                .unwrap();
            assert_eq!(row.enabled, !headless && available);
            if !row.enabled {
                let served = verbs::serve_command(&command, &mut app.core, &meta, &info);
                let [Served::Reply { rc: 10, body, .. }] = served.as_slice() else {
                    panic!("unavailable location.focus must refuse");
                };
                let refusal: verbs::Refusal = serde_json::from_str(body).unwrap();
                assert_eq!(refusal.error_code, verbs::code::UNAVAILABLE);
                assert_eq!(
                    refusal.reason.as_deref(),
                    Some(if headless { "headless" } else { "window_busy" })
                );
            }
        }
        app.dialog = Some(dialogs::Dialog::Confirm {
            token: 1,
            message: "Confirm".into(),
        });
        assert!(!app.server_meta().location_focus_available);
        app.dialog = None;
        app.quitting = true;
        assert!(!app.server_meta().location_focus_available);
    }

    #[test]
    fn prepared_settings_activation_retains_model_dialogues_selection_and_operation() {
        let (dir, mut app, mut lane) = fixture();
        // Real app-owned state: a listing, a selection, a queued dialog and
        // a running file operation — exactly what a paint/text/layout change
        // must not touch.
        let paths: Vec<_> = ["a", "b"].map(|name| dir.path().join(name)).into();
        app.core.on_event(CoreEvent::ListingArrived {
            pane: PaneId::Left,
            generation: app.core.pane(PaneId::Left).generation,
            path: dir.path().to_owned(),
            root: true,
            result: Ok(paths
                .iter()
                .map(|path| dopus_core::FileEntry {
                    name: path.file_name().unwrap().to_string_lossy().into_owned(),
                    path: path.clone(),
                    is_dir: false,
                    size: Some(1),
                    child_count: None,
                    modified: None,
                })
                .collect()),
        });
        app.core.select_path(PaneId::Left, Some(paths[0].clone()));
        app.dialog = Some(dialogs::Dialog::Confirm {
            token: 7,
            message: "Confirm".into(),
        });
        app.core.set_active_pane(PaneId::Left);
        app.core.copy_selection_to_other_pane();
        assert!(app.core.availability().operation_running);
        // A denser crimson/dark generation activates over the bootstrap.
        let mut desktop = settings::Desktop::default();
        desktop.appearance.scheme = "crimson".into();
        desktop.appearance.mode = "dark".into();
        desktop.ui.density = 1.5;
        let changed = activate(&mut app, &mut lane, desktop);
        assert!(
            changed.is_some(),
            "a new generation must be a render change"
        );
        assert_eq!(app.content().theme.scheme, Scheme::Crimson);
        assert_eq!(app.content().theme.mode, Mode::Dark);
        assert!(app.content().theme.density > 1.0);
        // The app-owned model is untouched.
        assert_eq!(app.core.pane(PaneId::Left).path, dir.path());
        assert_eq!(
            app.core.selected_paths(PaneId::Left),
            vec![paths[0].clone()]
        );
        assert!(matches!(
            app.dialog,
            Some(dialogs::Dialog::Confirm { token: 7, .. })
        ));
        assert!(app.core.availability().operation_running);
        assert_eq!(app.editing, None);
        // Every required icon handle exists BEFORE the view draws the new
        // theme — no partial fills, nothing to wait for.
        for tint in [
            icons::tint_key(app.content().theme.tokens.palette.text),
            icons::tint_key(app.content().theme.tokens.palette.muted_text),
            icons::tint_key(app.content().theme.tokens.palette.selection_text),
        ] {
            for icon in icons::ALL {
                assert!(
                    app.content()
                        .icons
                        .get(icon, &tint, icons::RASTER_PX)
                        .is_some()
                        || app.content().icons.glyph(icon).is_some(),
                    "{icon:?} at {tint} must exist before activation"
                );
            }
        }
    }

    #[test]
    fn activation_fault_retains_last_good_content() {
        // The builder prepares the embedded fallback and one authority
        // generation, then faults on the third build — exactly the shape of
        // a resource the worker cannot produce for a newer generation.
        let builds = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let build = {
            let builds = std::sync::Arc::clone(&builds);
            move |look: &appearance::settings::Prepared, snapshot: &settings::Snapshot| {
                let n = builds.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if n >= 2 {
                    Err(Diagnostic::new(
                        "unsupported_content",
                        "icons.archive",
                        "raster unavailable",
                    ))
                } else {
                    Content::build(look, snapshot)
                }
            }
        };
        let (_dir, mut app, mut lane) = fixture_with(build);
        // A crimson authority generation activates as the baseline.
        let mut crimson = settings::Desktop::default();
        crimson.appearance.scheme = "crimson".into();
        let changed = activate(&mut app, &mut lane, crimson);
        assert!(changed.is_some());
        assert_eq!(app.content().theme.scheme, Scheme::Crimson);
        // A second, newer generation stages, then its preparation FAULTS:
        // the applied content must stay the baseline (LastGood), never the
        // faulted stage.
        let generation = 1;
        let _ = app
            .settings
            .handle_with(SettingsEvent::Refresh, Some(generation), |_| {});
        drive(&mut lane, 1);
        let work = app
            .settings
            .session()
            .host()
            .consumer()
            .current_work()
            .cloned()
            .expect("refresh read work");
        let binding = app.settings.session().host().consumer().binding().clone();
        let mut stone = settings::Desktop::default();
        stone.appearance.scheme = "stone".into();
        let snapshot = snapshot(&binding, stone, 2);
        let _ = app.settings.handle_with(
            SettingsEvent::Rpc(work, Ok(Some(snapshot))),
            Some(generation),
            |_| {},
        );
        drive(&mut lane, 2);
        let changed = drain(&mut app);
        assert!(changed.is_empty(), "a fault never activates");
        assert_eq!(
            app.content().theme.scheme,
            Scheme::Crimson,
            "LastGood retained"
        );
        let evidence = app.settings.session().host().consumer().evidence();
        assert!(evidence.fault.is_some());
        assert_eq!(
            evidence.kind,
            Some(settings::fallback::PresentationKind::LastGood)
        );
    }

    #[test]
    fn theme_request_forwards_a_fenced_validated_apply_with_the_current_read() {
        let (_dir, mut app, mut lane) = fixture();
        let _ = activate(&mut app, &mut lane, settings::Desktop::default());
        assert_eq!(
            app.settings
                .session()
                .host()
                .consumer()
                .applied()
                .unwrap()
                .revision,
            settings::Revision(1)
        );
        let (handle, mut responses) = BusHandle::response_sink();
        app.bus = Some(handle);
        let _ = app.theme_request(Some(42.into()), Some("crimson"), Some("dark"));
        let Ok(bus::Effect::ThemeApply { request, .. }) = responses.try_recv() else {
            panic!("theme.request must forward a fenced apply")
        };
        assert_eq!(
            request.reply_id.as_ref().map(|request| request.id),
            Some(42)
        );
        assert_eq!(
            request.changes["appearance.scheme"],
            serde_json::json!("crimson")
        );
        assert_eq!(
            request.changes["appearance.mode"],
            serde_json::json!("dark")
        );
        assert_eq!(request.expected_incarnation, "fixture");
        assert_eq!(request.expected_revision, settings::Revision(1));
        assert_eq!(request.binding.instance, "fixture");
        assert_eq!(request.binding.profile, "default");
        assert!(!request.operation_id.is_empty());
        // The request is a real mutation of the shared authority: nothing
        // app-local changed until the authority answers.
        assert_eq!(app.content().theme.scheme, Scheme::Ocean);
    }

    #[test]
    fn theme_request_refuses_without_a_confirmed_read() {
        let (_dir, mut app, mut _lane) = fixture();
        let (handle, mut responses) = BusHandle::response_sink();
        app.bus = Some(handle);
        let _ = app.theme_request(Some(7.into()), Some("crimson"), None);
        let Ok(bus::Effect::Respond {
            id: 7,
            rc: 10,
            body,
        }) = responses.try_recv()
        else {
            panic!("an unconfirmed settings read must refuse, never fake success")
        };
        let refusal: verbs::Refusal = serde_json::from_str(&body).unwrap();
        assert_eq!(refusal.error_code, verbs::code::UNAVAILABLE);
        assert_eq!(refusal.reason.as_deref(), Some("unsupported"));
    }

    #[test]
    fn state_and_describe_carry_reconciled_settings_evidence() {
        let (_dir, mut app, mut lane) = fixture();
        let _ = activate(&mut app, &mut lane, settings::Desktop::default());
        assert_eq!(
            app.settings
                .session()
                .host()
                .consumer()
                .applied()
                .unwrap()
                .revision,
            settings::Revision(1)
        );
        let (handle, mut responses) = BusHandle::response_sink();
        app.bus = Some(handle);
        app.editing = Some((PaneId::Left, "unfinished native path".into()));
        for verb in ["dopus.state", "app.describe"] {
            let command = bus::Command {
                id: 9.into(),
                verb: verb.into(),
                body: "{}".into(),
                caller_key: "mesh:caller@example".into(),
            };
            let _ = app.serve(&command);
            let Ok(bus::Effect::Respond { id: 9, rc: 0, body }) = responses.try_recv() else {
                panic!("{verb} must reply")
            };
            let value: serde_json::Value = serde_json::from_str(&body).unwrap();
            let evidence = &value["settings"];
            assert_eq!(evidence["context"], "app:dopus");
            // The sink has no live generation: the reconcile demotes the
            // installed data to LastGood and keeps it — the canonical
            // offline evidence, reported instead of a stale confirmed claim.
            assert_eq!(evidence["kind"], "last_good");
            assert_eq!(evidence["current"]["incarnation"], "fixture");
            assert!(value.get("settings_cache").is_some());
            if verb == "dopus.state" {
                assert_eq!(value["ui"]["location_draft"]["pane"], "left");
                assert_eq!(value["ui"]["location_draft"]["text"], "unfinished native path");
                assert_eq!(app.editing.as_ref().unwrap().1, "unfinished native path");
            }
            if verb == "app.describe" {
                application::describe::validate(&value).unwrap();
                assert_eq!(value["pid"], std::process::id());
                assert_eq!(value["app_id"], APP_ID);
                assert_eq!(value["service"], "dopus");
                assert!(value["resources"].is_object());
                assert!(value["preparation"].is_object());
            }
        }
    }
}

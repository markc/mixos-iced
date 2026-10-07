// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native iced front-end. Workers feed bounded operations back by completion;
//! no redraw heartbeat, shell capture program or portal is used.
use crate::{
    bus::{self, BusHandle, Delivery},
    capture::{self, Mode, Request, Target, Window},
    document::{Crop, Document, Kind, Point, Shape},
    menu,
    strings::label,
    verbs::{self, Operation},
    viewport::Viewport,
};
use application::presentation::native::Ui;
use application::{Element, Renderer, iced};
use iced::futures::{StreamExt, channel::mpsc::Receiver};
use iced::{
    Subscription, Task, mouse,
    widget::{self, button, canvas, column, container, row, slider, text_input},
    window,
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};
use toolkit::{Theme, requester};
pub const APP_ID: &str = "dev.mixos.cap";
/// A failed cancellation cleanup retries at most this many real Connected
/// events before the target is abandoned with a visible status.
const MAX_CLEANUP_ATTEMPTS: u32 = 3;

impl std::fmt::Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&label(self.key()))
    }
}
impl std::fmt::Display for Window {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.title.is_empty() {
            f.write_str(&self.app_id)
        } else {
            f.write_str(&self.title)
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Select,
    Crop,
    Draw(Kind),
}
impl Tool {
    fn key(self) -> &'static str {
        match self {
            Self::Select => "select",
            Self::Crop => "crop",
            Self::Draw(kind) => kind.key(),
        }
    }
}
#[derive(Debug, Clone)]
pub enum Gesture {
    Add(Shape),
    Crop(Crop),
    Select(Option<u64>),
    Move(u64, f32, f32),
    Pan(f32, f32),
}
#[derive(Debug, Clone)]
enum Pending {
    Capture,
    Open,
    Quit,
}
#[derive(Debug, Clone, Copy)]
enum Dialog {
    Properties,
    Shortcuts,
    About,
}
impl Dialog {
    fn key(self) -> &'static str {
        match self {
            Self::Properties => "properties",
            Self::Shortcuts => "shortcuts",
            Self::About => "about",
        }
    }
}
#[derive(Debug, Clone)]
pub enum Message {
    Menu(menu::Action),
    OpenMenu(usize),
    Bus(Delivery),
    Window(window::Id, window::Event),
    Refresh,
    Refreshed(Result<(Value, Value), String>),
    Shown(crate::bus::Request, Result<Value, String>),
    Mode(Mode),
    Output(String),
    Choose(Window),
    Delay(String),
    Pointer(bool),
    Take,
    Cancel,
    /// The capture attempt's generation fences stale completions: a late
    /// result never updates a newer capture.
    Captured(u64, Result<capture::Captured, capture::CaptureError>),
    CleanupRetried(Result<capture::CleanupOutcome, String>),
    Preview(u64, Result<image::RgbaImage, String>),
    Tool(Tool),
    Gesture(Gesture),
    Colour(toolkit::color_picker::Hsv),
    Width(f32),
    Text(String),
    TextSize(String),
    Zoom(f32),
    Fit,
    Undo,
    Redo,
    Delete,
    Uncrop,
    Save,
    Open,
    Request(requester::Event),
    Opened(Result<(PathBuf, Document), String>),
    Saved(Result<PathBuf, String>),
    Copy,
    ClipboardImage(Result<image::RgbaImage, String>),
    Copied(Result<(), iced::advanced::clipboard::Error>),
    Quit,
    Discard,
    Keep,
    Key(iced::keyboard::Key, iced::keyboard::Modifiers),
    Noop,
}
impl From<requester::Event> for Message {
    fn from(value: requester::Event) -> Self {
        Self::Request(value)
    }
}
static DELIVERIES: OnceLock<Mutex<Option<Receiver<Delivery>>>> = OnceLock::new();
fn deliveries() -> impl iced::futures::Stream<Item = Delivery> {
    let rx = DELIVERIES
        .get()
        .expect("Bus stream installed")
        .lock()
        .unwrap()
        .take()
        .expect("one Bus subscription");
    iced::futures::stream::unfold(rx, |mut rx| async move {
        rx.next().await.map(|message| (message, rx))
    })
}
pub struct App {
    bus: BusHandle,
    comp: String,
    bootstrap: appearance::settings::Prepared,
    settings_ui: Ui<()>,
    directory: PathBuf,
    request: Request,
    delay: String,
    outputs: Vec<String>,
    windows: Vec<Window>,
    selected_window: Option<Window>,
    own: Option<Target>,
    window: Option<window::Id>,
    document: Option<Document>,
    preview: Option<iced::advanced::image::Handle>,
    revision: u64,
    rendering: bool,
    repaint: bool,
    path: Option<PathBuf>,
    metadata: Value,
    tool: Tool,
    selected: Option<u64>,
    colour: iced::Color,
    width: f32,
    annotation_text: String,
    text_size: String,
    zoom: f32,
    pan: Point,
    status: String,
    busy: bool,
    cancel: Option<tokio::sync::watch::Sender<bool>>,
    pending_reply: Option<crate::bus::Request>,
    picker: Option<requester::Requester>,
    picker_strings: requester::Strings,
    pending: Option<Pending>,
    confirm: bool,
    dialog: Option<Dialog>,
    refused: bool,
    /// A user interacted with the window; fences a late successful handoff
    /// from closing it under the user.
    touched: bool,
    /// The capture attempt generation: increments per attempt, never reused.
    capture_generation: u64,
    /// A failed cancellation cleanup, retained for one explicit bounded
    /// retry on a later real Connected event — never rebuilt for another
    /// compositor.
    failed_cleanup: Option<capture::Cleanup>,
    cleanup_attempts: u32,
    launched: std::time::Instant,
}
impl App {
    fn new(
        bus: BusHandle,
        bootstrap: appearance::settings::Prepared,
        settings_ui: Ui<()>,
        comp: String,
        directory: PathBuf,
    ) -> App {
        let colour = bootstrap.tokens().palette.destructive;
        App {
            bus,
            comp,
            bootstrap,
            settings_ui,
            directory,
            request: Request::default(),
            delay: "0".into(),
            outputs: vec![],
            windows: vec![],
            selected_window: None,
            own: None,
            window: None,
            document: None,
            preview: None,
            revision: 0,
            rendering: false,
            repaint: false,
            path: None,
            metadata: Value::Null,
            tool: Tool::Draw(Kind::Arrow),
            selected: None,
            colour,
            width: 4.0,
            annotation_text: String::new(),
            text_size: "24".into(),
            zoom: 1.0,
            pan: Point { x: 0.0, y: 0.0 },
            status: label("ready"),
            busy: false,
            cancel: None,
            pending_reply: None,
            picker: None,
            picker_strings: requester::Strings {
                placeholder: label("name"),
                show_hidden: label("hidden"),
                hide_hidden: label("hidden"),
                truncated: label("files"),
                recent: label("recent"),
            },
            pending: None,
            confirm: false,
            dialog: None,
            refused: false,
            touched: false,
            capture_generation: 0,
            failed_cleanup: None,
            cleanup_attempts: 0,
            launched: std::time::Instant::now(),
        }
    }
}
pub fn run(service: &str, url: &str, comp: &str, path: Option<PathBuf>) -> Result<(), String> {
    let directory = capture::media_directory()?;
    let handoff = path
        .as_ref()
        .map(|p| vec![p.to_string_lossy().into_owned()]);
    let (bus, mut settings_ui, bootstrap, rx) = bus::start(service, url, handoff)?;
    let result = (|| {
        DELIVERIES
            .set(Mutex::new(Some(rx)))
            .map_err(|_| "Cap already started in this process")?;
        settings_ui.reconcile(bus.settings_generation());
        let mut app = App::new(bus.clone(), bootstrap, settings_ui, comp.into(), directory);
        let mut startup = vec![app.refresh()];
        if let Some(path) = path {
            startup.push(app.open_path(path));
        }
        let font = app
            .bootstrap
            .typography()
            .get("ui")
            .expect("UI typography")
            .font;
        application::start(
            (app, Task::batch(startup)),
            App::update,
            App::view,
            application::Window::new(APP_ID, iced::Size::new(1040.0, 720.0), font)
                .minimum(iced::Size::new(760.0, 450.0))
                .defer_close(),
        )
        .title(|_: &App| label("title"))
        .theme(|app: &App| app.look().theme())
        .subscription(App::subscription)
        .run()
        .map_err(|e| e.to_string())
    })();
    bus.quit();
    bus.wait_done(Duration::from_secs(3));
    result
}
async fn work<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    crate::worker::run(f).await
}
impl App {
    fn look(&self) -> &appearance::settings::Prepared {
        self.settings_ui
            .session()
            .host()
            .presentation()
            .map_or(&self.bootstrap, |presentation| presentation.appearance())
    }
    fn typography(&self, role: &str) -> toolkit::typography::TextStyle {
        self.look()
            .typography()
            .get(role)
            .expect("prepared typography role")
    }
    fn text<'a>(
        &self,
        content: impl iced::advanced::text::IntoFragment<'a>,
    ) -> application::widget::Text<'a, Theme> {
        self.typography("ui").text(content)
    }
    /// Persistent provenance: the applied settings source and the sampled
    /// connection state, never a queued boolean.
    fn persistent_status(&self) -> String {
        use settings::fallback::PresentationKind;
        let kind = match self.settings_ui.session().host().consumer().evidence().kind {
            Some(PresentationKind::Current) => "settings-current",
            Some(PresentationKind::Cached) => "settings-cached",
            Some(PresentationKind::Embedded) => "settings-embedded",
            Some(PresentationKind::Retained) => "settings-retained",
            Some(PresentationKind::LastGood) => "settings-last-good",
            None => "settings-bootstrap",
        };
        let connection = if self.bus.connected() {
            "bus-connected"
        } else if self.refused {
            "bus-refused"
        } else if self.bus.ever_registered() {
            "bus-disconnected"
        } else {
            "bus-connecting"
        };
        format!("{} · {}", label(kind), label(connection))
    }
    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            Subscription::run(deliveries).map(Message::Bus),
            iced::event::listen_with(|event, status, id| match event {
                iced::Event::Window(event) => Some(Message::Window(id, event)),
                iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                    key, modifiers, ..
                }) if status == iced::event::Status::Ignored => Some(Message::Key(key, modifiers)),
                _ => None,
            }),
        ])
    }
    fn error(&mut self, error: impl std::fmt::Display) {
        self.status = format!("{}: {error}", label("error"));
    }
    fn refresh(&self) -> Task<Message> {
        let bus = self.bus.clone();
        let comp = self.comp.clone();
        Task::perform(
            async move {
                let windows = capture::list(&bus, &comp).await?;
                let outputs = bus
                    .call(
                        &comp,
                        "comp.props.get",
                        json!({"path":"outputs"}),
                        Duration::from_secs(5),
                    )
                    .await?;
                Ok((windows, outputs))
            },
            Message::Refreshed,
        )
    }
    fn preview(&mut self) -> Task<Message> {
        self.revision += 1;
        let revision = self.revision;
        if self.rendering {
            self.repaint = true;
            return Task::none();
        }
        let Some(doc) = self.document.as_ref().map(Document::raster_snapshot) else {
            return Task::none();
        };
        self.rendering = true;
        self.repaint = false;
        Task::perform(work(move || doc.render()), move |result| {
            Message::Preview(revision, result)
        })
    }
    /// Drain queued settings events and reconcile the live generation, on
    /// every Bus delivery before dispatch. Returns how many activations
    /// landed, for the shared receipt.
    fn sync_settings(&mut self) -> usize {
        let changes = self
            .settings_ui
            .drain_with(|| self.bus.settings_generation(), |_| {});
        self.settings_ui.reconcile(self.bus.settings_generation());
        changes.len()
    }
    fn info(&mut self) -> Value {
        self.settings_ui.reconcile(self.bus.settings_generation());
        let mut info = json!({"schema":"cap.v1","busy":self.busy,"status":self.status,"connected":self.bus.connected(),"document":self.document.as_ref().map(Document::info),"path":self.path,"capture":self.metadata,"mode":self.request.mode,"pid":std::process::id(),"version":env!("CARGO_PKG_VERSION"),"ui":{"menu_bar":true,"tool":self.tool.key(),"zoom":self.zoom,"dialog":self.dialog.map(Dialog::key)}});
        info["settings"] = json!(self.settings_ui.session().host().consumer().evidence());
        info["settings_cache"] = json!(self.settings_ui.session().cache_evidence());
        info
    }
    fn describe(&mut self) -> Value {
        self.settings_ui.reconcile(self.bus.settings_generation());
        let mut describe = json!({
            "schema":"cap.v1",
            "app_id":APP_ID,
            "version":env!("CARGO_PKG_VERSION"),
            "transport":"native",
            "verbs":verbs::VERBS
        });
        describe["settings"] = json!(self.settings_ui.session().host().consumer().evidence());
        describe["settings_cache"] = json!(self.settings_ui.session().cache_evidence());
        describe
    }
    fn modal(&self) -> bool {
        self.confirm || self.picker.is_some() || self.dialog.is_some()
    }
    fn menu_context(&self) -> menu::Context<'_> {
        menu::Context {
            document: self.document.as_ref(),
            selected: self.selected,
            request: &self.request,
            outputs: &self.outputs,
            windows: &self.windows,
            window: self.selected_window.as_ref(),
            tool: self.tool,
            busy: self.busy,
            capturing: self.cancel.is_some(),
            modal: self.modal(),
        }
    }
    fn reply(&self, id: impl Into<crate::bus::Request>, result: Result<Value, String>) {
        match result {
            Ok(v) => self.bus.respond(id, 0, v.to_string()),
            Err(e) => self.bus.respond(id, 10, json!({"error":e}).to_string()),
        }
    }
    fn request_pending(&mut self, action: Pending) -> Task<Message> {
        if self.busy {
            if matches!(action, Pending::Quit) {
                self.pending = Some(action);
                if let Some(cancel) = &self.cancel {
                    let _ = cancel.send(true);
                }
            } else {
                self.error(label("busy"));
            }
            return Task::none();
        }
        if self.document.as_ref().is_some_and(Document::dirty) {
            self.pending = Some(action);
            self.confirm = true;
            return Task::none();
        }
        self.perform_pending(action)
    }
    fn perform_pending(&mut self, action: Pending) -> Task<Message> {
        match action {
            Pending::Capture => self.take(),
            Pending::Open => {
                self.file_picker(requester::Mode::Open);
                Task::none()
            }
            Pending::Quit => {
                self.bus.quit();
                self.bus.wait_done(Duration::from_secs(3));
                iced::exit()
            }
        }
    }
    fn take(&mut self) -> Task<Message> {
        if self.busy {
            self.error(label("busy"));
            return Task::none();
        }
        let bus = self.bus.clone();
        self.request.delay = match self.delay.parse::<u32>() {
            Ok(n) if n <= 10 => n,
            _ => {
                self.error("delay must be 0..10 seconds");
                return Task::none();
            }
        };
        if self.pending_reply.is_none() {
            self.request.window = if self.request.mode == Mode::Window {
                self.selected_window.as_ref().map(|w| w.target.clone())
            } else {
                None
            };
        }
        if let Err(error) = self.request.validate() {
            self.error(error);
            return Task::none();
        }
        let generation = self
            .capture_generation
            .checked_add(1)
            .expect("capture generations exhausted");
        self.capture_generation = generation;
        let (tx, rx) = tokio::sync::watch::channel(false);
        self.cancel = Some(tx);
        self.busy = true;
        self.status = label("capturing");
        let comp = self.comp.clone();
        let request = self.request.clone();
        let directory = self.directory.clone();
        Task::perform(
            async move {
                let own = capture::own_window(&bus, &comp)
                    .await
                    .map_err(capture::CaptureError::plain)?;
                capture::take(bus, comp, request, Some(own), directory, generation, rx).await
            },
            move |result| Message::Captured(generation, result),
        )
    }
    fn file_picker(&mut self, mode: requester::Mode) {
        let directory = self
            .path
            .as_ref()
            .and_then(|p| p.parent())
            .map(PathBuf::from)
            .unwrap_or_else(|| self.directory.clone());
        self.picker = Some(
            requester::Requester::new(mode, directory, vec![], Arc::new(requester::StdFs))
                .with_name(&format!("cap-{}.png", uuid::Uuid::now_v7())),
        );
    }
    fn open_path(&mut self, path: PathBuf) -> Task<Message> {
        self.busy = true;
        Task::perform(
            work(move || Document::open(&path).map(|doc| (path, doc))),
            Message::Opened,
        )
    }
    fn save_path(&mut self, path: PathBuf) -> Task<Message> {
        let Some(doc) = self.document.clone() else {
            return Task::none();
        };
        self.busy = true;
        self.status = label("working");
        Task::perform(
            work(move || {
                doc.export(&path)?;
                Ok(path)
            }),
            Message::Saved,
        )
    }
    /// One explicit bounded retry of a failed cancellation cleanup, submitted
    /// only while idle; the exact retained identity is never rebuilt for a
    /// replacement compositor.
    fn retry_cleanup(&mut self) -> Task<Message> {
        if self.busy {
            return Task::none();
        }
        let Some(cleanup) = self.failed_cleanup.clone() else {
            return Task::none();
        };
        self.busy = true;
        let bus = self.bus.clone();
        let comp = self.comp.clone();
        Task::perform(
            async move { capture::retry_cleanup(&bus, &comp, &cleanup).await },
            Message::CleanupRetried,
        )
    }
    fn update(&mut self, message: Message) -> Task<Message> {
        if matches!(
            &message,
            Message::Menu(_)
                | Message::OpenMenu(_)
                | Message::Mode(_)
                | Message::Output(_)
                | Message::Choose(_)
                | Message::Delay(_)
                | Message::Pointer(_)
                | Message::Take
                | Message::Cancel
                | Message::Tool(_)
                | Message::Gesture(_)
                | Message::Colour(_)
                | Message::Width(_)
                | Message::Text(_)
                | Message::TextSize(_)
                | Message::Zoom(_)
                | Message::Fit
                | Message::Undo
                | Message::Redo
                | Message::Delete
                | Message::Uncrop
                | Message::Save
                | Message::Open
                | Message::Refresh
                | Message::Request(_)
                | Message::Copy
                | Message::Quit
                | Message::Discard
                | Message::Keep
                | Message::Key(..)
        ) {
            self.touched = true;
        }
        match message {
            Message::OpenMenu(index) => {
                if self.modal() {
                    return Task::none();
                }
                iced::advanced::widget::operate(toolkit::menu::open_operation(menu::BAR_ID, index))
                    .discard()
                    .chain(Task::done(Message::Noop))
            }
            Message::Menu(action) => {
                if !menu::enabled(&action, &self.menu_context()) {
                    return Task::none();
                }
                let message = match action {
                    menu::Action::Open => Message::Open,
                    menu::Action::Save => Message::Save,
                    menu::Action::Copy => Message::Copy,
                    menu::Action::Quit => Message::Quit,
                    menu::Action::Take => Message::Take,
                    menu::Action::Cancel => Message::Cancel,
                    menu::Action::Refresh => Message::Refresh,
                    menu::Action::Mode(mode) => Message::Mode(mode),
                    menu::Action::Output(output) => Message::Output(output),
                    menu::Action::Window(window) => Message::Choose(window),
                    menu::Action::Delay(delay) => Message::Delay(delay.to_string()),
                    menu::Action::Pointer(pointer) => Message::Pointer(pointer),
                    menu::Action::Tool(tool) => {
                        if tool == Tool::Draw(Kind::Text) {
                            self.dialog = Some(Dialog::Properties);
                        }
                        Message::Tool(tool)
                    }
                    menu::Action::Undo => Message::Undo,
                    menu::Action::Redo => Message::Redo,
                    menu::Action::Delete => Message::Delete,
                    menu::Action::Uncrop => Message::Uncrop,
                    menu::Action::ZoomIn => Message::Zoom(self.zoom * 1.25),
                    menu::Action::ZoomOut => Message::Zoom(self.zoom / 1.25),
                    menu::Action::Fit => Message::Fit,
                    menu::Action::Properties | menu::Action::Shortcuts | menu::Action::About => {
                        self.dialog = Some(match action {
                            menu::Action::Properties => Dialog::Properties,
                            menu::Action::Shortcuts => Dialog::Shortcuts,
                            _ => Dialog::About,
                        });
                        return Task::none();
                    }
                };
                self.update(message)
            }
            Message::Refresh => self.refresh(),
            Message::Shown(id, result) => {
                self.busy = false;
                if let Err(error) = &result {
                    self.error(error);
                }
                self.reply(id, result);
                if self.modal() {
                    return Task::none();
                }
                self.pending
                    .take()
                    .map(|action| self.request_pending(action))
                    .unwrap_or_else(Task::none)
            }
            Message::Refreshed(result) => {
                match result {
                    Ok((rows, outputs)) => {
                        self.windows = capture::windows(&rows);
                        self.own = rows["windows"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .find(|w| {
                                w["app_id"] == APP_ID
                                    && w["pid"].as_u64() == Some(u64::from(std::process::id()))
                            })
                            .and_then(|w| {
                                Some(Target {
                                    id: w["id"].as_u64()?,
                                    generation: w["generation"].as_u64()?,
                                })
                            });
                        self.windows
                            .retain(|w| self.own.as_ref() != Some(&w.target));
                        self.selected_window =
                            capture::selected_window(&self.windows, self.selected_window.as_ref());
                        let outputs = outputs.get("value").unwrap_or(&outputs);
                        self.outputs = outputs
                            .as_object()
                            .into_iter()
                            .flat_map(|map| map.values())
                            .filter_map(|row| row["name"].as_str().map(str::to_owned))
                            .collect();
                        if self
                            .request
                            .output
                            .as_ref()
                            .is_none_or(|old| !self.outputs.contains(old))
                        {
                            self.request.output = self.outputs.first().cloned();
                        }
                    }
                    Err(error) => self.error(error),
                }
                Task::none()
            }
            Message::Window(id, window::Event::Opened { .. }) => {
                self.window = Some(id);
                self.refresh()
            }
            Message::Window(_, window::Event::CloseRequested) => {
                self.request_pending(Pending::Quit)
            }
            Message::Window(_, _) => Task::none(),
            Message::Mode(mode) => {
                if !self.busy {
                    self.request.mode = mode;
                }
                Task::none()
            }
            Message::Output(output) => {
                self.request.output = Some(output);
                Task::none()
            }
            Message::Choose(window) => {
                self.selected_window = Some(window);
                Task::none()
            }
            Message::Delay(value) => {
                self.delay = value.clone();
                if let Ok(delay) = value.parse::<u32>()
                    && delay <= 10
                {
                    self.request.delay = delay;
                }
                Task::none()
            }
            Message::Pointer(value) => {
                self.request.cursor = value;
                Task::none()
            }
            Message::Take => self.request_pending(Pending::Capture),
            Message::Cancel => {
                if let Some(cancel) = &self.cancel {
                    let _ = cancel.send(true);
                }
                Task::none()
            }
            Message::Captured(generation, result) => {
                if generation != self.capture_generation {
                    // A late result from an older attempt never updates a
                    // newer capture.
                    return Task::none();
                }
                self.busy = false;
                self.cancel = None;
                match result {
                    Ok(capture) => {
                        self.preview = None;
                        self.document = Some(capture.document);
                        self.path = Some(capture.path);
                        self.metadata = capture.metadata;
                        self.zoom = 1.0;
                        self.pan = Point { x: 0.0, y: 0.0 };
                        self.selected = None;
                        self.status = label("capture-complete");
                    }
                    Err(error) => {
                        if error.cleanup.is_some() {
                            // A failed cancellation cleanup supersedes any
                            // earlier retained target; a plain failure keeps
                            // the outstanding obligation.
                            self.failed_cleanup = error.cleanup;
                            self.status = error.message;
                        } else {
                            self.error(error.message);
                        }
                    }
                }
                if let Some(id) = self.pending_reply.take() {
                    let value =
                        if self.document.is_some() && self.status == label("capture-complete") {
                            Ok(self.info())
                        } else {
                            Err(self.status.clone())
                        };
                    self.reply(id, value);
                }
                let task = self.preview();
                if let Some(action) = self.pending.take() {
                    return Task::batch([task, self.request_pending(action)]);
                }
                Task::batch([task, self.refresh()])
            }
            Message::CleanupRetried(result) => {
                self.busy = false;
                match result {
                    Ok(capture::CleanupOutcome::Restored) => {
                        self.failed_cleanup = None;
                        self.cleanup_attempts = 0;
                        self.status = label("ready");
                    }
                    Ok(capture::CleanupOutcome::StaleInstance) => {
                        // The compositor restarted: the old identity is
                        // retired by the owner, never retargeted here.
                        self.failed_cleanup = None;
                        self.cleanup_attempts = 0;
                        self.status = label("ready");
                    }
                    Err(error) => {
                        self.cleanup_attempts += 1;
                        self.status = format!("{}: cleanup failed: {error}", label("error"));
                        if self.cleanup_attempts >= MAX_CLEANUP_ATTEMPTS {
                            self.failed_cleanup = None;
                            self.status = format!("{}: cleanup abandoned: {error}", label("error"));
                        }
                    }
                }
                Task::none()
            }
            Message::Preview(revision, result) => {
                self.rendering = false;
                if revision == self.revision {
                    match result {
                        Ok(image) => {
                            self.preview = Some(iced::advanced::image::Handle::from_rgba(
                                image.width(),
                                image.height(),
                                image.into_raw(),
                            ))
                        }
                        Err(error) => self.error(error),
                    }
                }
                if self.repaint {
                    self.preview()
                } else {
                    Task::none()
                }
            }
            Message::Tool(tool) => {
                self.tool = tool;
                Task::none()
            }
            Message::Colour(colour) => {
                self.colour = colour.into();
                Task::none()
            }
            Message::Width(width) => {
                self.width = width;
                Task::none()
            }
            Message::Text(value) => {
                if value.len() <= 4096 {
                    self.annotation_text = value;
                }
                Task::none()
            }
            Message::TextSize(value) => {
                self.text_size = value;
                Task::none()
            }
            Message::Zoom(zoom) => {
                self.zoom = zoom.clamp(0.1, 8.0);
                Task::none()
            }
            Message::Fit => {
                self.zoom = 1.0;
                self.pan = Point { x: 0.0, y: 0.0 };
                Task::none()
            }
            Message::Gesture(gesture) => {
                if self.busy || self.modal() {
                    return Task::none();
                };
                match gesture {
                    Gesture::Select(id) => {
                        self.selected = id;
                        return Task::none();
                    }
                    Gesture::Pan(dx, dy) => {
                        self.pan.x += dx;
                        self.pan.y += dy;
                        return Task::none();
                    }
                    _ => {}
                }
                if let Some(doc) = &mut self.document {
                    let result = match gesture {
                        Gesture::Add(mut shape) => {
                            if shape.kind == Kind::Text {
                                shape.text = Some(self.annotation_text.clone());
                            }
                            if matches!(shape.kind, Kind::Text | Kind::Number) {
                                shape.size = self.text_size.parse().ok();
                            }
                            if shape.kind == Kind::Number {
                                shape.number = Some(
                                    doc.objects()
                                        .iter()
                                        .filter_map(|o| o.shape.number)
                                        .max()
                                        .unwrap_or(0)
                                        .saturating_add(1),
                                );
                            }
                            doc.add(shape).map(|id| {
                                self.selected = Some(id);
                            })
                        }
                        Gesture::Crop(crop) => doc.set_crop(Some(crop)),
                        Gesture::Move(id, dx, dy) => doc.move_object(id, dx, dy),
                        _ => Ok(()),
                    };
                    if let Err(error) = result {
                        self.error(error);
                    }
                }
                self.preview()
            }
            Message::Undo | Message::Redo | Message::Delete | Message::Uncrop => {
                if self.busy {
                    return Task::none();
                };
                if let Some(doc) = &mut self.document {
                    match message {
                        Message::Undo => {
                            doc.undo();
                        }
                        Message::Redo => {
                            doc.redo();
                        }
                        Message::Delete => {
                            if let Some(id) = self.selected.take() {
                                let _ = doc.delete(id);
                            }
                        }
                        Message::Uncrop => {
                            let _ = doc.set_crop(None);
                        }
                        _ => {}
                    }
                }
                self.preview()
            }
            Message::Save => {
                if !self.busy && self.document.is_some() {
                    self.confirm = false;
                    self.file_picker(requester::Mode::Save);
                }
                Task::none()
            }
            Message::Open => self.request_pending(Pending::Open),
            Message::Request(event) => {
                if self.busy {
                    return Task::none();
                }
                let outcome = self.picker.as_mut().and_then(|p| p.update(event));
                match outcome {
                    Some(requester::Outcome::Open(paths)) => {
                        self.picker = None;
                        paths
                            .first()
                            .map(|p| self.open_path(PathBuf::from(p)))
                            .unwrap_or_else(Task::none)
                    }
                    Some(requester::Outcome::Save { path, exists }) => {
                        if exists {
                            self.error(label("overwrite"));
                            Task::none()
                        } else {
                            self.picker = None;
                            self.save_path(path.into())
                        }
                    }
                    None => Task::none(),
                }
            }
            Message::Opened(result) => {
                self.busy = false;
                let task = match result {
                    Ok((path, doc)) => {
                        self.preview = None;
                        self.metadata = Value::Null;
                        self.document = Some(doc);
                        self.path = Some(path);
                        self.selected = None;
                        self.zoom = 1.0;
                        self.pan = Point { x: 0.0, y: 0.0 };
                        self.status = label("ready");
                        if let Some(id) = self.pending_reply.take() {
                            let value = self.info();
                            self.reply(id, Ok(value));
                        }
                        self.preview()
                    }
                    Err(error) => {
                        if let Some(id) = self.pending_reply.take() {
                            self.reply(id, Err(error.clone()));
                        }
                        self.error(error);
                        Task::none()
                    }
                };
                if let Some(action) = self.pending.take() {
                    Task::batch([task, self.request_pending(action)])
                } else {
                    task
                }
            }
            Message::Saved(result) => {
                self.busy = false;
                match result {
                    Ok(path) => {
                        if let Some(doc) = &mut self.document {
                            doc.mark_saved();
                        }
                        self.path = Some(path);
                        self.status = label("saved");
                        if let Some(id) = self.pending_reply.take() {
                            let value = self.info();
                            self.reply(id, Ok(value));
                        }
                        if let Some(action) = self.pending.take() {
                            return self.perform_pending(action);
                        }
                    }
                    Err(error) => {
                        if let Some(id) = self.pending_reply.take() {
                            self.reply(id, Err(error.clone()));
                        }
                        self.error(error);
                        self.pending = None;
                    }
                }
                Task::none()
            }
            Message::Copy => {
                if self.busy {
                    return Task::none();
                };
                let Some(doc) = self.document.as_ref().map(Document::raster_snapshot) else {
                    return Task::none();
                };
                self.busy = true;
                Task::perform(work(move || doc.render()), Message::ClipboardImage)
            }
            Message::ClipboardImage(result) => match result {
                Ok(image) => iced::clipboard::write(iced::advanced::clipboard::Content::Image(
                    iced::advanced::clipboard::Image {
                        size: iced::Size::new(image.width(), image.height()),
                        rgba: image.into_raw().into(),
                    },
                ))
                .map(Message::Copied),
                Err(error) => {
                    self.busy = false;
                    self.error(error);
                    self.pending
                        .take()
                        .map(|action| self.request_pending(action))
                        .unwrap_or_else(Task::none)
                }
            },
            Message::Copied(result) => {
                self.busy = false;
                match result {
                    Ok(()) => self.status = label("copied"),
                    Err(error) => {
                        self.error(format!("{}: {error:?}", label("clipboard-unavailable")))
                    }
                }
                self.pending
                    .take()
                    .map(|action| self.request_pending(action))
                    .unwrap_or_else(Task::none)
            }
            Message::Quit => self.request_pending(Pending::Quit),
            Message::Keep => {
                self.dialog = None;
                self.pending = None;
                self.confirm = false;
                self.picker = None;
                Task::none()
            }
            Message::Discard => {
                self.confirm = false;
                self.pending
                    .take()
                    .map(|action| self.perform_pending(action))
                    .unwrap_or_else(Task::none)
            }
            Message::Key(key, modifiers) => {
                use iced::keyboard::{Key, key::Named};
                match key {
                    Key::Named(Named::Escape) => {
                        if self.dialog.is_some() {
                            self.dialog = None;
                        } else if self.confirm {
                            self.pending = None;
                            self.confirm = false;
                        } else if self.picker.is_some() {
                            self.picker = None;
                            self.pending = None;
                        } else if let Some(cancel) = &self.cancel {
                            let _ = cancel.send(true);
                        }
                        self.selected = None;
                        self.preview()
                    }
                    _ if !self.modal() => {
                        if let Some(index) = menu::mnemonic(&key, modifiers) {
                            self.update(Message::OpenMenu(index))
                        } else if let Some(action) = menu::shortcut(&key, modifiers) {
                            self.update(Message::Menu(action))
                        } else {
                            Task::none()
                        }
                    }
                    _ => Task::none(),
                }
            }
            Message::Bus(delivery) => self.bus_delivery(delivery),
            Message::Noop => Task::none(),
        }
    }
    fn bus_delivery(&mut self, delivery: Delivery) -> Task<Message> {
        if self.sync_settings() > 0 {
            eprintln!(
                "CAP_SETTINGS {}",
                json!({
                    "evidence": self.settings_ui.session().host().consumer().evidence(),
                    "settings_cache": self.settings_ui.session().cache_evidence(),
                    "elapsed_ms": self.launched.elapsed().as_millis(),
                })
            );
        }
        match delivery {
            Delivery::Command(command) => self.command(command),
            Delivery::Settings => Task::none(),
            Delivery::Connected => {
                // A real Connected event retries a failed cancellation
                // cleanup; no polling, no second connection, no retargeting.
                let cleanup = self.retry_cleanup();
                Task::batch([cleanup, self.refresh()])
            }
            Delivery::Changed => self.refresh(),
            Delivery::Disconnected => {
                self.error("Bus disconnected");
                Task::none()
            }
            Delivery::Refused { message } => {
                self.refused = true;
                self.status = message;
                Task::none()
            }
            Delivery::Forwarded(result) => match result {
                Ok(()) if !self.touched && !self.bus.ever_registered() => {
                    self.request_pending(Pending::Quit)
                }
                Ok(()) => Task::none(),
                Err(error) => {
                    self.status = error;
                    Task::none()
                }
            },
        }
    }
    fn command(&mut self, command: crate::bus::Command) -> Task<Message> {
        if !self.bus.is_current(&command.id) {
            return Task::none();
        }
        let id = command.id;
        let verb = command.verb.as_str();
        let value = match verbs::parse(verb, &command.body) {
            Ok(v) => v,
            Err(e) => {
                self.reply(id, Err(e));
                return Task::none();
            }
        };
        if verb == "cap.ping" || verb == "cap.info" || verb == "app.describe" {
            // The existing modal bypass: information commands answer with
            // real work state while a dialogue is open.
            let value = if verb == "app.describe" {
                self.describe()
            } else {
                self.info()
            };
            self.reply(id, Ok(value));
            return Task::none();
        }
        if self.modal() && verb != "cap.show" {
            self.reply(id, Err("dialog active".into()));
            return Task::none();
        }
        if verb == "cap.cancel" {
            if let Some(cancel) = &self.cancel {
                let _ = cancel.send(true);
            }
            self.reply(id, Ok(json!({"cancelling":self.cancel.is_some()})));
            return Task::none();
        }
        if self.busy {
            self.reply(id, Err(label("busy")));
            return Task::none();
        }
        if matches!(verb, "cap.open" | "cap.capture" | "cap.quit")
            && self.document.as_ref().is_some_and(Document::dirty)
        {
            self.reply(id, Err(label("save-before-capture")));
            return Task::none();
        }
        match verbs::operation(verb, value.clone()) {
            Ok(Operation::Show) => {
                let bus = self.bus.clone();
                let comp = self.comp.clone();
                self.busy = true;
                Task::perform(
                    async move {
                        let target = capture::own_window(&bus, &comp).await?;
                        capture::show(&bus, &comp, target).await
                    },
                    move |result| Message::Shown(id.clone(), result),
                )
            }
            Ok(Operation::Quit) => {
                self.reply(id, Ok(json!({"quitting":true})));
                self.request_pending(Pending::Quit)
            }
            Ok(Operation::Capture(request)) => {
                self.delay = request.delay.to_string();
                self.request = request;
                self.selected_window = self
                    .windows
                    .iter()
                    .find(|w| Some(&w.target) == self.request.window.as_ref())
                    .cloned();
                self.pending_reply = Some(id.clone());
                let task = self.take();
                if !self.busy {
                    self.pending_reply = None;
                    self.reply(id, Err(self.status.clone()));
                }
                task
            }
            Ok(Operation::Open(path)) => {
                self.pending_reply = Some(id.clone());
                match capture::absolute(&path) {
                    Ok(p) => self.open_path(p),
                    Err(e) => {
                        self.pending_reply = None;
                        self.reply(id, Err(e));
                        Task::none()
                    }
                }
            }
            Ok(Operation::Export(path)) => {
                if self.document.is_none() {
                    self.reply(id, Err("no image".into()));
                    return Task::none();
                };
                self.pending_reply = Some(id.clone());
                match capture::absolute(&path) {
                    Ok(p) => self.save_path(p),
                    Err(e) => {
                        self.pending_reply = None;
                        self.reply(id, Err(e));
                        Task::none()
                    }
                }
            }
            Err(_) if verbs::is_edit(verb) => {
                let result = self
                    .document
                    .as_mut()
                    .ok_or_else(|| "no image".into())
                    .and_then(|doc| verbs::edit(doc, verb, value));
                self.reply(id, result);
                self.preview()
            }
            Err(error) => {
                self.reply(id, Err(error));
                Task::none()
            }
        }
    }
    fn view(&self) -> Element<'_, Message, Theme> {
        let tokens = self.look().tokens();
        let gap = tokens.metrics.spacing.sm;
        let action = |key: &str, message: Message, enabled: bool| {
            let b = button(self.text(label(key)));
            if enabled { b.on_press(message) } else { b }
        };
        let menubar: Element<'_, menu::Action, Theme> =
            toolkit::Menu::bar(menu::bar(&self.menu_context()))
                .id(menu::BAR_ID)
                .style(tokens.menu_style())
                .into();
        let content: Element<'_, Message, Theme> =
            if let (Some(doc), Some(image)) = (&self.document, &self.preview) {
                let canvas: Element<'_, Message, Theme> = canvas::Canvas::new(Picture {
                    document: doc,
                    tool: self.tool,
                    colour: self.colour.into_rgba8(),
                    width: self.width,
                    zoom: self.zoom,
                    pan: self.pan,
                    selected: self.selected,
                    revision: self.revision,
                    busy: self.busy,
                })
                .width(iced::Fill)
                .height(iced::Fill)
                .into();
                crate::preview::plane(canvas, image, doc.output_dimensions(), self.zoom, self.pan)
            } else {
                container(self.text(label("empty")))
                    .center(iced::Fill)
                    .into()
            };
        let status = column![
            row![
                self.text(&self.status),
                widget::space().width(iced::Fill),
                self.text(format!(
                    "{} · {} · {:.0}%",
                    label(self.request.mode.key()),
                    label(self.tool.key()),
                    self.zoom * 100.0
                )),
                self.typography("mono").text(
                    self.document
                        .as_ref()
                        .map(|d| {
                            let (w, h) = d.output_dimensions();
                            format!(
                                "{w} × {h}{}",
                                if d.dirty() {
                                    format!(" · {}", label("not-saved"))
                                } else {
                                    String::new()
                                }
                            )
                        })
                        .unwrap_or_default()
                )
            ]
            .spacing(gap),
            self.text(self.persistent_status())
        ]
        .spacing(gap);
        let base: Element<'_, Message, Theme> = column![
            menubar.map(Message::Menu),
            content,
            container(status).padding(tokens.metrics.spacing.sm),
        ]
        .height(iced::Fill)
        .width(iced::Fill)
        .into();
        if self.confirm {
            let dialog = column![
                self.text(label("discard-title")),
                self.text(label("discard-body")),
                row![
                    action("keep", Message::Keep, true),
                    action("save", Message::Save, true),
                    action("discard", Message::Discard, true)
                ]
                .spacing(gap)
            ]
            .spacing(gap);
            toolkit::dialog::Modal::new(
                base,
                widget::opaque(
                    container(
                        container(dialog)
                            .padding(tokens.metrics.spacing.lg)
                            .style(toolkit::theme::container::card),
                    )
                    .center(iced::Fill),
                ),
            )
            .on_key(modal_key)
            .into()
        } else if let Some(picker) = &self.picker {
            toolkit::dialog::Modal::new(
                base,
                widget::opaque(
                    container(
                        container(column![
                            picker
                                .view_for::<Message, Theme, Renderer>(tokens, &self.picker_strings),
                            action("keep", Message::Keep, true)
                        ])
                        .padding(tokens.metrics.spacing.lg)
                        .width(600)
                        .height(500)
                        .style(toolkit::theme::container::card),
                    )
                    .center(iced::Fill),
                ),
            )
            .on_key(modal_key)
            .into()
        } else if let Some(dialog) = self.dialog {
            let body = match dialog {
                Dialog::Properties => column![
                    self.text(label("annotation-properties")),
                    row![
                        self.text(label("width")),
                        slider(0.5..=40.0, self.width, Message::Width).width(iced::Fill),
                        self.text(format!("{:.1}", self.width)),
                    ]
                    .spacing(gap)
                    .align_y(iced::Center),
                    self.text(label("colour")),
                    toolkit::ColorPicker::new(self.colour, Message::Colour)
                        .width(iced::Fill)
                        .height(tokens.metrics.spacing.xl * 4.0),
                    self.text(label("annotation-text")),
                    text_input(
                        crate::strings::label_ref("text-placeholder"),
                        &self.annotation_text
                    )
                    .on_input(Message::Text)
                    .width(iced::Fill),
                    row![
                        self.text(label("text-size")),
                        text_input(
                            crate::strings::label_ref("text-size-range"),
                            &self.text_size
                        )
                        .on_input(Message::TextSize)
                        .width(iced::Fill),
                    ]
                    .spacing(gap)
                    .align_y(iced::Center),
                ],
                Dialog::Shortcuts => {
                    column![
                        self.text(label("shortcuts")),
                        self.text(label("shortcuts-body"))
                    ]
                }
                Dialog::About => column![
                    self.text(label("about")),
                    self.text(format!("{} {}", label("title"), env!("CARGO_PKG_VERSION"))),
                    self.text(label("about-body"))
                ],
            }
            .spacing(tokens.metrics.spacing.md);
            toolkit::dialog::Modal::new(
                base,
                widget::opaque(
                    container(
                        container(
                            column![
                                widget::scrollable(body).height(iced::Fill),
                                action("done", Message::Keep, true),
                            ]
                            .spacing(gap),
                        )
                        .padding(tokens.metrics.spacing.lg)
                        .width(tokens.metrics.spacing.xl * 22.0)
                        .height(tokens.metrics.spacing.xl * 17.0)
                        .style(toolkit::theme::container::card),
                    )
                    .center(iced::Fill),
                ),
            )
            .on_key(modal_key)
            .into()
        } else {
            base
        }
    }
}

fn modal_key(key: &iced::keyboard::Key, _: iced::keyboard::Modifiers) -> Option<Message> {
    matches!(
        key,
        iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape)
    )
    .then_some(Message::Keep)
}

struct Picture<'a> {
    document: &'a Document,
    tool: Tool,
    colour: [u8; 4],
    width: f32,
    zoom: f32,
    pan: Point,
    selected: Option<u64>,
    revision: u64,
    busy: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::Effect;
    fn test_app() -> App {
        let consumer = settings::consumer::Consumer::for_app(
            settings::Binding {
                instance: "fixture".into(),
                profile: "default".into(),
            },
            "cap",
        )
        .unwrap();
        let (ui, _lane) = application::presentation::native::bridge(
            application::presentation::native::Session::new(consumer),
            application::presentation::native::Worker::offline(|_, _| Ok(())),
        );
        let (bus, _effects) = BusHandle::response_sink();
        App::new(
            bus,
            appearance::settings::bootstrap().unwrap(),
            ui,
            "comp".into(),
            PathBuf::from("/tmp"),
        )
    }
    #[test]
    fn keyboard_menus_dispatch_real_actions_at_the_minimum_window_size() {
        use iced::keyboard::key::Named;
        let mut app = test_app();
        let mut ui = iced_test::Simulator::with_size(
            iced::Settings::default(),
            iced::Size::new(760.0, 450.0),
            app.view(),
        );
        ui.tap_key(Named::F10);
        ui.tap_key(Named::Enter);
        let messages: Vec<_> = ui.into_messages().collect();
        assert!(
            matches!(messages.as_slice(), [Message::Menu(menu::Action::Open)]),
            "{messages:?}"
        );
        for message in messages {
            let _ = app.update(message);
        }
        assert!(app.picker.is_some());
        let _ = app.update(Message::Keep);

        app.document = Some(Document::new(image::RgbaImage::new(100, 100)).unwrap());
        let mut ui = iced_test::Simulator::with_size(
            iced::Settings::default(),
            iced::Size::new(760.0, 450.0),
            app.view(),
        );
        ui.tap_key(Named::F10);
        for _ in 0..3 {
            ui.tap_key(Named::ArrowRight);
        }
        ui.tap_key(Named::Enter);
        let messages: Vec<_> = ui.into_messages().collect();
        assert!(
            matches!(
                messages.as_slice(),
                [Message::Menu(menu::Action::Tool(Tool::Select))]
            ),
            "{messages:?}"
        );
        for message in messages {
            let _ = app.update(message);
        }
        assert_eq!(app.tool, Tool::Select);
    }
    #[test]
    fn text_properties_block_background_actions_and_preserve_text_on_escape() {
        let mut app = test_app();
        app.document = Some(Document::new(image::RgbaImage::new(100, 100)).unwrap());
        let _ = app.update(Message::Menu(menu::Action::Tool(Tool::Draw(Kind::Text))));
        assert!(matches!(app.dialog, Some(Dialog::Properties)));
        let _ = app.update(Message::Text("Keep this annotation".into()));
        let _ = app.update(Message::Menu(menu::Action::Open));
        assert!(app.picker.is_none());
        let (bus, mut effects) = BusHandle::response_sink();
        app.bus = bus;
        let _ = app.update(Message::Bus(Delivery::Command(crate::bus::Command {
            id: 1.into(),
            verb: "cap.open".into(),
            body: json!({"path":"/tmp/another.png"}).to_string(),
            caller_key: "local:test".into(),
        })));
        let Effect::Respond { rc, .. } = effects.try_recv().unwrap() else {
            panic!("refusal");
        };
        assert_ne!(rc, 0);
        let mut ui = iced_test::Simulator::with_size(
            iced::Settings::default(),
            iced::Size::new(760.0, 450.0),
            app.view(),
        );
        ui.tap_key(iced::keyboard::key::Named::Escape);
        let messages: Vec<_> = ui.into_messages().collect();
        assert!(
            matches!(messages.as_slice(), [Message::Keep]),
            "{messages:?}"
        );
        for message in messages {
            let _ = app.update(message);
        }
        assert!(app.dialog.is_none());
        assert_eq!(app.annotation_text, "Keep this annotation");
        assert_eq!(app.tool, Tool::Draw(Kind::Text));
        let _ = app.update(Message::Menu(menu::Action::ZoomIn));
        assert_eq!(app.zoom, 1.25);
        let _ = app.update(Message::Menu(menu::Action::Fit));
        assert_eq!(app.zoom, 1.0);
    }
    #[test]
    fn every_information_and_properties_dialog_keeps_done_visible_in_a_small_window() {
        for dialog in [Dialog::Properties, Dialog::Shortcuts, Dialog::About] {
            let mut app = test_app();
            app.dialog = Some(dialog);
            let mut ui = iced_test::Simulator::with_size(
                iced::Settings::default(),
                iced::Size::new(760.0, 450.0),
                app.view(),
            );
            let bounds = ui.find("Done").unwrap().visible_bounds().unwrap();
            assert!(
                bounds.y >= 0.0 && bounds.y + bounds.height <= 450.0,
                "{dialog:?}: {bounds:?}"
            );
            ui.click("Done").unwrap();
            assert!(matches!(
                ui.into_messages().collect::<Vec<_>>().as_slice(),
                [Message::Keep]
            ));
        }
    }
    #[test]
    fn escape_closes_each_modal_and_discards_its_pending_action() {
        for confirmation in [false, true] {
            let mut app = test_app();
            app.pending = Some(Pending::Quit);
            app.confirm = confirmation;
            if !confirmation {
                app.file_picker(requester::Mode::Open);
            }
            let mut ui = iced_test::Simulator::with_size(
                iced::Settings::default(),
                iced::Size::new(1040.0, 720.0),
                app.view(),
            );
            ui.tap_key(iced::keyboard::key::Named::Escape);
            let messages: Vec<_> = ui.into_messages().collect();
            assert!(
                matches!(messages.as_slice(), [Message::Keep]),
                "{messages:?}"
            );
            for message in messages {
                let _ = app.update(message);
            }
            assert!(!app.confirm);
            assert!(app.picker.is_none());
            assert!(app.pending.is_none());
        }
    }
    #[test]
    fn activation_holds_the_job_slot_until_compositor_confirmation() {
        let mut app = test_app();
        let (bus, mut effects) = BusHandle::response_sink();
        app.bus = bus;
        let command = |id: u64, verb: &str| {
            Message::Bus(Delivery::Command(crate::bus::Command {
                id: id.into(),
                verb: verb.into(),
                body: "{}".into(),
                caller_key: "local:test".into(),
            }))
        };
        let _ = app.update(command(1, "cap.show"));
        assert!(app.busy);
        let _ = app.update(command(2, "cap.capture"));
        let Effect::Respond { id, rc, .. } = effects.try_recv().unwrap() else {
            panic!("capture must be refused")
        };
        assert_eq!(id, 2);
        assert_ne!(rc, 0);
        let _ = app.update(Message::Shown(1.into(), Err("exclusive_layer".into())));
        assert!(!app.busy);
        let Effect::Respond { id, rc, body } = effects.try_recv().unwrap() else {
            panic!("activation reply")
        };
        assert_eq!(id, 1);
        assert_ne!(rc, 0);
        assert!(body.contains("exclusive_layer"));
    }
    fn install_fonts() {
        static INSTALLED: std::sync::Once = std::sync::Once::new();
        INSTALLED.call_once(|| {
            toolkit::fonts::install(
                toolkit::fonts::FontSet::new().sans(
                    include_bytes!("../../../vendor/font/Inter-VariableFont_opsz,wght.ttf")
                        .as_slice(),
                ),
                None,
            )
            .unwrap();
        });
    }
    #[test]
    fn settings_activation_changes_the_appearance_and_keeps_editor_state() {
        install_fonts();
        let consumer = settings::consumer::Consumer::for_app(
            settings::Binding {
                instance: "fixture".into(),
                profile: "default".into(),
            },
            "cap",
        )
        .unwrap();
        let (ui, mut lane) = application::presentation::native::bridge(
            application::presentation::native::Session::new(consumer),
            application::presentation::native::Worker::offline(|_, _| Ok(())),
        );
        let (bus, _effects) = BusHandle::response_sink();
        let mut app = App::new(
            bus,
            appearance::settings::bootstrap().unwrap(),
            ui,
            "comp".into(),
            PathBuf::from("/tmp"),
        );
        let mut doc = Document::new(image::RgbaImage::new(40, 30)).unwrap();
        doc.add(Shape {
            kind: Kind::Rectangle,
            points: vec![Point { x: 2.0, y: 2.0 }, Point { x: 20.0, y: 20.0 }],
            colour: app.colour.into_rgba8(),
            width: 2.0,
            text: None,
            size: None,
            number: None,
        })
        .unwrap();
        app.document = Some(doc);
        app.selected = Some(1);
        app.zoom = 2.5;
        app.pan = Point { x: 7.0, y: 9.0 };
        app.revision = 4;
        app.preview = Some(iced::advanced::image::Handle::from_rgba(
            1,
            1,
            vec![0, 0, 0, 255],
        ));
        app.pending_reply = Some(3.into());
        let undo = app.document.as_ref().unwrap().can_undo();
        assert!(app.settings_ui.session().host().presentation().is_none());
        app.settings_ui.reconcile(None);
        // Drive the real lane on its own bounded runtime; the UI loop drains.
        let driver = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                loop {
                    if let application::presentation::native::Progress::UiClosed = lane.drive().await {
                        break;
                    }
                }
            });
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while app.settings_ui.session().host().presentation().is_none()
            && std::time::Instant::now() < deadline
        {
            app.sync_settings();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            app.settings_ui.session().host().presentation().is_some(),
            "an embedded fallback presentation activates through the real bridge"
        );
        assert!(
            std::ptr::eq(
                app.look(),
                app.settings_ui
                    .session()
                    .host()
                    .presentation()
                    .unwrap()
                    .appearance()
            ),
            "the app renders the activated appearance, not bootstrap"
        );
        assert_eq!(
            app.settings_ui.session().host().consumer().evidence().kind,
            Some(settings::fallback::PresentationKind::Embedded)
        );
        assert!(!app.busy);
        assert_eq!(app.document.as_ref().unwrap().dimensions(), (40, 30));
        assert_eq!(app.document.as_ref().unwrap().objects().len(), 1);
        assert_eq!(app.document.as_ref().unwrap().can_undo(), undo);
        assert!(app.document.as_ref().unwrap().dirty());
        assert_eq!(app.selected, Some(1));
        assert_eq!(app.zoom, 2.5);
        assert_eq!(app.pan, Point { x: 7.0, y: 9.0 });
        assert_eq!(app.revision, 4);
        assert_eq!(
            app.pending_reply.as_ref().map(|request| request.id),
            Some(3)
        );
        assert!(app.preview.is_some());
        drop(app);
        driver.join().unwrap();
    }
    #[test]
    fn describe_bypasses_an_open_dialog_and_carries_canonical_settings_evidence() {
        let mut app = test_app();
        app.dialog = Some(Dialog::About);
        let (bus, mut effects) = BusHandle::response_sink();
        app.bus = bus;
        let _ = app.update(Message::Bus(Delivery::Command(crate::bus::Command {
            id: 1.into(),
            verb: "app.describe".into(),
            body: "{}".into(),
            caller_key: "local:test".into(),
        })));
        let Effect::Respond { id, rc, body } = effects.try_recv().unwrap() else {
            panic!("app.describe must answer while a dialog is open")
        };
        assert_eq!(id, 1);
        assert_eq!(rc, 0);
        let value: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["app_id"], APP_ID);
        assert_eq!(value["settings"]["context"], "app:cap");
        assert!(value["settings_cache"].is_object());
        assert!(matches!(app.dialog, Some(Dialog::About)));
    }
    #[test]
    fn handoff_never_closes_a_touched_window_and_keeps_dirty_work() {
        // A user who touched the window is never closed by a late handoff.
        let mut app = test_app();
        app.touched = true;
        let _ = app.update(Message::Bus(Delivery::Forwarded(Ok(()))));
        assert!(app.pending.is_none());
        // An untouched window with dirty work keeps the work behind its
        // confirm dialogue instead of quitting under it.
        let mut app = test_app();
        app.document = Some(Document::new(image::RgbaImage::new(10, 10)).unwrap());
        app.document
            .as_mut()
            .unwrap()
            .set_crop(Some(Crop {
                x: 0,
                y: 0,
                width: 5,
                height: 5,
            }))
            .unwrap();
        assert!(app.document.as_ref().unwrap().dirty());
        let _ = app.update(Message::Bus(Delivery::Forwarded(Ok(()))));
        assert!(app.confirm);
        assert!(matches!(app.pending, Some(Pending::Quit)));
        assert!(app.document.is_some());
        // A failed forward leaves the refusal visible and never quits.
        let mut app = test_app();
        let _ = app.update(Message::Bus(Delivery::Forwarded(Err(
            "the running instance refused the handoff".into(),
        ))));
        assert_eq!(app.status, "the running instance refused the handoff");
        assert!(app.pending.is_none());
    }
    #[test]
    fn activation_preserves_save_as_for_pending_dirty_close() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = test_app();
        app.directory = directory.path().into();
        let mut document = Document::new(image::RgbaImage::new(10, 10)).unwrap();
        document
            .set_crop(Some(Crop {
                x: 0,
                y: 0,
                width: 5,
                height: 5,
            }))
            .unwrap();
        app.document = Some(document);
        let _ = app.update(Message::Quit);
        assert!(app.confirm);
        let _ = app.update(Message::Save);
        let _ = app.update(Message::Request(requester::Event::Input(
            "chosen.png".into(),
        )));
        let (bus, _) = BusHandle::response_sink();
        app.bus = bus;
        let _ = app.update(Message::Bus(Delivery::Command(crate::bus::Command {
            id: 1.into(),
            verb: "cap.show".into(),
            body: "{}".into(),
            caller_key: "local:test".into(),
        })));
        let _ = app.update(Message::Shown(1.into(), Ok(json!({"focused":true}))));
        assert!(!app.confirm);
        assert!(matches!(app.pending, Some(Pending::Quit)));
        let outcome = app
            .picker
            .as_mut()
            .unwrap()
            .update(requester::Event::Submit);
        let Some(requester::Outcome::Save { path, .. }) = outcome else {
            panic!("save requester retained: {outcome:?}");
        };
        assert_eq!(PathBuf::from(path), directory.path().join("chosen.png"));
    }
    #[test]
    fn file_picker_cannot_start_export_during_activation() {
        let directory = tempfile::tempdir().unwrap();
        let mut app = test_app();
        app.directory = directory.path().into();
        app.document = Some(Document::new(image::RgbaImage::new(10, 10)).unwrap());
        app.file_picker(requester::Mode::Save);
        let _ = app.update(Message::Request(requester::Event::Input(
            "fresh.png".into(),
        )));
        let (bus, _) = BusHandle::response_sink();
        app.bus = bus;
        let _ = app.update(Message::Bus(Delivery::Command(crate::bus::Command {
            id: 1.into(),
            verb: "cap.show".into(),
            body: "{}".into(),
            caller_key: "local:test".into(),
        })));
        assert!(app.busy);
        let _ = app.update(Message::Request(requester::Event::Submit));
        assert!(
            app.picker.is_some(),
            "activation must retain the pending save dialog"
        );
        assert!(app.busy);
        let _ = app.update(Message::Shown(1.into(), Ok(json!({"focused":true}))));
        let _ = app.update(Message::Request(requester::Event::Submit));
        assert!(app.picker.is_none());
        assert!(
            app.busy,
            "export owns the job slot after activation completes"
        );
    }
    #[test]
    fn explicit_agent_window_target_survives_a_different_gui_selection() {
        let mut app = test_app();
        let (bus, _) = BusHandle::response_sink();
        app.bus = bus;
        app.windows = capture::windows(
            &json!({"windows":[{"id":9,"generation":2,"title":"Cached selection"}]}),
        );
        app.selected_window = app.windows.first().cloned();
        let _ = app.update(Message::Bus(Delivery::Command(crate::bus::Command {
            id: 1.into(),
            verb: "cap.capture".into(),
            body: json!({"mode":"window","window":{"id":7,"generation":3}}).to_string(),
            caller_key: "local:test".into(),
        })));
        assert!(app.busy);
        assert_eq!(
            app.request.window,
            Some(Target {
                id: 7,
                generation: 3
            })
        );
    }
    #[test]
    fn a_dirty_document_dialog_refuses_agent_replacement() {
        let mut app = test_app();
        app.document = Some(Document::new(image::RgbaImage::new(10, 10)).unwrap());
        app.confirm = true;
        let (bus, mut effects) = BusHandle::response_sink();
        app.bus = bus;
        let _ = app.update(Message::Bus(Delivery::Command(crate::bus::Command {
            id: 1.into(),
            verb: "cap.open".into(),
            body: json!({"path":"/tmp/another.png"}).to_string(),
            caller_key: "local:test".into(),
        })));
        let Effect::Respond { rc, body, .. } = effects.try_recv().unwrap() else {
            panic!("refusal")
        };
        assert_ne!(rc, 0);
        assert!(body.contains("dialog"));
        assert!(app.confirm);
        assert!(!app.busy);
        assert_eq!(app.document.as_ref().unwrap().dimensions(), (10, 10));
    }
    #[test]
    fn document_replacement_clears_preview_and_rejects_old_raster_results() {
        let mut app = test_app();
        app.document = Some(Document::new(image::RgbaImage::new(10, 10)).unwrap());
        app.preview = Some(iced::advanced::image::Handle::from_rgba(
            10,
            10,
            vec![255; 400],
        ));
        app.metadata = json!({"output":"old"});
        app.rendering = true;
        let old_revision = app.revision;
        let _ = app.update(Message::Opened(Ok((
            PathBuf::from("/tmp/new.png"),
            Document::new(image::RgbaImage::new(20, 30)).unwrap(),
        ))));
        assert!(app.preview.is_none());
        assert!(app.metadata.is_null());
        assert_eq!(app.document.as_ref().unwrap().dimensions(), (20, 30));
        let _ = app.update(Message::Preview(
            old_revision,
            Ok(image::RgbaImage::new(10, 10)),
        ));
        assert!(app.preview.is_none());
        app.preview = Some(iced::advanced::image::Handle::from_rgba(
            10,
            10,
            vec![255; 400],
        ));
        let _ = app.update(Message::Captured(
            0,
            Ok(capture::Captured {
                document: Document::new(image::RgbaImage::new(40, 50)).unwrap(),
                path: PathBuf::from("/tmp/captured.png"),
                metadata: json!({"output":"new"}),
            }),
        ));
        assert!(app.preview.is_none());
        assert_eq!(app.document.as_ref().unwrap().dimensions(), (40, 50));
    }
    #[test]
    fn quitting_during_a_cancelled_capture_preserves_dirty_work() {
        let mut app = test_app();
        let mut doc = Document::new(image::RgbaImage::new(40, 40)).unwrap();
        doc.add(Shape {
            kind: Kind::Rectangle,
            points: vec![Point { x: 2.0, y: 2.0 }, Point { x: 20.0, y: 20.0 }],
            colour: app.colour.into_rgba8(),
            width: 2.0,
            text: None,
            size: None,
            number: None,
        })
        .unwrap();
        app.document = Some(doc);
        app.busy = true;
        let (cancel, rx) = tokio::sync::watch::channel(false);
        app.cancel = Some(cancel);
        let _ = app.update(Message::Quit);
        assert!(*rx.borrow());
        let _ = app.update(Message::Captured(
            0,
            Err(capture::CaptureError::cancelled(None)),
        ));
        assert!(app.confirm);
        assert!(matches!(app.pending, Some(Pending::Quit)));
        assert!(app.document.as_ref().unwrap().dirty());
        assert_eq!(app.document.as_ref().unwrap().objects().len(), 1);
    }
    #[test]
    fn stale_capture_generations_never_update_a_newer_capture() {
        let mut app = test_app();
        app.capture_generation = 4;
        let _ = app.update(Message::Captured(
            3,
            Ok(capture::Captured {
                document: Document::new(image::RgbaImage::new(40, 50)).unwrap(),
                path: PathBuf::from("/tmp/stale.png"),
                metadata: json!({"output":"stale"}),
            }),
        ));
        assert!(
            app.document.is_none(),
            "a late completion from an older attempt installs nothing"
        );
        let _ = app.update(Message::Captured(
            4,
            Ok(capture::Captured {
                document: Document::new(image::RgbaImage::new(20, 30)).unwrap(),
                path: PathBuf::from("/tmp/current.png"),
                metadata: json!({"output":"current"}),
            }),
        ));
        assert_eq!(app.document.as_ref().unwrap().dimensions(), (20, 30));
    }
    #[test]
    fn failed_cleanup_is_retained_and_reported_truthfully() {
        let mut app = test_app();
        let cleanup = capture::Cleanup {
            selection: Some(capture::Selection {
                instance: "itest".into(),
                owner: "owner".into(),
                generation: 2,
            }),
            window: Some(Target {
                id: 1,
                generation: 2,
            }),
        };
        let _ = app.update(Message::Captured(
            0,
            Err(capture::CaptureError {
                message: "cancelled; cleanup failed: broker gone".into(),
                cleanup: Some(cleanup.clone()),
            }),
        ));
        assert_eq!(app.status, "cancelled; cleanup failed: broker gone");
        assert_eq!(app.failed_cleanup, Some(cleanup));
        // The bounded retry abandons the target after the attempt cap.
        for _ in 0..MAX_CLEANUP_ATTEMPTS {
            let _ = app.update(Message::CleanupRetried(Err("still gone".into())));
        }
        assert!(app.failed_cleanup.is_none());
        assert!(app.status.contains("cleanup abandoned"));
        // A stale compositor instance retires the target without retargeting.
        let mut app = test_app();
        app.failed_cleanup = Some(capture::Cleanup {
            selection: Some(capture::Selection {
                instance: "itest".into(),
                owner: "owner".into(),
                generation: 2,
            }),
            window: Some(Target {
                id: 1,
                generation: 2,
            }),
        });
        let _ = app.update(Message::CleanupRetried(Ok(
            capture::CleanupOutcome::StaleInstance,
        )));
        assert!(app.failed_cleanup.is_none());
        assert_eq!(app.status, label("ready"));
    }
    #[test]
    fn full_freehand_stroke_keeps_its_accumulated_geometry() {
        use canvas::Program;
        let doc = Document::new(image::RgbaImage::new(120, 100)).unwrap();
        let picture = Picture {
            document: &doc,
            tool: Tool::Draw(Kind::Pen),
            colour: toolkit::Tokens::default().palette.destructive.into_rgba8(),
            width: 4.0,
            zoom: 1.0,
            pan: Point { x: 0.0, y: 0.0 },
            selected: None,
            revision: 1,
            busy: false,
        };
        let mut state = DragState {
            points: vec![Point { x: 10.0, y: 20.0 }; 16_384],
            revision: 1,
            ..Default::default()
        };
        picture.update(
            &mut state,
            &iced::Event::Mouse(mouse::Event::CursorMoved {
                position: iced::Point::new(30.0, 40.0),
            }),
            iced::Rectangle::with_size(iced::Size::new(120.0, 100.0)),
            mouse::Cursor::Available(iced::Point::new(30.0, 40.0)),
        );
        assert_eq!(state.points.len(), 16_384);
        assert_eq!(state.points[1], Point { x: 10.0, y: 20.0 });
        assert_eq!(*state.points.last().unwrap(), Point { x: 30.0, y: 40.0 });
    }
    #[test]
    fn iced_drag_uses_original_pixels_after_crop_and_zoom() {
        let mut doc = Document::new(image::RgbaImage::from_pixel(
            120,
            100,
            image::Rgba([255, 255, 255, 255]),
        ))
        .unwrap();
        doc.set_crop(Some(Crop {
            x: 20,
            y: 10,
            width: 80,
            height: 60,
        }))
        .unwrap();
        let rendered = doc.render().unwrap();
        let handle = iced::advanced::image::Handle::from_rgba(
            rendered.width(),
            rendered.height(),
            rendered.into_raw(),
        );
        let colour = toolkit::Tokens::default().palette.destructive.into_rgba8();
        let picture = Picture {
            document: &doc,
            tool: Tool::Draw(Kind::Arrow),
            colour,
            width: 4.0,
            zoom: 1.5,
            pan: Point { x: 0.0, y: 0.0 },
            selected: None,
            revision: 1,
            busy: false,
        };
        let element: Element<'_, Message, Theme> = canvas::Canvas::new(picture)
            .width(iced::Fill)
            .height(iced::Fill)
            .into();
        let element = crate::preview::plane(
            element,
            &handle,
            doc.output_dimensions(),
            1.5,
            Point { x: 0.0, y: 0.0 },
        );
        let mut ui = iced_test::Simulator::with_size(
            iced::Settings::default(),
            iced::Size::new(600.0, 400.0),
            element,
        );
        ui.point_at(iced::Point::new(100.0, 100.0));
        ui.simulate([iced::Event::Mouse(mouse::Event::ButtonPressed(
            mouse::Button::Left,
        ))]);
        ui.point_at(iced::Point::new(400.0, 300.0));
        ui.simulate([iced::Event::Mouse(mouse::Event::CursorMoved {
            position: iced::Point::new(400.0, 300.0),
        })]);
        let directory = tempfile::tempdir().unwrap();
        assert!(
            ui.snapshot(&Theme::default())
                .unwrap()
                .matches_image(directory.path().join("guides.png"))
                .unwrap()
        );
        let path = std::fs::read_dir(directory.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let pixels = image::open(path).unwrap().to_rgba8();
        assert_eq!(pixels.get_pixel(10, 10).0, [255, 255, 255, 255]);
        assert_ne!(
            pixels.get_pixel(500, 400).0,
            [255, 255, 255, 255],
            "drag guide must render above the opaque image"
        );
        ui.simulate([iced::Event::Mouse(mouse::Event::ButtonReleased(
            mouse::Button::Left,
        ))]);
        let messages: Vec<_> = ui.into_messages().collect();
        let [Message::Gesture(Gesture::Add(shape))] = messages.as_slice() else {
            panic!("one complete gesture: {messages:?}")
        };
        assert!((shape.points[0].x - 40.0).abs() < 0.001);
        assert!((shape.points[0].y - 30.0).abs() < 0.001);
        assert!((shape.points[1].x - 70.0).abs() < 0.001);
        assert!((shape.points[1].y - 50.0).abs() < 0.001);
        doc.add(shape.clone()).unwrap();
        assert_eq!(doc.output_dimensions(), (80, 60));
        assert_eq!(doc.objects().len(), 1);
        doc.undo();
        assert!(doc.objects().is_empty());
        assert_eq!(
            doc.crop(),
            Some(Crop {
                x: 20,
                y: 10,
                width: 80,
                height: 60
            })
        );
    }
}
#[derive(Default)]
struct DragState {
    points: Vec<Point>,
    moving: Option<u64>,
    panning: bool,
    revision: u64,
}
impl Picture<'_> {
    fn viewport(&self, bounds: iced::Rectangle) -> Viewport {
        Viewport::fit(
            self.document.output_dimensions(),
            (bounds.width, bounds.height),
            self.zoom,
            self.pan,
        )
    }
    fn source(&self, p: Point) -> Point {
        let c = self.document.crop();
        Point {
            x: p.x + c.map_or(0, |c| c.x) as f32,
            y: p.y + c.map_or(0, |c| c.y) as f32,
        }
    }
}
impl canvas::Program<Message, Theme, Renderer> for Picture<'_> {
    type State = DragState;
    fn update(
        &self,
        state: &mut DragState,
        event: &canvas::Event,
        bounds: iced::Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        if state.revision != self.revision || self.busy {
            state.points.clear();
            state.moving = None;
            state.panning = false;
            state.revision = self.revision;
        }
        if self.busy {
            return None;
        }
        let viewport = self.viewport(bounds);
        let position = cursor.position_in(bounds).map(|p| Point { x: p.x, y: p.y });
        match event {
            canvas::Event::Mouse(mouse::Event::ButtonPressed(button))
                if matches!(button, mouse::Button::Left | mouse::Button::Middle) =>
            {
                let p = position?;
                if !viewport.contains(p) && *button == mouse::Button::Left {
                    return None;
                }
                let p = self.source(viewport.image(p));
                state.points = vec![p];
                state.panning = *button == mouse::Button::Middle;
                if self.tool == Tool::Select && !state.panning {
                    state.moving = self.document.hit(p, 6.0 / viewport.scale);
                    return Some(
                        canvas::Action::publish(Message::Gesture(Gesture::Select(state.moving)))
                            .and_capture(),
                    );
                }
                Some(canvas::Action::request_redraw().and_capture())
            }
            canvas::Event::Mouse(mouse::Event::CursorMoved { .. }) if !state.points.is_empty() => {
                if let Some(p) = position {
                    let p = self.source(viewport.image(p));
                    if matches!(self.tool, Tool::Draw(Kind::Pen | Kind::Highlighter))
                        && !state.panning
                    {
                        if state.points.len() < 16_384 {
                            state.points.push(p)
                        } else {
                            *state.points.last_mut().expect("full stroke") = p;
                        }
                    } else {
                        state.points.truncate(1);
                        state.points.push(p)
                    }
                }
                Some(canvas::Action::request_redraw().and_capture())
            }
            canvas::Event::Mouse(mouse::Event::ButtonReleased(button))
                if matches!(button, mouse::Button::Left | mouse::Button::Middle)
                    && !state.points.is_empty() =>
            {
                let points = std::mem::take(&mut state.points);
                if points.len() < 2 {
                    state.panning = false;
                    state.moving = None;
                    return Some(canvas::Action::request_redraw().and_capture());
                }
                let a = points[0];
                let b = *points.last().unwrap();
                let action = if state.panning {
                    state.panning = false;
                    Gesture::Pan((b.x - a.x) * viewport.scale, (b.y - a.y) * viewport.scale)
                } else if let Some(id) = state.moving.take() {
                    Gesture::Move(id, b.x - a.x, b.y - a.y)
                } else {
                    match self.tool {
                        Tool::Draw(kind) => Gesture::Add(Shape {
                            kind,
                            points: if matches!(kind, Kind::Pen | Kind::Highlighter) {
                                points
                            } else {
                                vec![a, b]
                            },
                            colour: self.colour,
                            width: self.width,
                            text: None,
                            size: None,
                            number: None,
                        }),
                        Tool::Crop => {
                            let (w, h) = self.document.dimensions();
                            let x = a.x.min(b.x).floor().clamp(0.0, w as f32) as u32;
                            let y = a.y.min(b.y).floor().clamp(0.0, h as f32) as u32;
                            let r = a.x.max(b.x).ceil().clamp(0.0, w as f32) as u32;
                            let bottom = a.y.max(b.y).ceil().clamp(0.0, h as f32) as u32;
                            if r <= x || bottom <= y {
                                return Some(canvas::Action::request_redraw());
                            }
                            Gesture::Crop(Crop {
                                x,
                                y,
                                width: r - x,
                                height: bottom - y,
                            })
                        }
                        Tool::Select => Gesture::Select(None),
                    }
                };
                Some(canvas::Action::publish(Message::Gesture(action)).and_capture())
            }
            _ => None,
        }
    }
    fn draw(
        &self,
        state: &DragState,
        renderer: &Renderer,
        theme: &Theme,
        bounds: iced::Rectangle,
        _: mouse::Cursor,
    ) -> Vec<canvas::Geometry<Renderer>> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let viewport = self.viewport(bounds);
        let c = self.document.crop();
        let to_view = |p: Point| {
            let p = viewport.view(Point {
                x: p.x - c.map_or(0, |c| c.x) as f32,
                y: p.y - c.map_or(0, |c| c.y) as f32,
            });
            iced::Point::new(p.x, p.y)
        };
        if state.points.len() > 1 && state.revision == self.revision {
            let a = to_view(state.points[0]);
            let b = to_view(*state.points.last().unwrap());
            let path = if matches!(
                self.tool,
                Tool::Crop | Tool::Draw(Kind::Rectangle | Kind::Ellipse | Kind::Redact)
            ) {
                canvas::Path::rectangle(
                    iced::Point::new(a.x.min(b.x), a.y.min(b.y)),
                    iced::Size::new((a.x - b.x).abs(), (a.y - b.y).abs()),
                )
            } else {
                canvas::Path::new(|p| {
                    p.move_to(a);
                    for point in &state.points[1..] {
                        p.line_to(to_view(*point));
                    }
                })
            };
            frame.stroke(
                &path,
                canvas::Stroke::default()
                    .with_color(theme.tokens().palette.primary)
                    .with_width(2.0),
            );
        }
        if let Some(object) = self
            .document
            .objects()
            .iter()
            .find(|o| Some(o.id) == self.selected)
        {
            let (l, t, r, b) = object.shape.bounds();
            let a = to_view(Point { x: l, y: t });
            let b = to_view(Point { x: r, y: b });
            frame.stroke(
                &canvas::Path::rectangle(
                    a,
                    iced::Size::new((b.x - a.x).max(2.0), (b.y - a.y).max(2.0)),
                ),
                canvas::Stroke::default()
                    .with_color(theme.tokens().palette.primary)
                    .with_width(1.0),
            );
        }
        vec![frame.into_geometry()]
    }
    fn mouse_interaction(
        &self,
        _: &DragState,
        bounds: iced::Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if cursor.is_over(bounds) && !self.busy {
            if self.tool == Tool::Select {
                mouse::Interaction::Grab
            } else {
                mouse::Interaction::Crosshair
            }
        } else {
            mouse::Interaction::default()
        }
    }
}

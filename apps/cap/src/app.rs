// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native iced front-end. Workers feed bounded operations back by completion;
//! no redraw heartbeat, shell capture program or portal is used.
use crate::{
    bus::{self, BusHandle, Delivery},
    capture::{self, Mode, Request, Target, Window},
    document::{Crop, Document, Kind, Point, Shape},
    strings::label,
    verbs::{self, Operation},
    viewport::Viewport,
};
use iced::futures::{StreamExt, channel::mpsc::UnboundedReceiver};
use iced::{
    Element, Subscription, Task, mouse,
    widget::{
        self, button, canvas, checkbox, column, container, pick_list, row, slider, text, text_input,
    },
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
#[derive(Debug, Clone)]
pub enum Message {
    Bus(Delivery),
    Window(window::Id, window::Event),
    Refresh,
    Refreshed(Result<(Value, Value), String>),
    Shown(u64, Result<Value, String>),
    Mode(Mode),
    Output(String),
    Choose(Window),
    Delay(String),
    Pointer(bool),
    Take,
    Cancel,
    Captured(Result<capture::Captured, String>),
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
static DELIVERIES: OnceLock<Mutex<Option<UnboundedReceiver<Delivery>>>> = OnceLock::new();
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
    bus: Option<BusHandle>,
    comp: String,
    look: appearance::Appearance,
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
    pending_reply: Option<u64>,
    picker: Option<requester::Requester>,
    picker_strings: requester::Strings,
    pending: Option<Pending>,
    confirm: bool,
}
fn initial(look: appearance::Appearance, directory: PathBuf) -> App {
    let colour = look.tokens.palette.destructive;
    App {
        bus: None,
        comp: "comp".into(),
        look,
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
    }
}
pub fn run(service: &str, url: &str, comp: &str, path: Option<PathBuf>) -> Result<(), String> {
    let (handle, rx) = bus::spawn(service, url).map_err(|e| e.to_string())?;
    let look = appearance::install(&appearance::Theme::load()).map_err(|e| e.to_string())?;
    let font = look.ui_font();
    DELIVERIES
        .set(Mutex::new(Some(rx)))
        .map_err(|_| "Cap already started in this process")?;
    let mut app = initial(look, capture::media_directory()?);
    app.bus = Some(handle);
    app.comp = comp.into();
    let mut startup = vec![app.refresh()];
    if let Some(path) = path {
        startup.push(app.open_path(path));
    }
    let state = std::cell::RefCell::new(Some((app, Task::batch(startup))));
    iced::application(
        move || state.borrow_mut().take().expect("one boot"),
        App::update,
        App::view,
    )
    .title(|_: &App| label("title"))
    .theme(|app: &App| app.look.theme())
    .default_font(font)
    .subscription(App::subscription)
    .window(window::Settings {
        size: iced::Size::new(1040.0, 720.0),
        min_size: Some(iced::Size::new(760.0, 450.0)),
        exit_on_close_request: false,
        platform_specific: window::settings::PlatformSpecific {
            application_id: APP_ID.into(),
            ..Default::default()
        },
        ..Default::default()
    })
    .run()
    .map_err(|e| e.to_string())
}
async fn work<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    crate::worker::run(f).await
}
impl App {
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
        let Some(bus) = self.bus.clone() else {
            return Task::none();
        };
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
    fn info(&self) -> Value {
        json!({"schema":"cap.v1","busy":self.busy,"status":self.status,"document":self.document.as_ref().map(Document::info),"path":self.path,"capture":self.metadata,"mode":self.request.mode,"pid":std::process::id(),"version":env!("CARGO_PKG_VERSION")})
    }
    fn reply(&self, id: u64, result: Result<Value, String>) {
        if let Some(bus) = &self.bus {
            match result {
                Ok(v) => bus.respond(id, 0, v.to_string()),
                Err(e) => bus.respond(id, 10, json!({"error":e}).to_string()),
            }
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
                if let Some(bus) = &self.bus {
                    bus.quit();
                    bus.wait_done(Duration::from_secs(3));
                }
                iced::exit()
            }
        }
    }
    fn take(&mut self) -> Task<Message> {
        if self.busy {
            self.error(label("busy"));
            return Task::none();
        }
        let Some(bus) = self.bus.clone() else {
            self.error("Bus unavailable");
            return Task::none();
        };
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
        let (tx, rx) = tokio::sync::watch::channel(false);
        self.cancel = Some(tx);
        self.busy = true;
        self.status = label("capturing");
        let comp = self.comp.clone();
        let request = self.request.clone();
        let directory = self.directory.clone();
        Task::perform(
            async move {
                let own = capture::own_window(&bus, &comp).await?;
                capture::take(bus, comp, request, Some(own), directory, rx).await
            },
            Message::Captured,
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
    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Refresh => self.refresh(),
            Message::Shown(id, result) => {
                self.reply(id, result);
                Task::none()
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
            Message::Captured(result) => {
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
                    Err(error) => self.error(error),
                }
                if let Some(id) = self.pending_reply.take() {
                    self.reply(
                        id,
                        if self.document.is_some() && self.status == label("capture-complete") {
                            Ok(self.info())
                        } else {
                            Err(self.status.clone())
                        },
                    );
                }
                let task = self.preview();
                if let Some(action) = self.pending.take() {
                    return Task::batch([task, self.request_pending(action)]);
                }
                Task::batch([task, self.refresh()])
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
                if self.busy || self.picker.is_some() || self.confirm {
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
                            self.reply(id, Ok(self.info()));
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
                            self.reply(id, Ok(self.info()));
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
                        if self.confirm {
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
                    Key::Named(Named::Delete) if !self.confirm && self.picker.is_none() => {
                        self.update(Message::Delete)
                    }
                    Key::Character(s)
                        if modifiers.control() && self.picker.is_none() && !self.confirm =>
                    {
                        match s.as_str() {
                            "z" if modifiers.shift() => self.update(Message::Redo),
                            "z" => self.update(Message::Undo),
                            "y" => self.update(Message::Redo),
                            "s" => self.update(Message::Save),
                            "o" => self.update(Message::Open),
                            "c" => self.update(Message::Copy),
                            _ => Task::none(),
                        }
                    }
                    _ => Task::none(),
                }
            }
            Message::Bus(Delivery::Command(command)) => {
                let id = command.id;
                let verb = command.verb.as_str();
                let value = match verbs::parse(verb, &command.body) {
                    Ok(v) => v,
                    Err(e) => {
                        self.reply(id, Err(e));
                        return Task::none();
                    }
                };
                if verb == "cap.ping" || verb == "cap.info" {
                    self.reply(id, Ok(self.info()));
                    return Task::none();
                }
                if (self.picker.is_some() || self.confirm) && verb != "cap.show" {
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
                        if let Some(target) = self.own.clone()
                            && let Some(bus) = self.bus.clone()
                        {
                            let comp = self.comp.clone();
                            return Task::perform(
                                async move {
                                    bus.call(
                                        &comp,
                                        "comp.window.restore",
                                        json!(target),
                                        Duration::from_secs(5),
                                    )
                                    .await?;
                                    bus.call(&comp,"comp.window.focus",json!({"id":target.id,"generation":target.generation,"raise":true}),Duration::from_secs(5)).await
                                },
                                move |result| Message::Shown(id, result),
                            );
                        }
                        self.reply(id, Err("Cap window is not yet known to compd".into()));
                        Task::none()
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
                        self.pending_reply = Some(id);
                        let task = self.take();
                        if !self.busy {
                            self.pending_reply = None;
                            self.reply(id, Err(self.status.clone()));
                        }
                        task
                    }
                    Ok(Operation::Open(path)) => {
                        self.pending_reply = Some(id);
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
                        self.pending_reply = Some(id);
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
            Message::Bus(Delivery::ThemeChanged) => {
                self.look.retheme(&appearance::Theme::load());
                Task::none()
            }
            Message::Bus(Delivery::Disconnected) => {
                self.error("Bus disconnected");
                Task::none()
            }
            Message::Bus(_) | Message::Noop => Task::none(),
        }
    }
    fn view(&self) -> Element<'_, Message, Theme> {
        let tokens = self.look.tokens;
        let gap = tokens.metrics.spacing.sm;
        let action = |key: &str, message: Message, enabled: bool| {
            let b = button(text(label(key)));
            if enabled { b.on_press(message) } else { b }
        };
        let mut modes = row![].spacing(gap);
        for mode in Mode::ALL {
            modes = modes.push(action(mode.key(), Message::Mode(mode), !self.busy).style(
                if self.request.mode == mode {
                    toolkit::theme::button::primary
                } else {
                    toolkit::theme::button::secondary
                },
            ));
        }
        let controls = row![
            modes,
            pick_list(
                self.request.output.clone(),
                self.outputs.clone(),
                String::clone
            )
            .on_select(Message::Output)
            .placeholder(label("output")),
            pick_list(
                self.selected_window.clone(),
                self.windows.clone(),
                Window::to_string
            )
            .on_select(Message::Choose)
            .placeholder(label("choose-window"))
            .width(180),
            text(label("delay")),
            text_input("0–10", &self.delay)
                .on_input(Message::Delay)
                .width(55),
            checkbox(self.request.mode != Mode::Window && self.request.cursor)
                .label(label("pointer"))
                .on_toggle_maybe(
                    (!self.busy && self.request.mode != Mode::Window).then_some(Message::Pointer)
                )
        ]
        .spacing(gap)
        .align_y(iced::Center);
        let actions = row![
            action("take", Message::Take, !self.busy),
            action("cancel", Message::Cancel, self.busy),
            action("refresh", Message::Refresh, !self.busy),
            action("open", Message::Open, !self.busy),
            action("save", Message::Save, !self.busy && self.document.is_some()),
            action("copy", Message::Copy, !self.busy && self.document.is_some()),
            widget::space().width(iced::Fill),
            action(
                "undo",
                Message::Undo,
                !self.busy && self.document.as_ref().is_some_and(Document::can_undo)
            ),
            action(
                "redo",
                Message::Redo,
                !self.busy && self.document.as_ref().is_some_and(Document::can_redo)
            )
        ]
        .spacing(gap);
        let mut tools = row![
            action("select", Message::Tool(Tool::Select), !self.busy),
            action("crop", Message::Tool(Tool::Crop), !self.busy)
        ]
        .spacing(gap);
        for kind in Kind::ALL {
            tools = tools.push(
                action(kind.key(), Message::Tool(Tool::Draw(kind)), !self.busy).style(
                    if self.tool == Tool::Draw(kind) {
                        toolkit::theme::button::primary
                    } else {
                        toolkit::theme::button::secondary
                    },
                ),
            );
        }
        let settings = row![
            text(label("width")),
            slider(0.5..=40.0, self.width, Message::Width).width(120),
            text(format!("{:.1}", self.width)),
            action(
                "delete",
                Message::Delete,
                self.selected.is_some() && !self.busy
            ),
            action(
                "uncrop",
                Message::Uncrop,
                self.document.as_ref().is_some_and(|d| d.crop().is_some()) && !self.busy
            ),
            action("fit", Message::Fit, true),
            button("−").on_press(Message::Zoom(self.zoom / 1.25)),
            text(format!("{} {:.0}%", label("zoom"), self.zoom * 100.0)),
            button("+").on_press(Message::Zoom(self.zoom * 1.25)),
            widget::space().width(iced::Fill),
            text(label("colour")),
            toolkit::ColorPicker::new(self.colour, Message::Colour).width(180)
        ]
        .spacing(gap)
        .align_y(iced::Center);
        let settings = column![
            settings,
            row![
                text_input(
                    crate::strings::label_ref("text-placeholder"),
                    &self.annotation_text
                )
                .on_input(Message::Text)
                .width(iced::Fill),
                text(label("text-size")),
                text_input("8–256", &self.text_size)
                    .on_input(Message::TextSize)
                    .width(70)
            ]
            .spacing(gap)
        ]
        .spacing(gap);
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
                container(text(label("empty"))).center(iced::Fill).into()
            };
        let status = row![
            text(&self.status),
            widget::space().width(iced::Fill),
            text(
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
        .spacing(gap);
        let base: Element<'_, Message, Theme> =
            container(column![controls, actions, tools, settings, content, status].spacing(gap))
                .padding(tokens.metrics.spacing.md)
                .into();
        if self.confirm {
            let dialog = column![
                text(label("discard-title")),
                text(label("discard-body")),
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
            .into()
        } else if let Some(picker) = &self.picker {
            toolkit::dialog::Modal::new(
                base,
                widget::opaque(
                    container(
                        container(column![
                            picker.view::<Message>(tokens, &self.picker_strings),
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
            .into()
        } else {
            base
        }
    }
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
    fn test_app() -> App {
        let theme = appearance::Theme::from_source("cap-test", "mode: \"dark\"\n").unwrap();
        let look = appearance::install_with(
            &theme,
            appearance::FontSources::none(appearance::FontOrigin::NoSet { roots: vec![] }),
        )
        .unwrap();
        initial(look, PathBuf::from("/tmp"))
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
        let _ = app.update(Message::Captured(Ok(capture::Captured {
            document: Document::new(image::RgbaImage::new(40, 50)).unwrap(),
            path: PathBuf::from("/tmp/captured.png"),
            metadata: json!({"output":"new"}),
        })));
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
        let _ = app.update(Message::Captured(Err("cancelled".into())));
        assert!(app.confirm);
        assert!(matches!(app.pending, Some(Pending::Quit)));
        assert!(app.document.as_ref().unwrap().dirty());
        assert_eq!(app.document.as_ref().unwrap().objects().len(), 1);
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
impl canvas::Program<Message, Theme> for Picture<'_> {
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
        renderer: &iced::Renderer,
        theme: &Theme,
        bounds: iced::Rectangle,
        _: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
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

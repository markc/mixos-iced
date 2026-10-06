// SPDX-License-Identifier: MIT OR Apache-2.0
//! Thin native frontend. Completion snapshots replace state; topics only
//! request a refetch. Mutations are serial and are never retried automatically.
use crate::{
    bus::{self, Delivery, Handle, Reply},
    menu::{self, Action},
    model::{self, APP_ID, EDGES, Page, Selection, Snapshot, View, rows, string},
    strings::label,
};
use application::iced::{
    self, Element, Subscription, Task,
    widget::{self, column, container, row, text},
    window,
};
use iced::futures::{StreamExt, channel::mpsc::UnboundedReceiver};
use serde_json::{Value, json};
use std::sync::{Mutex, OnceLock};
use toolkit::Theme;

#[derive(Debug, Clone)]
pub struct Settings {
    pub service: String,
    pub scenes: String,
    pub comp: String,
    pub host: String,
    pub url: String,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            service: model::SERVICE.into(),
            scenes: std::env::var("SCENES_SERVICE").unwrap_or_else(|_| "scenes".into()),
            comp: std::env::var("MIXOS_COMP_SERVICE").unwrap_or_else(|_| "comp".into()),
            host: std::env::var("SCENE_HOST").unwrap_or_else(|_| "shell".into()),
            url: ::bus::client_helpers::resolve_noded_url(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Choice {
    pub key: String,
    pub caption: String,
}
impl std::fmt::Display for Choice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.caption)
    }
}
#[derive(Debug, Clone)]
pub enum Message {
    Action(Action),
    OpenMenu(usize),
    SelectTemplate(Choice),
    SelectScene(Choice),
    SelectPage(String, Choice),
    Bus(Delivery),
    Completed(u64, Result<Reply, String>),
    Shown(Option<u64>, Result<Value, String>),
    Window(window::Id, window::Event),
    Key(iced::keyboard::Key, iced::keyboard::Modifiers),
    Confirm,
    Cancel,
    Noop,
}
#[derive(Debug, Clone)]
enum Dialog {
    Confirm {
        action: Action,
        selection: Selection,
        token: Value,
    },
    Shortcuts,
    About,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Refresh,
    Action,
}
struct Operation {
    ticket: u64,
    epoch: u64,
    kind: Kind,
    reply: Option<u64>,
}
pub struct App {
    settings: Settings,
    bus: Handle,
    look: appearance::Appearance,
    selection: Selection,
    snapshot: Snapshot,
    edge: &'static str,
    epoch: u64,
    next_ticket: u64,
    operation: Option<Operation>,
    refetch: bool,
    dialog: Option<Dialog>,
    status: String,
    notice: Option<String>,
    connected: bool,
    quitting: bool,
}
static STREAM: OnceLock<Mutex<Option<UnboundedReceiver<Delivery>>>> = OnceLock::new();
fn command_selection(args: &Value, current: &Selection) -> Result<Selection, String> {
    let selection: Selection = match args.get("selection") {
        Some(value) => {
            serde_json::from_value(value.clone()).map_err(|e| format!("invalid selection: {e}"))?
        }
        None => current.clone(),
    };
    selection.validate()?;
    Ok(selection)
}
fn deliveries() -> impl iced::futures::Stream<Item = Delivery> {
    let rx = STREAM
        .get()
        .expect("Bus stream installed")
        .lock()
        .unwrap()
        .take()
        .expect("one subscription");
    iced::futures::stream::unfold(rx, |mut rx| async move {
        rx.next().await.map(|message| (message, rx))
    })
}
pub fn run(settings: Settings, selection: Selection) -> Result<(), String> {
    let (bus, rx) = bus::start(&settings.service, &settings.url, &settings.host)?;
    let result = (|| {
        STREAM
            .set(Mutex::new(Some(rx)))
            .map_err(|_| "app already started")?;
        let look = appearance::install(&appearance::Theme::load()).map_err(|e| e.to_string())?;
        let font = look.ui_font();
        let mut app = App::new(settings, bus.clone(), look, selection);
        let initial = app.refresh();
        application::start(
            (app, initial),
            App::update,
            App::view,
            application::Window::new(APP_ID, iced::Size::new(1040.0, 720.0), font)
                .minimum(iced::Size::new(760.0, 450.0))
                .defer_close(),
        )
        .title(|_: &App| label("title"))
        .theme(|app: &App| app.look.theme())
        .subscription(App::subscription)
        .run()
        .map_err(|e| e.to_string())
    })();
    bus.quit();
    bus.wait_done();
    result
}
impl App {
    fn new(
        settings: Settings,
        bus: Handle,
        look: appearance::Appearance,
        selection: Selection,
    ) -> Self {
        Self {
            settings,
            bus,
            look,
            selection,
            snapshot: Snapshot::default(),
            edge: "bottom",
            epoch: 0,
            next_ticket: 0,
            operation: None,
            refetch: false,
            dialog: None,
            status: label("waiting"),
            notice: None,
            connected: true,
            quitting: false,
        }
    }
    fn context(&self) -> menu::Context<'_> {
        menu::Context {
            selection: &self.selection,
            snapshot: &self.snapshot,
            edge: self.edge,
            busy: self.operation.is_some() || !self.connected,
            modal: self.dialog.is_some(),
        }
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
    fn reply_error(&self, id: u64, code: &str, message: &str) {
        self.bus
            .reply(id, 10, json!({"error_code":code,"message":message}));
    }
    fn info(&self) -> Value {
        json!({"schema":"scene-editor.v1","app_id":APP_ID,"version":env!("CARGO_PKG_VERSION"),"pid":std::process::id(),"connected":self.connected,"busy":self.operation.is_some(),"selection":self.selection,"edge":self.edge,"status":self.status,"state_token":self.snapshot.0["state_token"],"ui":{"menu_bar":true,"dialog":match self.dialog{Some(Dialog::Confirm{..})=>Some("confirm"),Some(Dialog::Shortcuts)=>Some("shortcuts"),Some(Dialog::About)=>Some("about"),None=>None}},"snapshot":self.snapshot.0})
    }
    fn start(&mut self, verb: &str, args: Value, kind: Kind, reply: Option<u64>) -> Task<Message> {
        if self.operation.is_some() {
            if let Some(id) = reply {
                self.reply_error(id, "BUSY", "another operation is pending");
            }
            return Task::none();
        }
        self.next_ticket += 1;
        let ticket = self.next_ticket;
        self.operation = Some(Operation {
            ticket,
            epoch: self.epoch,
            kind,
            reply,
        });
        let bus = self.bus.clone();
        let service = self.settings.scenes.clone();
        let verb = verb.to_owned();
        Task::perform(
            async move { bus.call(&service, &verb, args).await },
            move |result| Message::Completed(ticket, result),
        )
    }
    fn refresh(&mut self) -> Task<Message> {
        if self.operation.is_some() {
            self.refetch = true;
            return Task::none();
        }
        self.refetch = false;
        self.start(
            "scenes.editor.snapshot",
            json!({"selection":self.selection}),
            Kind::Refresh,
            None,
        )
    }
    fn request(
        &mut self,
        mut body: Value,
        selection: Selection,
        token: Value,
        reply: Option<u64>,
    ) -> Task<Message> {
        body["selection"] = json!(selection);
        body["state_token"] = token;
        self.notice = None;
        self.status = label("busy");
        self.start("scenes.editor.action", body, Kind::Action, reply)
    }
    fn action(&mut self, action: Action) -> Task<Message> {
        if !menu::enabled(&action, &self.context()) {
            return Task::none();
        }
        match action {
            Action::View(view) => {
                self.notice = None;
                self.selection.view = view;
                if view != View::Arrange {
                    self.selection.page = None;
                }
                self.epoch += 1;
                self.refresh()
            }
            Action::Edge(edge) => {
                self.notice = None;
                self.edge = edge;
                self.selection.view = View::Arrange;
                self.epoch += 1;
                self.refresh()
            }
            Action::Refresh => {
                self.notice = None;
                self.refresh()
            }
            Action::Quit => self.quit(),
            Action::Shortcuts => {
                self.dialog = Some(Dialog::Shortcuts);
                Task::none()
            }
            Action::About => {
                self.dialog = Some(Dialog::About);
                Task::none()
            }
            action if action.confirmation().is_some() => {
                self.dialog = Some(Dialog::Confirm {
                    action,
                    selection: self.selection.clone(),
                    token: self.snapshot.0["state_token"].clone(),
                });
                Task::none()
            }
            action => self.request(
                action.request().expect("remote action"),
                self.selection.clone(),
                self.snapshot.0["state_token"].clone(),
                None,
            ),
        }
    }
    fn quit(&mut self) -> Task<Message> {
        if self.operation.is_some() {
            self.quitting = true;
            return Task::none();
        }
        self.bus.quit();
        iced::exit()
    }
    fn select(&mut self, selection: Selection) -> Task<Message> {
        if self.dialog.is_some()
            || self
                .operation
                .as_ref()
                .is_some_and(|op| op.kind == Kind::Action)
        {
            return Task::none();
        }
        self.selection = selection;
        self.notice = None;
        self.epoch += 1;
        self.refresh()
    }
    fn show(&self, id: Option<u64>) -> Task<Message> {
        let bus = self.bus.clone();
        let comp = self.settings.comp.clone();
        Task::perform(
            async move {
                let mapped = bus
                    .call(
                        &comp,
                        "comp.window.wait",
                        json!({"match":{"app_id":APP_ID},"until":"mapped","timeout_ms":10000}),
                    )
                    .await?;
                if mapped.rc != 0 {
                    return Err(mapped.value.to_string());
                }
                let list = bus.call(&comp, "comp.windows.list", json!({})).await?;
                if list.rc != 0 {
                    return Err(list.value.to_string());
                }
                let window = rows(&list.value["windows"])
                    .iter()
                    .find(|w| {
                        w["app_id"] == APP_ID
                            && w["pid"].as_u64() == Some(u64::from(std::process::id()))
                    })
                    .ok_or("window is not yet known to compd")?;
                let target = json!({"id":window["id"],"generation":window["generation"]});
                let restored = bus
                    .call(&comp, "comp.window.restore", target.clone())
                    .await?;
                if restored.rc != 0 || restored.value["minimized"] != false {
                    return Err("window restoration refused".into());
                }
                let mut target = target;
                target["raise"] = json!(true);
                let focused = bus.call(&comp, "comp.window.focus", target).await?;
                if focused.rc != 0 || focused.value["focused"] != true {
                    return Err("window activation refused".into());
                }
                Ok(json!({"shown":true,"target":focused.value}))
            },
            move |result| Message::Shown(id, result),
        )
    }
    fn command(&mut self, id: u64, verb: &str, body: &str) -> Task<Message> {
        let args: Value = match serde_json::from_str::<Value>(body) {
            Ok(args) if args.is_object() => args,
            _ => {
                self.reply_error(id, "ARGUMENT", "arguments must be a JSON object");
                return Task::none();
            }
        };
        match verb {
            "scene-editor.ping" => {
                self.bus.reply(
                    id,
                    0,
                    json!({"schema":"scene-editor.v1","version":env!("CARGO_PKG_VERSION")}),
                );
                Task::none()
            }
            "scene-editor.info" => {
                self.bus.reply(id, 0, self.info());
                Task::none()
            }
            "app.describe" => {
                self.bus.reply(id, 0, model::describe());
                Task::none()
            }
            "scene-editor.show" => {
                let navigate = args.get("view").is_some() || args.get("scene").is_some();
                if navigate
                    && (self.dialog.is_some()
                        || self
                            .operation
                            .as_ref()
                            .is_some_and(|op| op.kind == Kind::Action))
                {
                    self.reply_error(
                        id,
                        "BUSY",
                        "navigation is blocked while an action or dialogue is open",
                    );
                    return Task::none();
                }
                let mut selection = self.selection.clone();
                if let Some(view) = args.get("view").filter(|v| !v.is_null()) {
                    selection.view = match serde_json::from_value(view.clone()) {
                        Ok(view) => view,
                        Err(_) => {
                            self.reply_error(id, "ARGUMENT", "unknown view");
                            return Task::none();
                        }
                    };
                }
                if let Some(scene) = args.get("scene").filter(|v| !v.is_null()) {
                    let Some(name) = scene.as_str().filter(|name| model::valid_name(name)) else {
                        self.reply_error(id, "ARGUMENT", "invalid scene name");
                        return Task::none();
                    };
                    selection.scene = Some(name.into());
                    selection.page = None;
                    if args.get("view").is_none_or(Value::is_null) {
                        selection.view = View::Installed;
                    }
                }
                let refresh = if navigate {
                    self.select(selection)
                } else {
                    Task::none()
                };
                Task::batch([refresh, self.show(Some(id))])
            }
            "scene-editor.action" => {
                if self.dialog.is_some() || self.operation.is_some() || !self.connected {
                    self.reply_error(id, "BUSY", "an action or dialogue is already pending");
                    return Task::none();
                }
                let token = args
                    .get("state_token")
                    .cloned()
                    .unwrap_or_else(|| self.snapshot.0["state_token"].clone());
                let selection = match command_selection(&args, &self.selection) {
                    Ok(selection) => selection,
                    Err(error) => {
                        self.reply_error(id, "ARGUMENT", &error);
                        return Task::none();
                    }
                };
                self.request(args, selection, token, Some(id))
            }
            "scene-editor.quit" | "app.quit" => {
                if self.operation.is_some() || self.dialog.is_some() {
                    self.reply_error(id, "BUSY", "finish the operation or dialogue first");
                    return Task::none();
                }
                self.bus.reply(id, 0, json!({"quitting":true}));
                self.quit()
            }
            _ => {
                self.reply_error(id, "UNKNOWN_VERB", "unknown scene editor verb");
                Task::none()
            }
        }
    }
    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Action(action) => self.action(action),
            Message::OpenMenu(index) => {
                if self.dialog.is_some() {
                    return Task::none();
                }
                iced::advanced::widget::operate(toolkit::menu::open_operation(menu::BAR_ID, index))
                    .discard()
                    .chain(Task::done(Message::Noop))
            }
            Message::Key(key, mods) => {
                if self.dialog.is_some() {
                    return Task::none();
                }
                if let Some(index) = menu::mnemonic(&key, mods) {
                    return self.update(Message::OpenMenu(index));
                }
                if let Some(action) = menu::shortcut(&key, mods) {
                    return self.action(action);
                }
                Task::none()
            }
            Message::SelectTemplate(choice) => {
                let mut selection = self.selection.clone();
                selection.template = Some(choice.key);
                self.select(selection)
            }
            Message::SelectScene(choice) => {
                let mut selection = self.selection.clone();
                selection.scene = Some(choice.key);
                selection.page = None;
                self.select(selection)
            }
            Message::SelectPage(edge, choice) => {
                if self.dialog.is_some()
                    || self
                        .operation
                        .as_ref()
                        .is_some_and(|op| op.kind == Kind::Action)
                {
                    return Task::none();
                }
                let Some(edge) = EDGES.into_iter().find(|e| *e == edge) else {
                    return Task::none();
                };
                self.edge = edge;
                let mut selection = self.selection.clone();
                selection.page = Some(Page {
                    edge: edge.into(),
                    page: choice.key.clone(),
                });
                if let Some(owner) = self.snapshot.page_owner(&choice.key) {
                    selection.scene = Some(owner);
                }
                self.select(selection)
            }
            Message::Confirm => {
                if self.operation.is_some() {
                    return Task::none();
                }
                if let Some(Dialog::Confirm {
                    action,
                    selection,
                    token,
                }) = self.dialog.take()
                {
                    let mut body = action.request().expect("confirmed remote action");
                    body["confirmed"] = json!(true);
                    self.request(body, selection, token, None)
                } else {
                    Task::none()
                }
            }
            Message::Cancel => {
                self.dialog = None;
                Task::none()
            }
            Message::Completed(ticket, result) => {
                if self.operation.as_ref().is_none_or(|op| op.ticket != ticket) {
                    return Task::none();
                }
                let op = self.operation.take().expect("matching operation");
                let result = match result {
                    Ok(reply) => {
                        match Snapshot::parse(reply.value.clone()) {
                            Ok(snapshot) if op.epoch == self.epoch => {
                                if op.kind == Kind::Action {
                                    self.selection = snapshot.selection();
                                }
                                if op.kind == Kind::Action || self.notice.is_none() {
                                    self.status = if reply.rc == 0 {
                                        string(&snapshot.model()["status"]).into()
                                    } else {
                                        format!(
                                            "{}: {}",
                                            label("error"),
                                            string(&reply.value["failure"]["message"])
                                        )
                                    };
                                }
                                self.snapshot = snapshot;
                            }
                            Ok(_) => {
                                self.notice = None;
                                self.status = label("waiting");
                                self.refetch = true;
                            }
                            Err(error) => {
                                self.status = format!(
                                    "{}: {}",
                                    label("error"),
                                    reply.value["message"].as_str().unwrap_or(&error)
                                );
                                if op.kind == Kind::Action {
                                    self.refetch = true;
                                }
                            }
                        }
                        Ok(reply)
                    }
                    Err(error) => {
                        self.status = format!("{}: {error}", label("error"));
                        // Completion loss is uncertain, never a reason to retry a
                        // mutation. Refetch once to observe what actually landed.
                        if op.kind == Kind::Action {
                            self.refetch = true;
                        }
                        Err(error)
                    }
                };
                if op.kind == Kind::Action && op.epoch == self.epoch {
                    self.notice = Some(self.status.clone());
                }
                if let Some(id) = op.reply {
                    match result {
                        Ok(reply) => self.bus.reply(id, reply.rc, reply.value),
                        Err(error) => self.reply_error(id, "TRANSPORT", &error),
                    }
                }
                if self.quitting {
                    return self.quit();
                }
                if self.refetch {
                    self.refresh()
                } else {
                    Task::none()
                }
            }
            Message::Bus(Delivery::Command { id, verb, body }) => self.command(id, &verb, &body),
            Message::Bus(Delivery::Changed) => self.refresh(),
            Message::Bus(Delivery::Theme) => {
                self.look.retheme(&appearance::Theme::load());
                Task::none()
            }
            Message::Bus(Delivery::Connected) => {
                self.connected = true;
                self.refresh()
            }
            Message::Bus(Delivery::Disconnected) => {
                self.connected = false;
                self.status = label("waiting");
                Task::none()
            }
            Message::Shown(id, result) => {
                if let Some(id) = id {
                    match result {
                        Ok(value) => self.bus.reply(id, 0, value),
                        Err(error) => self.reply_error(id, "ACTIVATION", &error),
                    }
                }
                Task::none()
            }
            Message::Window(_, window::Event::CloseRequested) => {
                self.dialog = None;
                self.quit()
            }
            Message::Window(_, _) | Message::Noop => Task::none(),
        }
    }
    fn choices(&self, key: &str, id: &str) -> Vec<Choice> {
        rows(&self.snapshot.model()[key])
            .iter()
            .map(|r| Choice {
                key: string(&r[id]).into(),
                caption: rows(&r["cells"])
                    .iter()
                    .map(string)
                    .filter(|s| !s.is_empty() && *s != "●")
                    .collect::<Vec<_>>()
                    .join(" · "),
            })
            .collect()
    }
    fn scene_content(&self) -> Element<'_, Message, Theme> {
        let t = self.look.tokens;
        let gap = t.metrics.spacing.md;
        let gallery = self.selection.view == View::Gallery;
        let options = self.choices(
            if gallery { "templates" } else { "scenes" },
            if gallery { "template" } else { "name" },
        );
        let selected = options.iter().position(|o| {
            Some(o.key.as_str())
                == if gallery {
                    self.selection.template.as_deref()
                } else {
                    self.selection.scene.as_deref()
                }
        });
        let list: Element<'_, Message, Theme> = if options.is_empty() {
            container(text(label(if gallery {
                "empty-gallery"
            } else {
                "empty-installed"
            })))
            .center(iced::Fill)
            .into()
        } else {
            toolkit::SelectionList::new(options, move |_, choice| {
                if gallery {
                    Message::SelectTemplate(choice)
                } else {
                    Message::SelectScene(choice)
                }
            })
            .selected(selected)
            .font(self.look.ui_font())
            .text_size(t.metrics.text.md)
            .padding(gap)
            .height(iced::Fill)
            .into()
        };
        let mut details = column![text(label(if gallery {
            "select-template"
        } else {
            "select-scene"
        }))]
        .spacing(gap)
        .width(iced::Fill);
        let selected = &self.snapshot.model()[if gallery { "tpl" } else { "selected" }];
        if selected["shown"] == true {
            details = details.push(text(string(&selected["title"])));
            if gallery {
                for field in ["desc", "needs", "installed"] {
                    details = details.push(text(string(&selected[field])));
                }
            } else {
                details = details.push(text(string(&selected["status"])));
                if let Some(scene) = self.snapshot.scene(&self.selection) {
                    details = details.push(text(label("files")));
                    for field in ["scene", "behaviour", "metadata"] {
                        if let Some(path) = scene["files"][field].as_str() {
                            details = details.push(text(path).font(self.look.mono_font()));
                        }
                    }
                }
                if !rows(&selected["problems"]).is_empty() {
                    details = details.push(text(label("problems")));
                }
                for problem in rows(&selected["problems"]) {
                    details = details.push(text(string(&problem["cells"][0])));
                }
            }
        }
        row![
            container(list)
                .width(iced::FillPortion(3))
                .height(iced::Fill),
            widget::scrollable(details)
                .width(iced::FillPortion(2))
                .height(iced::Fill)
        ]
        .spacing(gap)
        .height(iced::Fill)
        .into()
    }
    fn arrange_content(&self) -> Element<'_, Message, Theme> {
        let t = self.look.tokens;
        let gap = t.metrics.spacing.md;
        let options: Vec<Choice> = EDGES
            .into_iter()
            .map(|edge| Choice {
                key: edge.into(),
                caption: format!(
                    "{} · {} · {}",
                    label(edge),
                    label(string(&self.snapshot.model()["edges"][edge]["mode"])),
                    string(&self.snapshot.model()["edges"][edge]["size"])
                ),
            })
            .collect();
        let edges: Element<'_, Message, Theme> =
            toolkit::SelectionList::new(options, |_, choice| {
                Message::Action(Action::Edge(
                    EDGES
                        .into_iter()
                        .find(|e| *e == choice.key)
                        .unwrap_or("bottom"),
                ))
            })
            .selected(EDGES.iter().position(|edge| *edge == self.edge))
            .font(self.look.ui_font())
            .text_size(t.metrics.text.md)
            .padding(gap)
            .into();
        let pages: Vec<Choice> = rows(&self.snapshot.model()["edges"][self.edge]["pages"])
            .iter()
            .map(|r| Choice {
                key: string(&r["page"]).into(),
                caption: string(&r["cells"][0]).into(),
            })
            .collect();
        let selected = pages.iter().position(|p| {
            self.selection
                .page
                .as_ref()
                .is_some_and(|page| page.edge == self.edge && page.page == p.key)
        });
        let edge = self.edge.to_owned();
        let pages: Element<'_, Message, Theme> =
            toolkit::SelectionList::new(pages, move |_, choice| {
                Message::SelectPage(edge.clone(), choice)
            })
            .selected(selected)
            .font(self.look.ui_font())
            .text_size(t.metrics.text.md)
            .padding(gap)
            .into();
        row![
            column![text(label("edge")), edges].width(iced::FillPortion(2)),
            column![text(label("select-page")), pages]
                .spacing(gap)
                .width(iced::FillPortion(3))
        ]
        .spacing(gap)
        .height(iced::Fill)
        .into()
    }
    pub fn view(&self) -> Element<'_, Message, Theme> {
        let t = self.look.tokens;
        let gap = t.metrics.spacing.md;
        let menubar: Element<'_, Action, Theme> = toolkit::Menu::bar(menu::bar(&self.context()))
            .id(menu::BAR_ID)
            .style(t.menu_style())
            .into();
        let content = if self.selection.view == View::Arrange {
            self.arrange_content()
        } else {
            self.scene_content()
        };
        let mut body = column![menubar.map(Message::Action)].spacing(t.metrics.spacing.sm);
        if self.snapshot.model()["banner"]["shown"] == true {
            body = body.push(
                container(text(string(&self.snapshot.model()["banner"]["text"])))
                    .padding(gap)
                    .style(toolkit::theme::container::card),
            );
        }
        let base: Element<'_, Message, Theme> = body
            .push(container(content).padding(gap).height(iced::Fill))
            .push(
                container(
                    row![
                        text(&self.status),
                        widget::space().width(iced::Fill),
                        text(label(self.selection.view.key()))
                    ]
                    .spacing(gap),
                )
                .padding(t.metrics.spacing.sm),
            )
            .height(iced::Fill)
            .into();
        let Some(dialog) = &self.dialog else {
            return toolkit::dialog::Modal::host(base, None).into();
        };
        let mut contents = column![].spacing(gap);
        let mut controls = row![].spacing(gap);
        match dialog {
            Dialog::Confirm {
                action, selection, ..
            } => {
                contents = contents
                    .push(text(label(action.confirmation().expect("confirm action"))))
                    .push(text(selection.scene.as_deref().unwrap_or_default()));
                controls = controls.push(
                    toolkit::CenteredButton::new(text(label("cancel"))).on_press(Message::Cancel),
                );
                let button = toolkit::CenteredButton::new(text(label("confirm")));
                controls = controls.push(if self.operation.is_none() {
                    button.on_press(Message::Confirm)
                } else {
                    button
                });
            }
            Dialog::Shortcuts => {
                contents = contents
                    .push(text(label("shortcuts")))
                    .push(text(label("shortcut-body")));
                controls = controls.push(
                    toolkit::CenteredButton::new(text(label("done"))).on_press(Message::Cancel),
                );
            }
            Dialog::About => {
                contents = contents
                    .push(text(format!(
                        "{} {}",
                        label("title"),
                        env!("CARGO_PKG_VERSION")
                    )))
                    .push(text(label("about-body")));
                controls = controls.push(
                    toolkit::CenteredButton::new(text(label("done"))).on_press(Message::Cancel),
                );
            }
        }
        toolkit::dialog::Modal::new(
            base,
            widget::opaque(
                container(
                    container(
                        column![widget::scrollable(contents).height(iced::Fill), controls]
                            .spacing(gap),
                    )
                    .padding(t.metrics.spacing.lg)
                    .width(t.metrics.spacing.xl * 22.0)
                    .height(t.metrics.spacing.xl * 17.0)
                    .style(toolkit::theme::container::card),
                )
                .center(iced::Fill),
            ),
        )
        .on_key(|key, _| {
            matches!(
                key,
                iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape)
            )
            .then_some(Message::Cancel)
        })
        .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn app() -> App {
        static LOOK: OnceLock<appearance::Appearance> = OnceLock::new();
        let look = LOOK
            .get_or_init(|| {
                appearance::install_with(
                    &appearance::Theme::embedded(),
                    appearance::FontSources::none(appearance::FontOrigin::NoSet { roots: vec![] }),
                )
                .unwrap()
            })
            .clone();
        App::new(
            Settings::default(),
            Handle::sink(),
            look,
            Selection::default(),
        )
    }
    fn snapshot(selection: &Selection) -> Value {
        json!({"schema":"scene-editor.snapshot.v1","state_token":"a".repeat(64),"selection":selection,"model":{"status":"ready","scenes":[],"templates":[]},"inventory":{"state_ok":true,"scenes":[]},"templates":{"templates":[]},"panels":{}})
    }
    #[test]
    fn remote_selection_is_validated_and_honoured() {
        let current = Selection {
            scene: Some("panel".into()),
            ..Default::default()
        };
        let named = command_selection(
            &json!({"selection":{"view":"installed","scene":"launcher"}}),
            &current,
        )
        .unwrap();
        assert_eq!(named.scene.as_deref(), Some("launcher"));
        assert_eq!(command_selection(&json!({}), &current).unwrap(), current);
        assert!(command_selection(&json!({"selection":{"scene":"../panel"}}), &current).is_err());
    }
    #[test]
    fn navigation_clears_old_page_and_stale_action_busy_status() {
        let mut app = app();
        app.selection.page = Some(Page {
            edge: "bottom".into(),
            page: "scene-panel".into(),
        });
        let initial = app.selection.clone();
        let _ = app.request(
            json!({"action":"reload"}),
            initial.clone(),
            json!("a".repeat(64)),
            None,
        );
        let ticket = app.operation.as_ref().unwrap().ticket;
        let _ = app.action(Action::View(View::Installed));
        assert!(app.selection.page.is_none());
        let _ = app.update(Message::Completed(
            ticket,
            Ok(Reply {
                rc: 0,
                value: snapshot(&initial),
            }),
        ));
        let ticket = app.operation.as_ref().unwrap().ticket;
        let selection = app.selection.clone();
        let _ = app.update(Message::Completed(
            ticket,
            Ok(Reply {
                rc: 0,
                value: snapshot(&selection),
            }),
        ));
        assert_eq!(app.status, "ready");
        assert!(app.operation.is_none());
        assert!(app.notice.is_none());
    }
    #[test]
    fn uncertain_action_refetches_once_and_keeps_warning_until_manual_refresh() {
        let mut app = app();
        let _ = app.request(
            json!({"action":"reload"}),
            app.selection.clone(),
            json!("a".repeat(64)),
            None,
        );
        let ticket = app.operation.as_ref().unwrap().ticket;
        let _ = app.update(Message::Completed(ticket, Err("connection lost".into())));
        let op = app.operation.as_ref().unwrap();
        assert_eq!(op.kind, Kind::Refresh);
        let ticket = op.ticket;
        let selection = app.selection.clone();
        let _ = app.update(Message::Completed(
            ticket,
            Ok(Reply {
                rc: 0,
                value: snapshot(&selection),
            }),
        ));
        assert!(app.status.contains("connection lost"));
        assert!(app.operation.is_none());
        let _ = app.action(Action::Refresh);
        assert!(app.notice.is_none());
    }
    #[test]
    fn stale_refresh_never_overwrites_newer_selection_or_model() {
        let mut app = app();
        let _ = app.refresh();
        let ticket = app.operation.as_ref().unwrap().ticket;
        let initial = app.selection.clone();
        let _ = app.action(Action::View(View::Installed));
        let _ = app.update(Message::Completed(
            ticket,
            Ok(Reply {
                rc: 0,
                value: snapshot(&initial),
            }),
        ));
        assert_eq!(app.selection.view, View::Installed);
        assert!(app.snapshot.0.is_null());
        assert!(
            app.operation
                .as_ref()
                .is_some_and(|op| op.ticket != ticket && op.epoch == app.epoch)
        );
    }
    #[test]
    fn modal_blocks_keyboard_and_agent_mutations() {
        let mut app = app();
        app.dialog = Some(Dialog::Shortcuts);
        let _ = app.update(Message::Key(
            iced::keyboard::Key::Character("q".into()),
            iced::keyboard::Modifiers::CTRL,
        ));
        let _ = app.command(
            1,
            "scene-editor.action",
            "{\"action\":\"remove\",\"confirmed\":true}",
        );
        assert!(app.dialog.is_some());
        assert!(app.operation.is_none());
        assert!(!app.quitting);
        let _ = app.update(Message::Cancel);
        assert!(app.dialog.is_none());
    }
    #[test]
    fn unknown_completion_is_ignored_and_quit_waits_for_accepted_work() {
        let mut app = app();
        let _ = app.refresh();
        let ticket = app.operation.as_ref().unwrap().ticket;
        let _ = app.update(Message::Completed(ticket + 1, Err("stale".into())));
        assert_eq!(app.operation.as_ref().unwrap().ticket, ticket);
        let _ = app.quit();
        assert!(app.quitting);
        assert!(app.operation.is_some());
    }
    #[test]
    fn confirmation_freezes_the_selected_identity_and_state_token() {
        let mut app = app();
        app.selection.scene = Some("panel".into());
        app.snapshot = Snapshot(
            json!({"state_token":"a".repeat(64),"inventory":{"state_ok":true,"scenes":[{"name":"panel","template":"panel"}]}}),
        );
        let _ = app.action(Action::Remote("reset"));
        app.snapshot.0["state_token"] = json!("b".repeat(64));
        assert!(
            matches!(&app.dialog,Some(Dialog::Confirm{selection,token,..}) if selection.scene.as_deref()==Some("panel")&&*token=="a".repeat(64))
        );
    }
    #[test]
    fn menus_navigate_and_modals_keep_done_reachable_at_minimum_size() {
        use iced::keyboard::key::Named;
        let mut app = app();
        let mut ui = application::test::Simulator::with_size(
            iced::Settings::default(),
            iced::Size::new(760.0, 450.0),
            app.view(),
        );
        ui.tap_key(Named::F10);
        ui.tap_key(Named::ArrowLeft);
        ui.tap_key(Named::ArrowLeft);
        ui.tap_key(Named::ArrowDown);
        ui.tap_key(Named::Enter);
        let messages: Vec<_> = ui.into_messages().collect();
        assert!(
            matches!(
                messages.as_slice(),
                [Message::Action(Action::View(View::Installed))]
            ),
            "{messages:?}"
        );
        for dialog in [Dialog::Shortcuts, Dialog::About] {
            app.dialog = Some(dialog);
            let mut ui = application::test::Simulator::with_size(
                iced::Settings::default(),
                iced::Size::new(760.0, 450.0),
                app.view(),
            );
            ui.click("Done").expect("Done is visible and clickable");
            assert!(matches!(
                ui.into_messages().collect::<Vec<_>>().as_slice(),
                [Message::Cancel]
            ));
            let mut ui = application::test::Simulator::with_size(
                iced::Settings::default(),
                iced::Size::new(760.0, 450.0),
                app.view(),
            );
            ui.tap_key(Named::F10);
            ui.tap_key(Named::Escape);
            assert!(matches!(
                ui.into_messages().collect::<Vec<_>>().as_slice(),
                [Message::Cancel]
            ));
        }
    }
}

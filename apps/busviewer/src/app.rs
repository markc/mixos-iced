// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::{
    bus::{self, Delivery, Handle, Reply},
    menu::{self, Action},
    model::{self, APP_ID, Selection, Snapshot},
    strings::label,
};
use application::iced::{
    self, Element, Subscription, Task,
    widget::{self, column, container, row, text, text_editor},
    window,
};
use iced::futures::{StreamExt, channel::mpsc::Receiver};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    sync::{Mutex, OnceLock},
};
use toolkit::{
    Theme,
    tree::{Children, Nodes},
};

#[derive(Debug, Clone)]
pub struct Settings {
    pub service: String,
    pub comp: String,
    pub url: String,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            service: "busviewer".into(),
            comp: std::env::var("MIXOS_COMP_SERVICE").unwrap_or_else(|_| "comp".into()),
            url: ::bus::client_helpers::resolve_noded_url(),
        }
    }
}
#[derive(Debug, Clone)]
pub enum Message {
    Action(Action),
    OpenMenu(usize),
    Filter(String),
    Toggle(String),
    Select(toolkit::Selection),
    Body(text_editor::Action),
    Reply(text_editor::Action),
    Split(f32),
    Bus(Delivery),
    Discovered(u64, Snapshot),
    Completed(u64, Result<Reply, String>),
    Shown(Option<u64>, Result<Value, String>),
    Window(window::Id, window::Event),
    Key(iced::keyboard::Key, iced::keyboard::Modifiers),
    Cancel,
    Noop,
}
#[derive(Debug, Clone)]
enum Row {
    Service(String),
    Verb(Selection),
    Error(String),
    Peers,
    Peer(String),
}
#[derive(Debug, Clone)]
struct Call {
    ticket: u64,
    target: Selection,
    body: String,
    reply: Option<u64>,
}
pub struct App {
    settings: Settings,
    bus: Handle,
    look: appearance::Appearance,
    snapshot: Snapshot,
    tree: Nodes<String, Row>,
    expanded: BTreeSet<String>,
    selected: Option<Selection>,
    row_key: Option<String>,
    filter: String,
    body: text_editor::Content,
    reply: text_editor::Content,
    last_reply: Value,
    split: f32,
    next_ticket: u64,
    discovery: Option<(u64, Option<u64>)>,
    call: Option<Call>,
    refetch: bool,
    connected: bool,
    dialog: Option<Action>,
    status: String,
    quitting: bool,
}
static STREAM: OnceLock<Mutex<Option<Receiver<Delivery>>>> = OnceLock::new();
fn deliveries() -> impl iced::futures::Stream<Item = Delivery> {
    let rx = STREAM
        .get()
        .expect("Bus stream installed")
        .lock()
        .unwrap()
        .take()
        .expect("one subscription");
    iced::futures::stream::unfold(rx, |mut rx| async move {
        rx.next().await.map(|event| (event, rx))
    })
    .chain(iced::futures::stream::once(async {
        Delivery::Disconnected
    }))
}
pub fn run(settings: Settings) -> Result<(), String> {
    let (bus, rx) = bus::start(&settings.service, &settings.url)?;
    let result = (|| {
        STREAM
            .set(Mutex::new(Some(rx)))
            .map_err(|_| "app already started")?;
        let look = appearance::install(&appearance::Theme::load()).map_err(|e| e.to_string())?;
        let font = look.ui_font();
        let mut app = App::new(settings, bus.clone(), look);
        let initial = app.refresh(None);
        application::start(
            (app, initial),
            App::update,
            App::view,
            application::Window::new(APP_ID, iced::Size::new(1100.0, 760.0), font)
                .minimum(iced::Size::new(760.0, 500.0))
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
    fn new(settings: Settings, bus: Handle, look: appearance::Appearance) -> Self {
        Self {
            settings,
            bus,
            look,
            snapshot: Snapshot::default(),
            tree: Nodes::new(),
            expanded: BTreeSet::new(),
            selected: None,
            row_key: None,
            filter: String::new(),
            body: text_editor::Content::new(),
            reply: text_editor::Content::new(),
            last_reply: Value::Null,
            split: 0.34,
            next_ticket: 0,
            discovery: None,
            call: None,
            refetch: false,
            connected: true,
            dialog: None,
            status: label("connecting"),
            quitting: false,
        }
    }
    fn busy(&self) -> bool {
        self.discovery.is_some() || self.call.is_some()
    }
    fn next(&mut self) -> u64 {
        self.next_ticket += 1;
        self.next_ticket
    }
    fn info(&self) -> Value {
        json!({"schema":"busviewer.v1","app_id":APP_ID,"version":env!("CARGO_PKG_VERSION"),"pid":std::process::id(),"connected":self.connected,"busy":self.busy(),"discovering":self.discovery.is_some(),"calling":self.call.is_some(),"selection":self.selected,"body":self.body.text(),"reply":self.last_reply,"status":self.status,"snapshot":self.snapshot,"ui":{"menu_bar":true,"dialog":self.dialog.as_ref().map(|a|format!("{a:?}")),"row_key":self.row_key}})
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
    fn error(&self, id: u64, code: &str, message: &str) {
        self.bus
            .reply(id, 10, json!({"error_code":code,"message":message}));
    }
    fn refresh(&mut self, reply: Option<u64>) -> Task<Message> {
        if self.busy() || !self.connected || self.quitting {
            if let Some(id) = reply {
                self.error(
                    id,
                    if self.connected {
                        "BUSY"
                    } else {
                        "DISCONNECTED"
                    },
                    &label(if self.connected {
                        "busy"
                    } else {
                        "disconnected"
                    }),
                );
            } else if self.connected && !self.quitting {
                self.refetch = true;
            }
            return Task::none();
        }
        let ticket = self.next();
        self.discovery = Some((ticket, reply));
        self.refetch = false;
        self.status = label("discovering");
        Task::perform(bus::discover(self.bus.clone()), move |snapshot| {
            Message::Discovered(ticket, snapshot)
        })
    }
    fn target(&self, args: &Value) -> Result<Selection, String> {
        match (args.get("service"), args.get("verb")) {
            (None, None) => self.selected.clone().ok_or_else(|| label("invalid-target")),
            (Some(service), Some(verb)) => Ok(Selection {
                service: service.as_str().ok_or("service must be a string")?.into(),
                verb: verb.as_str().ok_or("verb must be a string")?.into(),
            }),
            _ => Err("service and verb must be supplied together".into()),
        }
    }
    fn start_call(&mut self, target: Selection, body: String, reply: Option<u64>) -> Task<Message> {
        let error = if self.busy() || self.dialog.is_some() || !self.connected || self.quitting {
            Some(label("busy"))
        } else if self.snapshot.verb(&target).is_none() {
            Some(label("invalid-target"))
        } else {
            model::validate_body(&body)
                .err()
                .map(|e| format!("{}: {e}", label("invalid-json")))
        };
        if let Some(error) = error {
            if let Some(id) = reply {
                self.error(
                    id,
                    if !self.connected {
                        "DISCONNECTED"
                    } else if self.busy() || self.dialog.is_some() || self.quitting {
                        "BUSY"
                    } else {
                        "ARGUMENT"
                    },
                    &error,
                );
            } else {
                self.status = error;
            }
            return Task::none();
        }
        let ticket = self.next();
        self.call = Some(Call {
            ticket,
            target: target.clone(),
            body: body.clone(),
            reply,
        });
        self.status = label("calling");
        let bus = self.bus.clone();
        Task::perform(
            async move { bus.raw(&target.service, &target.verb, body).await },
            move |result| Message::Completed(ticket, result),
        )
    }
    fn rebuild(&mut self) {
        self.tree.clear();
        let filter = self.filter.to_lowercase();
        for (service, result) in &self.snapshot.services {
            let key = format!("service:{service}");
            let service_matches = service.to_lowercase().contains(&filter);
            let verbs = result
                .as_ref()
                .ok()
                .map(|verbs| {
                    verbs
                        .iter()
                        .filter(|v| {
                            service_matches
                                || format!("{} {} {}", v.name, v.args, v.description)
                                    .to_lowercase()
                                    .contains(&filter)
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if !service_matches && verbs.is_empty() {
                continue;
            }
            self.tree.push(
                None,
                key.clone(),
                Row::Service(service.clone()),
                Children::Loaded,
            );
            for verb in verbs {
                self.tree.push(
                    Some(&key),
                    format!("verb:{service}:{}", verb.name),
                    Row::Verb(Selection {
                        service: service.clone(),
                        verb: verb.name.clone(),
                    }),
                    Children::None,
                );
            }
            if let Err(error) = result {
                self.tree.push(
                    Some(&key),
                    format!("error:{service}"),
                    Row::Error(error.clone()),
                    Children::None,
                );
            }
            self.tree
                .set_expanded(&key, self.expanded.contains(&key) || !filter.is_empty());
        }
        let key = "mesh".to_owned();
        self.tree
            .push(None, key.clone(), Row::Peers, Children::Loaded);
        if let Some(error) = &self.snapshot.peer_error {
            self.tree.push(
                Some(&key),
                "mesh:error".into(),
                Row::Error(error.clone()),
                Children::None,
            );
        } else if self.snapshot.peers.is_empty() {
            self.tree.push(
                Some(&key),
                "mesh:empty".into(),
                Row::Peer(label("no-peers")),
                Children::None,
            );
        } else {
            for peer in &self.snapshot.peers {
                if peer.to_lowercase().contains(&filter) {
                    self.tree.push(
                        Some(&key),
                        format!("peer:{peer}"),
                        Row::Peer(peer.clone()),
                        Children::None,
                    );
                }
            }
        }
        self.tree
            .set_expanded(&key, self.expanded.contains(&key) || !filter.is_empty());
        if let Some(selected) = &self.selected {
            self.tree
                .expand_to(&format!("verb:{}:{}", selected.service, selected.verb));
        }
    }
    fn quit(&mut self) -> Task<Message> {
        self.quitting = true;
        if self.busy() {
            self.status = label("quitting");
            Task::none()
        } else {
            self.bus.quit();
            iced::exit()
        }
    }
    fn action(&mut self, action: Action) -> Task<Message> {
        if self.dialog.is_some() || self.quitting {
            return Task::none();
        }
        match action {
            Action::Refresh => self.refresh(None),
            Action::Quit => self.quit(),
            Action::Call => self
                .selected
                .clone()
                .map(|target| self.start_call(target, self.body.text(), None))
                .unwrap_or_else(Task::none),
            Action::Format if !self.busy() => {
                let body = self.body.text();
                match model::validate_body(&body) {
                    Ok(()) => self.body = text_editor::Content::with_text(&model::pretty(&body)),
                    Err(error) => self.status = format!("{}: {error}", label("invalid-json")),
                };
                Task::none()
            }
            Action::Clear if !self.busy() => {
                self.body = text_editor::Content::new();
                Task::none()
            }
            Action::Copy => iced::clipboard::write(self.reply.text()).map(|_| Message::Noop),
            Action::About | Action::Shortcuts => {
                self.dialog = Some(action);
                Task::none()
            }
            _ => Task::none(),
        }
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
                    return Err(mapped.body);
                }
                let list = bus.call(&comp, "comp.windows.list", json!({})).await?;
                if list.rc != 0 {
                    return Err(list.body);
                }
                let value: Value = serde_json::from_str(&list.body).map_err(|e| e.to_string())?;
                let window = value["windows"]
                    .as_array()
                    .and_then(|rows| {
                        rows.iter().find(|w| {
                            w["app_id"] == APP_ID
                                && w["pid"].as_u64() == Some(u64::from(std::process::id()))
                        })
                    })
                    .ok_or("window not known to compd")?;
                let mut target = json!({"id":window["id"],"generation":window["generation"]});
                let restored = bus
                    .call(&comp, "comp.window.restore", target.clone())
                    .await?;
                let state: Value =
                    serde_json::from_str(&restored.body).map_err(|e| e.to_string())?;
                if restored.rc != 0 || state["minimized"] != false {
                    return Err(restored.body);
                }
                target["raise"] = json!(true);
                let focused = bus.call(&comp, "comp.window.focus", target).await?;
                let state: Value =
                    serde_json::from_str(&focused.body).map_err(|e| e.to_string())?;
                if focused.rc != 0 || state["focused"] != true {
                    return Err(focused.body);
                }
                Ok(json!({"shown":true,"target":state}))
            },
            move |result| Message::Shown(id, result),
        )
    }
    fn command(&mut self, id: u64, verb: &str, body: &str) -> Task<Message> {
        let args = match serde_json::from_str::<Value>(body) {
            Ok(args) if args.is_object() => args,
            _ => {
                self.error(id, "ARGUMENT", "arguments must be a JSON object");
                return Task::none();
            }
        };
        match verb {
            "busviewer.ping" => self.bus.reply(
                id,
                0,
                json!({"schema":"busviewer.v1","version":env!("CARGO_PKG_VERSION")}),
            ),
            "busviewer.info" => self.bus.reply(id, 0, self.info()),
            "HELP" => self.bus.reply(id, 0, model::describe()["verbs"].clone()),
            "app.describe" => self.bus.reply(id, 0, model::describe()),
            "busviewer.show" => return self.show(Some(id)),
            "busviewer.refresh" if self.dialog.is_none() => return self.refresh(Some(id)),
            "busviewer.select" if !self.busy() && self.dialog.is_none() && !self.quitting => {
                match self.target(&args) {
                    Ok(target) if self.snapshot.verb(&target).is_some() => {
                        self.selected = Some(target);
                        self.row_key = self
                            .selected
                            .as_ref()
                            .map(|s| format!("verb:{}:{}", s.service, s.verb));
                        self.rebuild();
                        self.bus.reply(id, 0, self.info());
                    }
                    _ => self.error(id, "ARGUMENT", &label("invalid-target")),
                }
            }
            "busviewer.call" => {
                let target = match self.target(&args) {
                    Ok(target) => target,
                    Err(error) => {
                        self.error(id, "ARGUMENT", &error);
                        return Task::none();
                    }
                };
                let body = match args.get("body") {
                    None => self.body.text(),
                    Some(Value::String(body)) => body.clone(),
                    _ => {
                        self.error(id, "ARGUMENT", "body must be JSON text");
                        return Task::none();
                    }
                };
                return self.start_call(target, body, Some(id));
            }
            "busviewer.quit" if !self.busy() && self.dialog.is_none() => {
                self.bus.reply(id, 0, json!({"quitting":true}));
                return self.quit();
            }
            "busviewer.select" | "busviewer.refresh" | "busviewer.quit" => {
                self.error(id, "BUSY", &label("busy"))
            }
            _ => self.error(id, "UNKNOWN_VERB", "unknown BusViewer verb"),
        }
        Task::none()
    }
    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Action(action) => self.action(action),
            Message::OpenMenu(index) if self.dialog.is_none() => {
                iced::advanced::widget::operate(toolkit::menu::open_operation(menu::BAR_ID, index))
                    .map(|()| Message::Noop)
                    .chain(Task::done(Message::Noop))
            }
            Message::Key(key, mods) => {
                if self.dialog.is_some() {
                    if key == iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape) {
                        self.dialog = None;
                    }
                    return Task::none();
                }
                if key == iced::keyboard::Key::Named(iced::keyboard::key::Named::F10)
                    && mods == iced::keyboard::Modifiers::empty()
                {
                    return self.update(Message::OpenMenu(0));
                }
                if let Some(index) = menu::mnemonic(&key, mods) {
                    return self.update(Message::OpenMenu(index));
                }
                menu::shortcut(&key, mods)
                    .map(|action| self.action(action))
                    .unwrap_or_else(Task::none)
            }
            Message::Cancel => {
                self.dialog = None;
                Task::none()
            }
            Message::Filter(filter) if self.dialog.is_none() => {
                self.filter = filter;
                self.rebuild();
                Task::none()
            }
            Message::Toggle(key) if self.dialog.is_none() => {
                if self.tree.toggle(&key) == Some(true) {
                    self.expanded.insert(key);
                } else {
                    self.expanded.remove(&key);
                }
                Task::none()
            }
            Message::Select(selection) if self.dialog.is_none() => {
                if let Some(row) = selection.cursor().and_then(|row| self.tree.visible(row)) {
                    self.row_key = Some(row.key.clone());
                    self.selected = match row.data {
                        Row::Verb(target) => Some(target.clone()),
                        _ => None,
                    };
                }
                Task::none()
            }
            Message::Body(action) if self.dialog.is_none() && self.call.is_none() => {
                let before = self.body.text();
                self.body.perform(action);
                if self.body.text().len() > model::BODY_LIMIT {
                    self.body = text_editor::Content::with_text(&before);
                    self.status = label("body-too-large");
                }
                Task::none()
            }
            Message::Reply(action) if !action.is_edit() && self.dialog.is_none() => {
                self.reply.perform(action);
                Task::none()
            }
            Message::Split(split) if self.dialog.is_none() => {
                self.split = split.clamp(0.2, 0.65);
                Task::none()
            }
            Message::Discovered(ticket, snapshot) => {
                let Some((expected, reply)) = self.discovery else {
                    return Task::none();
                };
                if expected != ticket {
                    return Task::none();
                }
                self.discovery = None;
                if snapshot.error.is_some() {
                    self.snapshot.error = snapshot.error;
                } else {
                    self.snapshot = snapshot;
                }
                if self.selected.as_ref().is_some_and(|target| {
                    self.snapshot.error.is_none()
                        && !matches!(self.snapshot.services.get(&target.service), Some(Err(_)))
                        && self.snapshot.verb(target).is_none()
                }) {
                    self.selected = None;
                    self.row_key = None;
                }
                self.rebuild();
                self.status = if !self.connected {
                    label("disconnected")
                } else if let Some(error) = &self.snapshot.error {
                    format!("{}: {error}", label("discovery-failed"))
                } else {
                    format!(
                        "{} — {} {} · {} {} · {} {}",
                        label("connected"),
                        self.snapshot.services.len(),
                        label("services"),
                        self.snapshot.peers.len(),
                        label("peers"),
                        self.snapshot.failures(),
                        label("descriptions-failed")
                    )
                };
                if let Some(id) = reply {
                    self.bus.reply(
                        id,
                        if self.snapshot.error.is_some() { 10 } else { 0 },
                        self.info(),
                    );
                }
                self.followup()
            }
            Message::Completed(ticket, result) => {
                let Some(call) = self.call.as_ref() else {
                    return Task::none();
                };
                if call.ticket != ticket {
                    return Task::none();
                }
                let call = self.call.take().expect("matching call");
                self.last_reply = match result {
                    Ok(reply) => {
                        json!({"service":call.target.service,"verb":call.target.verb,"request_body":call.body,"rc":reply.rc,"body":reply.body})
                    }
                    Err(error) => {
                        json!({"service":call.target.service,"verb":call.target.verb,"request_body":call.body,"transport_error":error,"outcome_unknown":true,"retried":false})
                    }
                };
                let rendered = if let Some(rc) = self.last_reply["rc"].as_u64() {
                    format!(
                        "{}  {}\n{} = {rc}\n\n{}",
                        call.target.service,
                        call.target.verb,
                        label("rc"),
                        model::pretty(self.last_reply["body"].as_str().unwrap_or_default())
                    )
                } else {
                    format!(
                        "{}  {}\n{}\n{}",
                        call.target.service,
                        call.target.verb,
                        label("transport-error"),
                        self.last_reply["transport_error"]
                            .as_str()
                            .unwrap_or_default()
                    )
                };
                self.reply = text_editor::Content::with_text(&model::bounded(&rendered));
                self.status = label(if self.connected {
                    "connected"
                } else {
                    "disconnected"
                });
                if let Some(id) = call.reply {
                    self.bus.reply(
                        id,
                        if self.last_reply["transport_error"].is_null() {
                            0
                        } else {
                            10
                        },
                        self.last_reply.clone(),
                    );
                }
                self.followup()
            }
            Message::Bus(Delivery::Command { id, verb, body }) => self.command(id, &verb, &body),
            Message::Bus(Delivery::Changed) => {
                if self.busy() {
                    self.refetch = true;
                    Task::none()
                } else {
                    self.refresh(None)
                }
            }
            Message::Bus(Delivery::Connected) => {
                self.connected = true;
                self.refresh(None)
            }
            Message::Bus(Delivery::Disconnected) => {
                self.connected = false;
                self.status = label("disconnected");
                Task::none()
            }
            Message::Bus(Delivery::Theme) => {
                if let Ok(look) = appearance::install(&appearance::Theme::load()) {
                    self.look = look;
                }
                Task::none()
            }
            Message::Shown(id, result) => {
                if let Some(id) = id {
                    match result {
                        Ok(value) => self.bus.reply(id, 0, value),
                        Err(error) => self.error(id, "ACTIVATION", &error),
                    }
                }
                Task::none()
            }
            Message::Window(_, window::Event::CloseRequested) => {
                self.dialog = None;
                self.quit()
            }
            _ => Task::none(),
        }
    }
    fn followup(&mut self) -> Task<Message> {
        if self.quitting {
            self.quit()
        } else if self.refetch {
            self.refresh(None)
        } else {
            Task::none()
        }
    }
    pub fn view(&self) -> Element<'_, Message, Theme> {
        let t = self.look.tokens;
        let gap = t.metrics.spacing.md;
        let selected = self
            .row_key
            .as_ref()
            .and_then(|key| self.tree.position(key))
            .map(toolkit::Selection::single)
            .unwrap_or_default();
        let tree = toolkit::TreeView::new(&self.tree, |row| {
            text(match row.data {
                Row::Service(name) | Row::Peer(name) => name.clone(),
                Row::Error(_) => label("descriptions-failed"),
                Row::Verb(target) => target.verb.clone(),
                Row::Peers => label("peers"),
            })
        })
        .on_toggle(Message::Toggle)
        .on_select(Message::Select)
        .selection(&selected);
        let left = column![
            text(label("services")).size(t.metrics.text.lg),
            toolkit::TextField::new(&label("search"), &self.filter)
                .on_input(Message::Filter)
                .id("busviewer-search"),
            tree
        ]
        .spacing(gap)
        .height(iced::Fill);
        let details = match self
            .selected
            .as_ref()
            .and_then(|selection| self.snapshot.verb(selection).map(|verb| (selection, verb)))
        {
            Some((selection, verb)) => format!(
                "{}  {}\n{}: {}\n{}: {}\n\n{}",
                selection.service,
                selection.verb,
                label("arguments"),
                if verb.args.is_empty() {
                    label("unspecified")
                } else {
                    verb.args.clone()
                },
                label("read-only"),
                label(match verb.read_only {
                    Some(true) => "yes",
                    Some(false) => "no",
                    None => "unknown",
                }),
                if verb.description.is_empty() {
                    label("description-unavailable")
                } else {
                    verb.description.clone()
                }
            ),
            None => self
                .row_key
                .as_ref()
                .and_then(|key| self.tree.get(key))
                .map(|row| match row {
                    Row::Error(error) => error.clone(),
                    Row::Service(name) => name.clone(),
                    Row::Peer(name) => format!("{}: {name}", label("peer-membership")),
                    _ => label("select"),
                })
                .unwrap_or_else(|| label("select")),
        };
        let body = text_editor(&self.body)
            .on_action(Message::Body)
            .placeholder(label("body"))
            .height(t.metrics.text.md * 7.0)
            .font(self.look.mono_font());
        let right = column![
            widget::scrollable(text(details)).height(t.metrics.text.md * 9.0),
            text(label("body")),
            body,
            text(label("reply")),
            text_editor(&self.reply)
                .on_action(Message::Reply)
                .height(iced::Fill)
                .font(self.look.mono_font())
        ]
        .spacing(gap)
        .height(iced::Fill);
        let split = toolkit::Split::new(self.split, left, right).on_drag(Message::Split);
        let bar: Element<'_, Action, Theme> = toolkit::Menu::bar(menu::bar(
            self.busy() || !self.connected,
            self.selected
                .as_ref()
                .is_some_and(|target| self.snapshot.verb(target).is_some()),
            self.dialog.is_some(),
        ))
        .id(menu::BAR_ID)
        .style(t.menu_style())
        .into();
        let base: Element<'_, Message, Theme> = column![
            bar.map(Message::Action),
            container(split).padding(gap).height(iced::Fill),
            container(text(&self.status).size(t.metrics.text.sm)).padding(t.metrics.spacing.sm)
        ]
        .height(iced::Fill)
        .into();
        let Some(dialog) = &self.dialog else {
            return toolkit::dialog::Modal::host(base, None).into();
        };
        let contents = column![
            text(label(if *dialog == Action::About {
                "about"
            } else {
                "shortcuts"
            }))
            .size(t.metrics.text.lg),
            text(label(if *dialog == Action::About {
                "about-body"
            } else {
                "shortcut-body"
            })),
            row![
                widget::space().width(iced::Fill),
                toolkit::CenteredButton::new(text(label("done"))).on_press(Message::Cancel)
            ]
        ]
        .spacing(gap);
        toolkit::dialog::Modal::new(
            base,
            widget::opaque(
                container(
                    container(contents)
                        .padding(t.metrics.spacing.lg)
                        .width(t.metrics.spacing.xl * 22.0)
                        .style(toolkit::theme::container::card),
                )
                .center(iced::Fill),
            ),
        )
        .on_key(|key, _| {
            (*key == iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape))
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
        let mut app = App::new(Settings::default(), Handle::sink(), look);
        app.snapshot.services.insert(
            "example".into(),
            Ok(vec![model::Verb {
                name: "echo".into(),
                args: "value: JSON".into(),
                description: "Echo a value".into(),
                read_only: Some(true),
            }]),
        );
        app.rebuild();
        app
    }
    fn target() -> Selection {
        Selection {
            service: "example".into(),
            verb: "echo".into(),
        }
    }
    #[test]
    fn migrated_runtime_flow_validates_and_sends_once() {
        let mut app = app();
        let _ = app.start_call(target(), "{".into(), None);
        assert!(app.call.is_none());
        let _ = app.start_call(target(), "{\"value\":42}".into(), None);
        let ticket = app.call.as_ref().unwrap().ticket;
        let _ = app.start_call(target(), "{}".into(), None);
        assert_eq!(app.call.as_ref().unwrap().ticket, ticket);
        let _ = app.update(Message::Completed(
            ticket,
            Ok(Reply {
                rc: 10,
                body: "permission denied".into(),
            }),
        ));
        assert!(!app.busy());
        assert_eq!(app.last_reply["rc"], 10);
        assert!(app.reply.text().contains("permission denied"));
        assert_eq!(app.last_reply["request_body"], "{\"value\":42}");
    }
    #[test]
    fn stale_completions_never_clear_new_operation_or_mislabel_reply() {
        let mut app = app();
        let _ = app.start_call(target(), String::new(), None);
        let ticket = app.call.as_ref().unwrap().ticket;
        let _ = app.update(Message::Completed(
            ticket + 1,
            Ok(Reply {
                rc: 0,
                body: "wrong".into(),
            }),
        ));
        assert!(app.call.is_some());
        assert!(app.last_reply.is_null());
        app.selected = None;
        let _ = app.update(Message::Completed(ticket, Err("lost connection".into())));
        assert_eq!(app.last_reply["service"], "example");
        assert_eq!(app.last_reply["retried"], false);
        assert_eq!(app.last_reply["outcome_unknown"], true);
        assert!(app.call.is_none());
    }
    #[test]
    fn discovery_retains_selection_expansion_and_partial_failures() {
        let mut app = app();
        app.selected = Some(target());
        app.expanded.insert("service:example".into());
        let _ = app.refresh(None);
        let ticket = app.discovery.unwrap().0;
        let mut snapshot = app.snapshot.clone();
        snapshot.services.insert(
            "broken".into(),
            Err("HELP and app.describe unavailable".into()),
        );
        let _ = app.update(Message::Discovered(ticket + 1, Snapshot::default()));
        assert!(app.discovery.is_some());
        let _ = app.update(Message::Discovered(ticket, snapshot));
        assert_eq!(app.selected, Some(target()));
        assert!(app.tree.is_expanded(&"service:example".into()));
        assert_eq!(app.snapshot.failures(), 1);
        let _ = app.update(Message::Filter("echo".into()));
        assert!(app.tree.contains(&"verb:example:echo".into()));
        assert!(!app.tree.contains(&"service:broken".into()));
    }
    #[test]
    fn explicit_caller_target_never_silently_falls_back() {
        let mut app = app();
        app.selected = Some(target());
        assert_eq!(app.target(&json!({})).unwrap(), target());
        assert!(app.target(&json!({"service":"other"})).is_err());
        assert!(app.target(&json!({"service":42,"verb":"echo"})).is_err());
        let other = app
            .target(&json!({"service":"other","verb":"echo"}))
            .unwrap();
        let _ = app.start_call(other, "{}".into(), None);
        assert!(app.call.is_none());
    }
    #[test]
    fn modal_blocks_calls_and_quit_finishes_accepted_work() {
        let mut app = app();
        let _ = app.action(Action::About);
        let _ = app.start_call(target(), "{}".into(), None);
        assert!(app.call.is_none());
        let _ = app.action(Action::Quit);
        assert!(!app.quitting);
        let _ = app.update(Message::Cancel);
        let _ = app.start_call(target(), "{}".into(), None);
        let _ = app.quit();
        assert!(app.quitting);
        assert!(app.call.is_some());
    }
    #[test]
    fn simulator_keeps_menu_and_modal_done_reachable_at_minimum_size() {
        let mut app = app();
        let mut sim = application::test::Simulator::with_size(
            iced::Settings::default(),
            iced::Size::new(760.0, 500.0),
            app.view(),
        );
        use iced::keyboard::key::Named;
        sim.tap_key(Named::F10);
        sim.tap_key(Named::ArrowLeft);
        sim.tap_key(Named::ArrowDown);
        sim.tap_key(Named::Enter);
        let messages: Vec<_> = sim.into_messages().collect();
        assert!(
            messages
                .iter()
                .any(|m| matches!(m, Message::Action(Action::About)))
        );
        let _ = app.action(Action::About);
        let mut sim = application::test::Simulator::with_size(
            iced::Settings::default(),
            iced::Size::new(760.0, 500.0),
            app.view(),
        );
        sim.click("Done").unwrap();
        assert!(sim.into_messages().any(|m| matches!(m, Message::Cancel)));
    }
    #[test]
    fn command_replies_expose_identity_and_distinguish_refusals() {
        let mut app = app();
        for (id, verb) in [
            (1, "busviewer.ping"),
            (2, "busviewer.info"),
            (3, "HELP"),
            (4, "app.describe"),
        ] {
            let _ = app.command(id, verb, "{}");
        }
        let replies = app.bus.responses();
        assert_eq!(replies.len(), 4);
        assert!(replies.iter().all(|(_, rc, _)| *rc == 0));
        assert_eq!(replies[0].2["schema"], "busviewer.v1");
        assert_eq!(replies[1].2["app_id"], APP_ID);
        assert!(
            replies[2]
                .2
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["name"] == "busviewer.call")
        );
        let _ = app.command(
            5,
            "busviewer.select",
            r#"{"service":"example","verb":"echo"}"#,
        );
        assert_eq!(app.selected, Some(target()));
        assert_eq!(app.bus.responses().last().unwrap().1, 0);
        let _ = app.command(6, "busviewer.call", r#"{"body":"{"}"#);
        assert_eq!(
            app.bus.responses().last().unwrap().2["error_code"],
            "ARGUMENT"
        );
        assert!(!app.busy());
        let _ = app.action(Action::About);
        let _ = app.command(7, "busviewer.call", "{}");
        assert_eq!(app.bus.responses().last().unwrap().2["error_code"], "BUSY");
        let _ = app.command(8, "no-such-verb", "{}");
        assert_eq!(
            app.bus.responses().last().unwrap().2["error_code"],
            "UNKNOWN_VERB"
        );
        let _ = app.command(9, "busviewer.info", "[]");
        assert_eq!(
            app.bus.responses().last().unwrap().2["error_code"],
            "ARGUMENT"
        );
    }
    #[test]
    fn failed_discovery_retains_last_snapshot_and_registry_events_coalesce() {
        let mut app = app();
        app.selected = Some(target());
        let _ = app.refresh(None);
        let ticket = app.discovery.unwrap().0;
        let _ = app.update(Message::Bus(Delivery::Changed));
        assert!(app.refetch);
        let _ = app.update(Message::Bus(Delivery::Disconnected));
        assert!(!app.connected);
        let _ = app.update(Message::Discovered(
            ticket,
            Snapshot {
                error: Some("noded unavailable".into()),
                ..Snapshot::default()
            },
        ));
        assert_eq!(app.selected, Some(target()));
        assert!(app.snapshot.verb(&target()).is_some());
        assert_eq!(app.status, label("disconnected"));
        assert!(!app.busy());
        let _ = app.update(Message::Bus(Delivery::Connected));
        assert!(app.connected);
        assert!(app.discovery.is_some());
    }
    #[test]
    fn keyboard_respects_modal_and_quit_completes_after_call() {
        use iced::keyboard::{Key, Modifiers, key::Named};
        let mut app = app();
        app.selected = Some(target());
        let _ = app.action(Action::About);
        let _ = app.update(Message::Key(Key::Named(Named::Enter), Modifiers::CTRL));
        assert!(app.call.is_none());
        let _ = app.update(Message::Key(Key::Character("q".into()), Modifiers::CTRL));
        assert!(!app.quitting);
        let _ = app.update(Message::Key(Key::Named(Named::Escape), Modifiers::empty()));
        assert!(app.dialog.is_none());
        let _ = app.update(Message::Key(Key::Named(Named::Enter), Modifiers::CTRL));
        assert!(app.call.is_some());
        let ticket = app.call.as_ref().unwrap().ticket;
        let _ = app.quit();
        assert!(!app.bus.has_quit());
        let _ = app.update(Message::Completed(ticket, Err("lost response".into())));
        assert!(app.bus.has_quit());
        assert!(app.reply.text().contains(&label("transport-error")));
    }
    #[test]
    fn f10_navigates_shared_menu_in_actual_widget_tree() {
        use iced::keyboard::key::Named;
        let app = app();
        let mut ui = application::test::Simulator::with_size(
            iced::Settings::default(),
            iced::Size::new(760.0, 500.0),
            app.view(),
        );
        ui.tap_key(Named::F10);
        ui.tap_key(Named::ArrowLeft);
        ui.tap_key(Named::Enter);
        let messages:Vec<_>=ui.into_messages().collect();
        assert!(messages.iter().any(|m|matches!(m,Message::Action(Action::Shortcuts))),"{messages:?}");
    }
    #[test]
    fn editing_limits_reply_readonly_and_call_freeze_are_enforced() {
        let mut app = app();
        let _ = app.update(Message::Body(text_editor::Action::Edit(
            text_editor::Edit::Paste(std::sync::Arc::new("x".repeat(model::BODY_LIMIT + 1))),
        )));
        assert!(app.body.text().is_empty());
        assert_eq!(app.status, label("body-too-large"));
        app.reply = text_editor::Content::with_text("unchanged");
        let _ = app.update(Message::Reply(text_editor::Action::Edit(
            text_editor::Edit::Insert('x'),
        )));
        assert_eq!(app.reply.text(), "unchanged");
        let _ = app.update(Message::Reply(text_editor::Action::SelectAll));
        let _ = app.start_call(target(), String::new(), None);
        let _ = app.update(Message::Body(text_editor::Action::Edit(
            text_editor::Edit::Insert('x'),
        )));
        assert!(app.body.text().is_empty());
        let _ = app.update(Message::Split(0.9));
        assert_eq!(app.split, 0.65);
    }
}

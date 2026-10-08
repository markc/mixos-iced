// SPDX-License-Identifier: MIT OR Apache-2.0
//! Thin native frontend. Completion snapshots replace state; topics only
//! request a refetch. The prepared appearance is borrowed from the settings
//! session, and settings drains never disturb discovery, calls or edits.
use crate::{
    bus::{self, CallError, Delivery, Handle, Reply},
    menu::{self, Action},
    model::{self, APP_ID, Selection, Snapshot},
    strings::label,
};
use application::iced::{
    self, Element, Subscription, Task,
    widget::{self, column, container, row, text_editor},
    window,
};
use application::presentation::native::Ui;
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
    Completed(u64, Result<Reply, CallError>),
    Shown(u64, Option<bus::Request>, Result<Value, String>),
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
    reply: Option<bus::Request>,
}
pub struct App {
    window: Option<window::Id>,
    settings: Settings,
    bus: Handle,
    bootstrap: appearance::settings::Prepared,
    settings_ui: Ui<()>,
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
    discovery: Option<(u64, Option<bus::Request>)>,
    call: Option<Call>,
    activations: BTreeSet<u64>,
    refetch: bool,
    connected: bool,
    refused: bool,
    handoff_pending: bool,
    subscription_fault: Option<String>,
    launched: std::time::Instant,
    dialog: Option<Action>,
    status: String,
    quitting: bool,
    touched: bool,
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
    #[cfg(feature = "acceptance")]
    let (fixture, initial_task) = crate::acceptance::setup()?;
    #[cfg(feature = "acceptance")]
    let (bus, mut settings_ui, bootstrap, rx) =
        bus::start_fixture(&settings.service, &settings.url, fixture)?;
    #[cfg(not(feature = "acceptance"))]
    let (bus, mut settings_ui, bootstrap, rx) = bus::start(&settings.service, &settings.url)?;
    #[cfg(not(feature = "acceptance"))]
    let initial_task = Task::none();
    let result = (|| {
        STREAM
            .set(Mutex::new(Some(rx)))
            .map_err(|_| "app already started")?;
        let font = bootstrap
            .typography()
            .get("ui")
            .expect("UI typography")
            .font;
        settings_ui.reconcile(bus.settings_generation());
        let app = App::new(settings, bus.clone(), bootstrap, settings_ui);
        // The first refresh is deferred to the reliable registration edge:
        // the worker's Connected delivery, which also arrives when the
        // supervisor registered before this window subscribed.
        application::start(
            (app, initial_task),
            App::update,
            App::view,
            application::Window::desktop(APP_ID, iced::Size::new(1100.0, 760.0), font)
                .defer_close(),
        )
        .title(|_: &App| label("title"))
        .theme(|app: &App| app.look().theme())
        .frame_presentation(App::frame_binding)
        .subscription(App::subscription)
        .run()
        .map_err(|e| e.to_string())
    })();
    bus.quit();
    bus.frames.close();
    let stopped = bus.wait_done();
    result.and(stopped)
}
impl App {
    fn new(
        settings: Settings,
        bus: Handle,
        bootstrap: appearance::settings::Prepared,
        settings_ui: Ui<()>,
    ) -> Self {
        Self {
            window: None,
            settings,
            bus,
            bootstrap,
            settings_ui,
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
            activations: BTreeSet::new(),
            refetch: false,
            // The connection is sampled, never fabricated: the worker's
            // registration edge flips this true. Local operations and the
            // settings drain stay available either way.
            connected: false,
            refused: false,
            handoff_pending: false,
            subscription_fault: None,
            launched: std::time::Instant::now(),
            dialog: None,
            status: label("connecting"),
            quitting: false,
            touched: false,
        }
    }
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
    fn frame_binding(&self) -> Option<application::frames::FrameBinding> {
        self.settings_ui
            .session()
            .frame_stamp()
            .map(|stamp| self.bus.frames.binding(stamp))
    }
    fn publish_frame_target(&self) {
        #[cfg(feature = "acceptance")]
        if let (Some(endpoint), Some(window)) = (&self.bus.fixture_frames, self.window)
            && let Err(error) = endpoint.publish(application::acceptance::frames::Target {
                window,
                stamp: self.settings_ui.session().frame_stamp(),
            })
        {
            eprintln!("busviewer: fixture frame target: {error}");
        }
    }
    fn fixture_root<'a>(
        &self,
        content: Element<'a, Message, Theme>,
    ) -> Element<'a, Message, Theme> {
        #[cfg(feature = "acceptance")]
        if self.bus.fixture_frames.is_some() {
            return container(content)
                .width(iced::Fill)
                .height(iced::Fill)
                .id(crate::acceptance::ROOT_ID)
                .into();
        }
        content
    }
    fn text<'a>(
        &self,
        content: impl iced::advanced::text::IntoFragment<'a>,
    ) -> widget::Text<'a, Theme> {
        self.typography("ui").text(content)
    }
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
        let subscriptions = self
            .subscription_fault
            .as_deref()
            .unwrap_or("subscriptions-ok");
        format!(
            "{} · {} · {}",
            label(kind),
            label(connection),
            label(subscriptions)
        )
    }
    fn busy(&self) -> bool {
        self.discovery.is_some() || self.call.is_some() || !self.activations.is_empty()
    }
    fn next(&mut self) -> u64 {
        self.next_ticket += 1;
        self.next_ticket
    }
    fn info(&self) -> Value {
        json!({"schema":"busviewer.v1","app_id":APP_ID,"version":env!("CARGO_PKG_VERSION"),"pid":std::process::id(),"connected":self.bus.connected(),"busy":self.busy(),"discovering":self.discovery.is_some(),"calling":self.call.is_some(),"selection":self.selected,"body":self.body.text(),"reply":self.last_reply,"status":self.status,"snapshot":self.snapshot,"ui":{"menu_bar":true,"dialog":self.dialog.as_ref().map(|a|format!("{a:?}")),"row_key":self.row_key}})
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
    fn error(&self, id: bus::Request, code: &str, message: &str) {
        self.bus
            .reply(id, 10, json!({"error_code":code,"message":message}));
    }
    /// Settings frames share the bounded GUI channel: every delivery drains
    /// the mailbox against the client's actual sampled generation, so a
    /// coalesced wake is never the only path to the pending events. Draining
    /// never triggers discovery, a refetch or a replay of the pending call,
    /// and local operations stay available offline.
    fn drain_settings(&mut self) {
        let bus = &self.bus;
        if !self
            .settings_ui
            .drain_with(|| bus.settings_generation(), |_| {})
            .is_empty()
        {
            eprintln!(
                "BUSVIEWER_SETTINGS {}",
                json!({
                    "evidence": self.settings_ui.session().host().consumer().evidence(),
                    "settings_cache": self.settings_ui.session().cache_evidence(),
                    "elapsed_ms": self.launched.elapsed().as_millis(),
                })
            );
        }
        self.publish_frame_target();
    }
    fn refresh(&mut self, reply: Option<bus::Request>) -> Task<Message> {
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
    fn start_call(
        &mut self,
        target: Selection,
        body: String,
        reply: Option<bus::Request>,
    ) -> Task<Message> {
        let error = if self.busy() || self.dialog.is_some() || !self.connected || self.quitting {
            Some(label("busy"))
        } else if self.snapshot.verb(&target).is_none() {
            Some(label("invalid-target"))
        } else if body.len() > model::BODY_LIMIT {
            Some(label("body-too-large"))
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
                    Ok(()) => {
                        let formatted = model::pretty(&body);
                        if formatted.len() > model::BODY_LIMIT {
                            self.status = label("body-too-large");
                        } else {
                            self.body = text_editor::Content::with_text(&formatted);
                        }
                    }
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
    fn show(&mut self, id: Option<bus::Request>) -> Task<Message> {
        let ticket = self.next();
        self.activations.insert(ticket);
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
            move |result| Message::Shown(ticket, id.clone(), result),
        )
    }
    fn command(&mut self, id: impl Into<bus::Request>, verb: &str, body: &str) -> Task<Message> {
        let id = id.into();
        if !self.bus.is_current(&id) {
            return Task::none();
        }
        if verb == application::describe::VERB
            && let Err(violation) = application::describe::validate_request(body)
        {
            self.bus.reply(id, 10, model::describe_refusal(&violation));
            return Task::none();
        }
        let parsed = if verb == application::describe::VERB && body.trim().is_empty() {
            Ok(json!({}))
        } else {
            serde_json::from_str::<Value>(body)
        };
        let args = match parsed {
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
            "busviewer.info" => {
                self.settings_ui.reconcile(self.bus.settings_generation());
                self.publish_frame_target();
                let mut info = self.info();
                info["settings"] = json!(self.settings_ui.session().host().consumer().evidence());
                info["settings_cache"] = json!(self.settings_ui.session().cache_evidence());
                self.bus.reply(id, 0, info);
            }
            "HELP" => self.bus.reply(id, 0, model::describe()["verbs"].clone()),
            "app.describe" => {
                self.settings_ui.reconcile(self.bus.settings_generation());
                self.publish_frame_target();
                let mut describe = model::describe();
                let identity = application::describe::Identity {
                    app_id: Some(APP_ID),
                    version: env!("CARGO_PKG_VERSION"),
                    pid: std::process::id(),
                    service: self.bus.service_name(&self.settings.service),
                };
                match application::describe::complete_native_frames(
                    &mut describe,
                    identity,
                    self.settings_ui.session(),
                    &self.bus.frames,
                ) {
                    Ok(()) => self.bus.reply(id, 0, describe),
                    Err(violation) => self.bus.reply(id, 10, model::describe_refusal(&violation)),
                }
            }
            "busviewer.show" if !self.quitting => return self.show(Some(id)),
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
            "busviewer.show" | "busviewer.select" | "busviewer.refresh" | "busviewer.quit" => {
                self.error(id, "BUSY", &label("busy"))
            }
            _ => self.error(id, "UNKNOWN_VERB", "unknown BusViewer verb"),
        }
        Task::none()
    }
    pub fn update(&mut self, message: Message) -> Task<Message> {
        if matches!(
            &message,
            Message::Action(_)
                | Message::OpenMenu(_)
                | Message::Filter(_)
                | Message::Toggle(_)
                | Message::Select(_)
                | Message::Body(_)
                | Message::Reply(_)
                | Message::Split(_)
                | Message::Key(..)
        ) {
            self.touched = true;
        }
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
                let Some((expected, reply)) = self.discovery.clone() else {
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
                        json!({"service":call.target.service,"verb":call.target.verb,"request_body":call.body,"transport_error":error.message,"outcome_unknown":error.outcome_unknown,"retried":false})
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
            Message::Bus(delivery) => {
                self.drain_settings();
                match delivery {
                    Delivery::Command { id, verb, body } => self.command(id, &verb, &body),
                    Delivery::Changed => {
                        if self.busy() {
                            self.refetch = true;
                            Task::none()
                        } else {
                            self.refresh(None)
                        }
                    }
                    Delivery::Settings => Task::none(),
                    Delivery::Notice(message) => {
                        self.subscription_fault = Some(message);
                        Task::none()
                    }
                    Delivery::Refused {
                        name_taken,
                        message,
                    } => {
                        self.refused = true;
                        self.connected = false;
                        self.status = message;
                        // An untouched initial collision keeps the pre-settings
                        // handoff: one forward, and only before this process
                        // ever registered. A later refusal never exits or
                        // forwards; the window stays with its message.
                        if name_taken
                            && !self.bus.ever_registered()
                            && !self.touched
                            && !self.handoff_pending
                        {
                            self.handoff_pending = true;
                            self.bus.forward();
                        }
                        Task::none()
                    }
                    Delivery::Forwarded(result) => {
                        if !self.handoff_pending {
                            return Task::none();
                        }
                        self.handoff_pending = false;
                        match result {
                            Ok(()) if !self.touched && !self.bus.ever_registered() => self.quit(),
                            Ok(()) => Task::none(),
                            Err(error) => {
                                self.status = error;
                                Task::none()
                            }
                        }
                    }
                    Delivery::Connected => {
                        self.connected = true;
                        self.refresh(None)
                    }
                    Delivery::Disconnected => {
                        self.connected = false;
                        self.status = label("disconnected");
                        Task::none()
                    }
                }
            }
            Message::Shown(ticket, id, result) => {
                if !self.activations.remove(&ticket) {
                    return Task::none();
                }
                if let Some(id) = id {
                    match result {
                        Ok(value) => self.bus.reply(id, 0, value),
                        Err(error) => self.error(id, "ACTIVATION", &error),
                    }
                }
                self.followup()
            }
            Message::Window(id, window::Event::Opened { .. }) => {
                if self.window.is_some_and(|window| window != id) {
                    self.bus.frames.close();
                } else {
                    self.window = Some(id);
                    self.publish_frame_target();
                }
                Task::none()
            }
            Message::Window(id, window::Event::Closed) if self.window == Some(id) => {
                self.bus.frames.close();
                Task::none()
            }
            Message::Window(id, window::Event::CloseRequested) if self.window == Some(id) => {
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
        let t = self.look().tokens();
        let gap = t.metrics.spacing.md;
        let selected = self
            .row_key
            .as_ref()
            .and_then(|key| self.tree.position(key))
            .map(toolkit::Selection::single)
            .unwrap_or_default();
        let ui_style = self.typography("ui");
        let tree = toolkit::TreeView::new(&self.tree, move |row| {
            ui_style.text(match row.data {
                Row::Service(name) | Row::Peer(name) => name.clone(),
                Row::Error(_) => label("descriptions-failed"),
                Row::Verb(target) => target.verb.clone(),
                Row::Peers => label("peers"),
            })
        })
        .expanders(toolkit::tree::Expanders::new(None, None))
        .on_toggle(Message::Toggle)
        .on_select(Message::Select)
        .selection(&selected)
        .metrics(toolkit::controls::Metrics::from_tokens(t), ui_style);
        let left = column![
            self.text(label("services")).size(t.metrics.text.lg),
            toolkit::TextField::new(&label("search"), &self.filter)
                .on_input(Message::Filter)
                .id("busviewer-search")
                .text_style(ui_style),
            tree
        ]
        .spacing(gap)
        .height(iced::Fill);
        let right = widget::responsive(move |bounds| -> Element<'_, Message, Theme> {
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
        let mono = self.typography("mono");
        // Keep real text sizes. A compact inspector scrolls between finite
        // editor viewports; a larger pane preserves the extra reply space.
        let editor_height = (t.metrics.text.md * 7.0)
            .min((bounds.height - gap * 2.0).max(t.metrics.text.md * 3.0));
        let reply_height = editor_height.max(bounds.height
            - t.metrics.text.md * 11.0 - editor_height - gap * 4.0);
        let mut body = text_editor(&self.body)
            .on_action(Message::Body)
            .placeholder(label("body"))
            .height(editor_height)
            .font(mono.font)
            .size(mono.size);
        if let Some(height) = mono.line_height {
            body = body.line_height(iced::advanced::text::LineHeight::Absolute(iced::Pixels(
                height,
            )));
        }
        #[cfg(feature = "acceptance")]
        if self.bus.fixture_frames.is_some() {
            body = body.id(crate::acceptance::BODY_ID);
        }
        let mut reply = text_editor(&self.reply)
            .on_action(Message::Reply)
            .height(reply_height)
            .font(mono.font)
            .size(mono.size);
        if let Some(height) = mono.line_height {
            reply = reply.line_height(iced::advanced::text::LineHeight::Absolute(iced::Pixels(
                height,
            )));
        }
        #[cfg(feature = "acceptance")]
        if self.bus.fixture_frames.is_some() {
            reply = reply.id(crate::acceptance::REPLY_ID);
        }
        widget::scrollable(column![
            widget::scrollable(self.text(details)).height(t.metrics.text.md * 9.0),
            self.text(label("body")),
            body,
            self.text(label("reply")),
            reply
        ]
        .spacing(gap)
        // Leave a normal trailing gutter so integral scroll translations
        // can reveal the final editor's complete fractional-size border.
        .padding(iced::Padding { bottom: gap, ..Default::default() }))
        .id("busviewer-inspector")
        .height(iced::Fill)
        .width(iced::Fill)
        .into()
        });
        let split = toolkit::Split::new(self.split, left, right).on_drag(Message::Split);
        let bar: Element<'_, Action, Theme> = toolkit::Menu::bar(menu::bar(
            self.busy() || !self.connected,
            self.selected
                .as_ref()
                .is_some_and(|target| self.snapshot.verb(target).is_some()),
            self.dialog.is_some(),
        ))
        .id(menu::BAR_ID)
        .text_style(ui_style)
        .style(t.menu_style())
        .into();
        let base: Element<'_, Message, Theme> = column![
            bar.map(Message::Action),
            container(split).padding(gap).height(iced::Fill),
            container(
                column![
                    self.text(&self.status).size(t.metrics.text.sm),
                    self.text(self.persistent_status()).size(t.metrics.text.sm)
                ]
                .spacing(t.metrics.spacing.sm)
            )
            .padding(t.metrics.spacing.sm)
        ]
        .height(iced::Fill)
        .into();
        let Some(dialog) = &self.dialog else {
            return self.fixture_root(toolkit::dialog::Modal::host(base, None).into());
        };
        let contents = column![
            self.text(label(if *dialog == Action::About {
                "about"
            } else {
                "shortcuts"
            }))
            .size(t.metrics.text.lg),
            self.text(label(if *dialog == Action::About {
                "about-body"
            } else {
                "shortcut-body"
            })),
            row![
                widget::space().width(iced::Fill),
                toolkit::CenteredButton::new(self.text(label("done"))).on_press(Message::Cancel)
            ]
        ]
        .spacing(gap);
        self.fixture_root(
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
            .into(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use application::presentation::native::{Event as SettingsEvent, Progress};
    fn app() -> App {
        let consumer = settings::consumer::Consumer::for_app(
            settings::Binding {
                instance: "fixture".into(),
                profile: "default".into(),
            },
            "busviewer",
        )
        .unwrap();
        let (ui, _lane) = application::presentation::native::bridge(
            application::presentation::native::Session::new(consumer),
            application::presentation::native::Worker::offline(|_, _| Ok(())),
        );
        let mut app = App::new(
            Settings::default(),
            Handle::sink(),
            appearance::settings::bootstrap().unwrap(),
            ui,
        );
        app.connected = true;
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
    fn compact_desktop_keeps_discovery_and_modal_actions_visible() {
        for size in [iced::Size::new(420.0, 240.0), iced::Size::new(458.0, 346.0)] {
            let mut app = app();
            app.bootstrap = application::test::desktop_presentation(1.25);
            #[cfg(feature = "acceptance")]
            {
                app.bus.fixture_frames = Some(application::acceptance::frames::Endpoint::new(app.bus.frames.clone()));
            }
            let mut sim = application::test::Simulator::with_size(
                iced::Settings::default(), size, app.view(),
            );
            let search = sim.find(widget::Id::new("busviewer-search")).expect("discovery search");
            assert!(search.visible_bounds().is_some(), "visible search");
            application::test::assert_visible_bounds(search.bounds(), size);
            let service = sim.find("example").expect("discovered service");
            assert!(service.visible_bounds().is_some(), "visible service");
            application::test::assert_visible_bounds(service.bounds(), size);
            for text in [label("body"), label("reply")] {
                let mut reached = false;
                for step in 0..100 {
                    let control = sim.find(text.clone()).expect("primary call editor label");
                    let bounds = control.bounds();
                    if step == 0 || step == 99 {
                        eprintln!("compact size={size:?} label={text:?} bounds={bounds:?} visible={:?}", control.visible_bounds());
                        eprintln!("inspector={:?}", sim.find(widget::Id::new("busviewer-inspector")));
                    }
                    // Rectangle intersection subtracts translated f32 endpoints.
                    // Allow arithmetic round-off, never a clipped logical pixel.
                    if let Some(visible) = control.visible_bounds().filter(|visible|
                        (visible.width - bounds.width).abs() <= 0.001
                        && (visible.height - bounds.height).abs() <= 0.001
                    ) {
                        application::test::assert_visible_bounds(visible, size);
                        reached = true;
                        break;
                    }
                    sim.point_at(iced::Point::new(size.width * 0.8, size.height * 0.4));
                    let statuses = sim.simulate([iced::Event::Mouse(iced::mouse::Event::WheelScrolled {
                        delta: iced::mouse::ScrollDelta::Pixels { x: 0.0, y: -8.0 },
                    })]);
                    if step == 0 || step == 99 { eprintln!("wheel status={statuses:?}"); }
                }
                assert!(reached, "{text} must remain reachable by actual inspector scrolling");
            }
            #[cfg(feature = "acceptance")]
            for id in [crate::acceptance::BODY_ID, crate::acceptance::REPLY_ID] {
                let mut reached = false;
                for step in 0..100 {
                    let control = sim.find(widget::Id::from(id)).expect("actual editor viewport");
                    let bounds = control.bounds();
                    if let Some(visible) = control.visible_bounds().filter(|visible|
                        (visible.width - bounds.width).abs() <= 0.001
                        && (visible.height - bounds.height).abs() <= 0.001
                    ) {
                        application::test::assert_visible_bounds(visible, size);
                        reached = true;
                        break;
                    }
                    let inspector = sim.find(widget::Id::new("busviewer-inspector")).expect("owning inspector viewport");
                    let application::test::selector::Target::Scrollable { translation, bounds: viewport, .. } = inspector else {
                        panic!("inspector must retain the native scrollable contract");
                    };
                    if step == 0 || step == 99 {
                        eprintln!("editor={id} bounds={bounds:?} visible={:?} viewport={viewport:?} translation={translation:?}", control.visible_bounds());
                    }
                    sim.point_at(iced::Point::new(size.width * 0.8, size.height * 0.4));
                    let direction = if bounds.center_y() - translation.y < viewport.center_y() { 8.0 } else { -8.0 };
                    sim.simulate([iced::Event::Mouse(iced::mouse::Event::WheelScrolled {
                        delta: iced::mouse::ScrollDelta::Pixels { x: 0.0, y: direction },
                    })]);
                }
                assert!(reached, "{id} must retain a whole usable editor viewport");
                sim.click(widget::Id::from(id)).expect("reachable editor can receive focus");
                sim.typewrite("native");
            }
            #[cfg(feature = "acceptance")]
            {
                let messages: Vec<_> = sim.into_messages().collect();
                assert!(messages.iter().any(|message| matches!(message, Message::Body(_))));
                assert!(messages.iter().any(|message| matches!(message, Message::Reply(_))));
            }
            #[cfg(not(feature = "acceptance"))]
            drop(sim);
            let _ = app.action(Action::About);
            let mut sim = application::test::Simulator::with_size(
                iced::Settings::default(), size, app.view(),
            );
            let done = sim.find("Done").expect("modal completion");
            assert!(done.visible_bounds().is_some(), "visible completion");
            application::test::assert_visible_bounds(done.bounds(), size);
            sim.click("Done").expect("modal completion remains usable");
            assert!(sim.into_messages().any(|message| matches!(message, Message::Cancel)));
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
        let ticket = app.discovery.as_ref().unwrap().0;
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
            replies[1].2["settings"]["kind"].is_null(),
            "no presentation kind before any activation"
        );
        assert!(
            replies[1].2["settings_cache"].is_object(),
            "busviewer.info carries cache evidence"
        );
        assert!(
            replies[2]
                .2
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["name"] == "busviewer.call")
        );
        assert!(
            replies[3].2["settings"]["kind"].is_null(),
            "no presentation kind before any activation"
        );
        assert!(
            replies[3].2["settings_cache"].is_object(),
            "app.describe carries cache evidence"
        );
        application::describe::validate(&replies[3].2).unwrap();
        assert_eq!(replies[3].2["pid"], std::process::id());
        assert_eq!(replies[3].2["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(replies[3].2["service"], app.settings.service);
        assert!(replies[3].2["resources"].is_null());
        assert_eq!(replies[3].2["preparation"]["current"], false);
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
    fn canonical_description_requests_are_pure_and_raw_bounds_are_enforced() {
        let mut app = app();
        app.settings.service = "busviewer-custom".into();
        let before = app.info();
        let preparation =
            serde_json::to_value(app.settings_ui.session().preparation_evidence()).unwrap();
        let oversized = " ".repeat(application::describe::MAX_REQUEST_BYTES + 1);
        for (index, body) in [
            "",
            " \n ",
            "{}",
            "{",
            "[]",
            "null",
            r#"{"x":1}"#,
            oversized.as_str(),
        ]
        .into_iter()
        .enumerate()
        {
            let _ = app.command(index as u64 + 1, "app.describe", body);
            let responses = app.bus.responses();
            let (_, rc, reply) = responses.last().unwrap();
            if index < 3 {
                assert_eq!(*rc, 0);
                application::describe::validate(reply).unwrap();
                assert_eq!(reply["service"], "busviewer-custom");
            } else {
                assert_eq!(*rc, 10);
            }
            assert_eq!(app.info(), before);
            assert_eq!(
                serde_json::to_value(app.settings_ui.session().preparation_evidence()).unwrap(),
                preparation
            );
            assert!(app.settings_ui.session().frame_stamp().is_none());
        }
    }
    #[test]
    fn failed_discovery_retains_last_snapshot_and_registry_events_coalesce() {
        let mut app = app();
        app.selected = Some(target());
        let _ = app.refresh(None);
        let ticket = app.discovery.as_ref().unwrap().0;
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
        let messages: Vec<_> = ui.into_messages().collect();
        assert!(
            messages
                .iter()
                .any(|m| matches!(m, Message::Action(Action::Shortcuts))),
            "{messages:?}"
        );
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
    #[test]
    fn accepted_agent_calls_reply_once_and_quit_is_acknowledged() {
        let mut app = app();
        app.selected = Some(target());
        let _ = app.command(80, "busviewer.call", "{}");
        assert!(app.bus.responses().is_empty());
        let ticket = app.call.as_ref().unwrap().ticket;
        let _ = app.update(Message::Completed(
            ticket,
            Ok(Reply {
                rc: 10,
                body: "permission denied".into(),
            }),
        ));
        let replies = app.bus.responses();
        assert_eq!(replies.len(), 1);
        assert_eq!((replies[0].0, replies[0].1), (80, 0));
        assert_eq!(replies[0].2["rc"], 10);
        let _ = app.update(Message::Completed(ticket, Err("late duplicate".into())));
        assert_eq!(app.bus.responses().len(), 1);
        let _ = app.command(81, "busviewer.quit", "{}");
        assert_eq!(app.bus.responses().last().unwrap().2["quitting"], true);
        assert!(app.bus.has_quit());
        let _ = app.command(82, "busviewer.show", "{}");
        assert_eq!(app.bus.responses().last().unwrap().2["error_code"], "BUSY");
    }
    #[test]
    fn activation_is_fenced_and_shutdown_waits_for_its_reply() {
        let mut app = app();
        let _ = app.command(90, "busviewer.show", "{}");
        let ticket = *app.activations.first().unwrap();
        assert!(app.busy());
        let _ = app.command(91, "busviewer.quit", "{}");
        assert_eq!(app.bus.responses().last().unwrap().2["error_code"], "BUSY");
        let _ = app.quit();
        assert!(!app.bus.has_quit());
        let _ = app.update(Message::Shown(ticket + 1, Some(90.into()), Ok(json!({}))));
        assert!(app.busy());
        let _ = app.update(Message::Shown(
            ticket,
            Some(90.into()),
            Err("compd disconnected".into()),
        ));
        assert_eq!(app.bus.responses().last().unwrap().0, 90);
        assert_eq!(
            app.bus.responses().last().unwrap().2["error_code"],
            "ACTIVATION"
        );
        assert!(app.bus.has_quit());
        let replies = app.bus.responses().len();
        let _ = app.update(Message::Shown(ticket, Some(90.into()), Ok(json!({}))));
        assert_eq!(app.bus.responses().len(), replies);
    }
    #[test]
    fn formatting_never_expands_body_past_the_input_limit() {
        let mut app = app();
        let body = format!("[{}]", vec!["0"; 16_000].join(","));
        assert!(body.len() < model::BODY_LIMIT);
        assert!(model::pretty(&body).len() > model::BODY_LIMIT);
        app.body = text_editor::Content::with_text(&body);
        let _ = app.action(Action::Format);
        assert_eq!(app.body.text(), body);
        assert_eq!(app.status, label("body-too-large"));
        app.body = text_editor::Content::with_text("{\"a\":1}");
        let _ = app.action(Action::Format);
        assert_eq!(app.body.text(), "{\n  \"a\": 1\n}");
        let _ = app.action(Action::Clear);
        assert!(app.body.text().is_empty());
    }
    #[test]
    fn agent_lost_reply_is_an_uncertain_error_without_retry() {
        let mut app = app();
        app.selected = Some(target());
        let _ = app.command(92, "busviewer.call", "{}");
        let ticket = app.call.as_ref().unwrap().ticket;
        let _ = app.update(Message::Completed(ticket, Err("connection lost".into())));
        let replies = app.bus.responses();
        assert_eq!((replies[0].0, replies[0].1), (92, 10));
        assert_eq!(replies[0].2["transport_error"], "connection lost");
        assert_eq!(replies[0].2["outcome_unknown"], true);
        assert_eq!(replies[0].2["retried"], false);
    }
    /// Install the built-in licensed Inter font into both the sans and mono
    /// slots so the real prepare step resolves every typography role
    /// deterministically, without relying on undocumented host fonts.
    fn install_test_fonts() {
        static INSTALLED: std::sync::Once = std::sync::Once::new();
        INSTALLED.call_once(|| {
            let inter =
                include_bytes!("../../../vendor/font/Inter-VariableFont_opsz,wght.ttf").as_slice();
            toolkit::fonts::install(toolkit::fonts::FontSet::new().sans(inter).mono(inter), None)
                .expect("built-in licensed test font installs");
        });
    }
    /// A real prepared-presentation activation: driving the shared lane/worker
    /// pipeline through the app's own session rethemes the look (font, colour,
    /// geometry) while the accepted call, editor contents, cursors, filter,
    /// tree and split survive — and it never triggers a refetch or a replay.
    #[test]
    fn settings_activation_rethemes_without_disturbing_editor_state() {
        install_test_fonts();
        let binding = settings::Binding {
            instance: "fixture".into(),
            profile: "default".into(),
        };
        let consumer = settings::consumer::Consumer::for_app(binding.clone(), "busviewer").unwrap();
        let (ui, mut lane) = application::presentation::native::bridge(
            application::presentation::native::Session::new(consumer),
            application::presentation::native::Worker::offline(|_, _| Ok(())),
        );
        let mut app = App::new(
            Settings::default(),
            Handle::sink_with_generation(Some(1)),
            appearance::settings::bootstrap().unwrap(),
            ui,
        );
        app.connected = true;
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
        app.selected = Some(target());
        app.body = text_editor::Content::with_text("{\"value\":42}");
        app.reply = text_editor::Content::with_text("first reply");
        app.reply.perform(text_editor::Action::SelectAll);
        let _ = app.start_call(target(), "{\"value\":42}".into(), None);
        let ticket = app.call.as_ref().unwrap().ticket;
        let _ = app.update(Message::Filter("echo".into()));
        let _ = app.update(Message::Split(0.5));
        let before_font = app.look().typography().get("ui").unwrap().font;
        let before_surface = app.look().tokens().palette.surface;
        let before_metrics = app.look().tokens().metrics.text.md;
        let body = app.body.text();
        let reply_text = app.reply.text();
        let reply_selection = app.reply.selection();
        let reply_cursor = app.reply.cursor();
        let rows = app.tree.len();

        // Drive the real read pipeline through the app's own session, then run
        // the shared worker until the prepared presentation is published.
        let _ = app
            .settings_ui
            .handle_with(SettingsEvent::Wake, Some(1), |_| {});
        let subscribe = app
            .settings_ui
            .session()
            .host()
            .consumer()
            .current_work()
            .expect("subscribe work")
            .clone();
        let _ =
            app.settings_ui
                .handle_with(SettingsEvent::Rpc(subscribe, Ok(None)), Some(1), |_| {});
        let _ = app
            .settings_ui
            .handle_with(SettingsEvent::Wake, Some(1), |_| {});
        let read = app
            .settings_ui
            .session()
            .host()
            .consumer()
            .current_work()
            .expect("read work")
            .clone();
        let _ = app.settings_ui.handle_with(
            SettingsEvent::Rpc(
                read,
                Ok(Some(crate::bus::settings_snapshot(&binding, true, 1.5))),
            ),
            Some(1),
            |_| {},
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    match lane.drive().await {
                        Progress::Wake => {
                            let _ = app.update(Message::Bus(Delivery::Settings));
                        }
                        Progress::Updated => {}
                        Progress::UiClosed => panic!("test UI closed"),
                    }
                    if app.settings_ui.preparation_evidence().current
                        && app
                            .settings_ui
                            .session()
                            .host()
                            .consumer()
                            .applied()
                            .is_some_and(|applied| applied.revision == settings::Revision(2))
                    {
                        break;
                    }
                }
            })
            .await
            .expect("actual authority activation rather than first fallback wake");
        });
        // The app's own settings drain activates the prepared presentation.
        let _ = app.update(Message::Bus(Delivery::Settings));
        assert!(
            app.settings_ui.session().host().presentation().is_some(),
            "activation occurred"
        );
        assert_eq!(
            app.settings_ui.session().host().kind(),
            Some(settings::fallback::PresentationKind::Current),
            "the authority presentation is current"
        );
        assert_eq!(
            serde_json::json!(app.settings_ui.session().host().consumer().evidence())["kind"],
            "current",
            "the canonical kind string after activation"
        );
        assert_ne!(
            app.look().typography().get("ui").unwrap().font,
            before_font,
            "activated font"
        );
        assert_ne!(
            app.look().tokens().palette.surface,
            before_surface,
            "activated colour"
        );
        assert_ne!(
            app.look().tokens().metrics.text.md,
            before_metrics,
            "activated geometry"
        );
        assert_eq!(app.call.as_ref().unwrap().ticket, ticket);
        assert_eq!(app.call.as_ref().unwrap().body, "{\"value\":42}");
        assert_eq!(app.body.text(), body);
        assert_eq!(app.reply.text(), reply_text);
        assert_eq!(app.reply.selection(), reply_selection);
        assert_eq!(app.reply.cursor(), reply_cursor);
        assert_eq!(app.tree.len(), rows);
        assert_eq!(app.selected, Some(target()));
        assert_eq!(app.split, 0.5);
        assert!(app.last_reply.is_null());
        assert!(!app.refetch, "settings must never refetch or replay a call");
        assert!(app.discovery.is_none());
    }
    /// An untouched initial collision forwards exactly once and closes only on
    /// the matching completion; a touched window never hands off, and a stale
    /// completion never closes it.
    #[test]
    fn refused_collision_handoff_is_fenced_and_touch_blocks_it() {
        let mut app = app();
        let _ = app.update(Message::Bus(Delivery::Refused {
            name_taken: true,
            message: "already registered".into(),
        }));
        assert!(app.refused);
        assert!(!app.connected);
        assert!(app.handoff_pending);
        assert!(!app.quitting);
        assert_eq!(app.bus.forward_count(), 1);
        let _ = app.update(Message::Bus(Delivery::Refused {
            name_taken: true,
            message: "again".into(),
        }));
        assert_eq!(app.bus.forward_count(), 1, "one forward per collision");
        let _ = app.update(Message::Bus(Delivery::Forwarded(Err(
            "target disappeared".into()
        ))));
        assert!(!app.quitting);
        assert_eq!(app.status, "target disappeared");
        let _ = app.update(Message::Bus(Delivery::Forwarded(Ok(()))));
        assert!(
            !app.quitting,
            "a stale completion must not close the window"
        );
        let mut untouched = super::tests::app();
        let _ = untouched.update(Message::Bus(Delivery::Refused {
            name_taken: true,
            message: "already registered".into(),
        }));
        let _ = untouched.update(Message::Bus(Delivery::Forwarded(Ok(()))));
        assert!(untouched.quitting);
        let mut touched = super::tests::app();
        let _ = touched.update(Message::Filter("echo".into()));
        let _ = touched.update(Message::Bus(Delivery::Refused {
            name_taken: true,
            message: "already registered".into(),
        }));
        assert!(!touched.handoff_pending, "a touched window never hands off");
        assert!(!touched.quitting);
        assert_eq!(touched.bus.forward_count(), 0);
    }
}

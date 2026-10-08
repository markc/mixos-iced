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
    self, Subscription, Task,
    widget::{column, container, row},
    window,
};
use application::presentation::native::Ui;
use application::{Element, widget};
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
    bootstrap: appearance::settings::Prepared,
    settings_ui: Ui<()>,
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
    touched: bool,
    refused: bool,
    handoff_pending: bool,
    launch_selection: Selection,
    launched: std::time::Instant,
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
    let (bus, mut settings_ui, bootstrap, rx) =
        bus::start(&settings.service, &settings.url, &settings.host)?;
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
        let app = App::new(settings, bus.clone(), bootstrap, settings_ui, selection);
        application::start(
            (app, Task::none()),
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
    bus.wait_done();
    result
}
impl App {
    fn new(
        settings: Settings,
        bus: Handle,
        bootstrap: appearance::settings::Prepared,
        settings_ui: Ui<()>,
        selection: Selection,
    ) -> Self {
        Self {
            settings,
            bus,
            bootstrap,
            settings_ui,
            launch_selection: selection.clone(),
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
            touched: false,
            refused: false,
            handoff_pending: false,
            launched: std::time::Instant::now(),
            quitting: false,
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
        format!("{} · {}", label(kind), label(connection))
    }
    fn context(&self) -> menu::Context<'_> {
        menu::Context {
            selection: &self.selection,
            snapshot: &self.snapshot,
            edge: self.edge,
            busy: self.operation.is_some() || !self.bus.connected(),
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
    /// Read-only confirmation diagnostic: the action request, captured
    /// selection and captured state token a pending `Dialog::Confirm` froze,
    /// so an acceptance can prove those values survived without a mutation verb.
    fn confirmation(&self) -> Option<Value> {
        let Dialog::Confirm {
            action,
            selection,
            token,
        } = self.dialog.as_ref()?
        else {
            return None;
        };
        Some(json!({
            "action": action.request(),
            "selection": selection,
            "state_token": token,
        }))
    }
    fn describe(&self) -> Result<Value, application::describe::Violation> {
        let mut value = model::describe();
        application::describe::complete_native(
            &mut value,
            application::describe::Identity {
                app_id: Some(APP_ID),
                version: env!("CARGO_PKG_VERSION"),
                pid: std::process::id(),
                service: self.bus.service_name(),
            },
            self.settings_ui.session(),
        )?;
        Ok(value)
    }
    fn info(&self) -> Value {
        json!({"schema":"scene-editor.v1","app_id":APP_ID,"version":env!("CARGO_PKG_VERSION"),"pid":std::process::id(),"connected":self.bus.connected(),"busy":self.operation.is_some(),"selection":self.selection,"edge":self.edge,"epoch":self.epoch,"status":self.status,"state_token":self.snapshot.0["state_token"],"ui":{"menu_bar":true,"dialog":match self.dialog{Some(Dialog::Confirm{..})=>Some("confirm"),Some(Dialog::Shortcuts)=>Some("shortcuts"),Some(Dialog::About)=>Some("about"),None=>None},"confirmation":self.confirmation()},"snapshot":self.snapshot.0})
    }
    fn start(&mut self, verb: &str, args: Value, kind: Kind, reply: Option<u64>) -> Task<Message> {
        if !self.bus.connected() {
            self.status = label("waiting");
            if let Some(id) = reply {
                self.reply_error(id, "TRANSPORT", "Bus is disconnected");
            }
            return Task::none();
        }
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
        self.touched = true;
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
                if self
                    .selection
                    .page
                    .as_ref()
                    .is_some_and(|page| page.edge != edge)
                {
                    self.selection.page = None;
                }
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
        self.quitting = true;
        if self.operation.is_some() {
            self.quitting = true;
            return Task::none();
        }
        self.bus.quit();
        iced::exit()
    }
    fn select(&mut self, selection: Selection) -> Task<Message> {
        self.touched = true;
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
        if verb == "app.describe"
            && let Err(error) = application::describe::validate_request(body)
        {
            self.bus.reply(id, 10, crate::bus::describe_refusal(&error));
            return Task::none();
        }
        let body = if verb == "app.describe" && body.trim().is_empty() {
            "{}"
        } else {
            body
        };
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
                self.settings_ui.reconcile(self.bus.settings_generation());
                self.bus.reply(id, 0, self.info());
                Task::none()
            }
            "app.describe" => {
                self.settings_ui.reconcile(self.bus.settings_generation());
                match self.describe() {
                    Ok(describe) => self.bus.reply(id, 0, describe),
                    Err(error) => self.bus.reply(id, 10, crate::bus::describe_refusal(&error)),
                }
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
                if selection.view != View::Arrange {
                    selection.page = None;
                }
                let refresh = if navigate {
                    self.select(selection)
                } else {
                    Task::none()
                };
                Task::batch([refresh, self.show(Some(id))])
            }
            "scene-editor.action" => {
                if !self.bus.connected() {
                    self.status = label("waiting");
                    self.reply_error(id, "TRANSPORT", "Bus is disconnected");
                    return Task::none();
                }
                if self.dialog.is_some() || self.operation.is_some() {
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
        if matches!(
            &message,
            Message::Action(_)
                | Message::OpenMenu(_)
                | Message::SelectTemplate(_)
                | Message::SelectScene(_)
                | Message::SelectPage(..)
                | Message::Key(..)
                | Message::Confirm
                | Message::Cancel
        ) {
            self.touched = true;
        }
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
            Message::Bus(Delivery::Settings) => {
                let bus = &self.bus;
                if !self
                    .settings_ui
                    .drain_with(|| bus.settings_generation(), |_| {})
                    .is_empty()
                {
                    eprintln!(
                        "SCENE_EDITOR_SETTINGS {}",
                        json!({
                            "evidence": self.settings_ui.session().host().consumer().evidence(),
                            "settings_cache": self.settings_ui.session().cache_evidence(),
                            "elapsed_ms": self.launched.elapsed().as_millis(),
                        })
                    );
                }
                Task::none()
            }
            Message::Bus(Delivery::Refused {
                name_taken,
                message,
            }) => {
                self.refused = true;
                self.status = message;
                if name_taken
                    && !self.bus.ever_registered()
                    && !self.touched
                    && !self.handoff_pending
                {
                    self.handoff_pending = true;
                    self.bus.forward_selection(self.launch_selection.clone());
                }
                Task::none()
            }
            Message::Bus(Delivery::Forwarded(result)) => {
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
            Message::Bus(Delivery::Connected) => self.refresh(),
            Message::Bus(Delivery::Disconnected) => {
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
        let t = self.look().tokens();
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
            container(self.text(label(if gallery {
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
            .font(self.typography("ui").font)
            .text_size(self.typography("ui").size)
            .padding(gap)
            .height(iced::Fill)
            .into()
        };
        let mut details = column![self.text(label(if gallery {
            "select-template"
        } else {
            "select-scene"
        }))]
        .spacing(gap)
        .width(iced::Fill);
        let selected = &self.snapshot.model()[if gallery { "tpl" } else { "selected" }];
        if selected["shown"] == true {
            details = details.push(self.text(string(&selected["title"])));
            if gallery {
                for field in ["desc", "needs", "installed"] {
                    details = details.push(self.text(string(&selected[field])));
                }
            } else {
                details = details.push(self.text(string(&selected["status"])));
                if let Some(scene) = self.snapshot.scene(&self.selection) {
                    details = details.push(self.text(label("files")));
                    for field in ["scene", "behaviour", "metadata"] {
                        if let Some(path) = scene["files"][field].as_str() {
                            details = details.push(self.typography("mono").text(path));
                        }
                    }
                }
                if !rows(&selected["problems"]).is_empty() {
                    details = details.push(self.text(label("problems")));
                }
                for problem in rows(&selected["problems"]) {
                    details = details.push(self.text(string(&problem["cells"][0])));
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
        let t = self.look().tokens();
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
            .font(self.typography("ui").font)
            .text_size(self.typography("ui").size)
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
            .font(self.typography("ui").font)
            .text_size(self.typography("ui").size)
            .padding(gap)
            .into();
        row![
            column![self.text(label("edge")), edges].width(iced::FillPortion(2)),
            column![self.text(label("select-page")), pages]
                .spacing(gap)
                .width(iced::FillPortion(3))
        ]
        .spacing(gap)
        .height(iced::Fill)
        .into()
    }
    pub fn view(&self) -> Element<'_, Message, Theme> {
        let t = self.look().tokens();
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
                container(self.text(string(&self.snapshot.model()["banner"]["text"])))
                    .padding(gap)
                    .style(toolkit::theme::container::card),
            );
        }
        let base: Element<'_, Message, Theme> = body
            .push(container(content).padding(gap).height(iced::Fill))
            .push(
                container(
                    row![
                        column![self.text(&self.status), self.text(self.persistent_status())],
                        widget::space().width(iced::Fill),
                        self.text(label(self.selection.view.key()))
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
                    .push(self.text(label(action.confirmation().expect("confirm action"))))
                    .push(self.text(selection.scene.as_deref().unwrap_or_default()));
                controls = controls.push(
                    toolkit::CenteredButton::new(self.text(label("cancel")))
                        .on_press(Message::Cancel),
                );
                let button = toolkit::CenteredButton::new(self.text(label("confirm")));
                controls = controls.push(if self.operation.is_none() {
                    button.on_press(Message::Confirm)
                } else {
                    button
                });
            }
            Dialog::Shortcuts => {
                contents = contents
                    .push(self.text(label("shortcuts")))
                    .push(self.text(label("shortcut-body")));
                controls = controls.push(
                    toolkit::CenteredButton::new(self.text(label("done")))
                        .on_press(Message::Cancel),
                );
            }
            Dialog::About => {
                contents = contents
                    .push(self.text(format!("{} {}", label("title"), env!("CARGO_PKG_VERSION"))))
                    .push(self.text(label("about-body")));
                controls = controls.push(
                    toolkit::CenteredButton::new(self.text(label("done")))
                        .on_press(Message::Cancel),
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
        let consumer = settings::consumer::Consumer::for_app(
            settings::Binding {
                instance: "fixture".into(),
                profile: "default".into(),
            },
            "scene-editor",
        )
        .unwrap();
        let (ui, _lane) = application::presentation::native::bridge(
            application::presentation::native::Session::new(consumer),
            application::presentation::native::Worker::offline(|_, _| Ok(())),
        );
        App::new(
            Settings::default(),
            Handle::sink(),
            appearance::settings::bootstrap().unwrap(),
            ui,
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
    fn canonical_description_reads_the_installed_owner_without_editing() {
        let mut app = app();
        app.selection.scene = Some("unfinished-selection".into());
        let selection = app.selection.clone();
        let preparation =
            serde_json::to_value(app.settings_ui.session().preparation_evidence()).unwrap();
        let stamp = app.settings_ui.session().frame_stamp();
        let first = app.describe().unwrap();
        application::describe::validate(&first).unwrap();
        assert_eq!(first["pid"], std::process::id());
        assert_eq!(first["service"], "scene-editor");
        assert_eq!(first["app_id"], APP_ID);
        assert!(first["resources"].is_null());
        assert!(first["settings_cache"].is_object());
        assert_eq!(first["views"], model::describe()["views"]);
        assert_eq!(first, app.describe().unwrap());
        assert_eq!(app.selection, selection);
        assert_eq!(app.settings_ui.session().frame_stamp(), stamp);
        assert_eq!(
            serde_json::to_value(app.settings_ui.session().preparation_evidence()).unwrap(),
            preparation
        );
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
        assert_eq!(
            app.confirmation(),
            Some(json!({
                "action":{"action":"reset"},
                "selection":{"view":"gallery","template":null,"scene":"panel","page":null},
                "state_token":"a".repeat(64),
            })),
            "the read-only diagnostic reports the frozen action, selection and token"
        );
        let mut ui = application::test::Simulator::with_size(
            iced::Settings::default(),
            iced::Size::new(760.0, 450.0),
            app.view(),
        );
        ui.click("Cancel")
            .expect("Confirmation can be cancelled at minimum size");
        assert!(matches!(
            ui.into_messages().collect::<Vec<_>>().as_slice(),
            [Message::Cancel]
        ));
    }
    #[test]
    fn delayed_handoff_cannot_close_a_touched_window_or_clear_its_dialogue() {
        let mut app = app();
        let _ = app.update(Message::Bus(Delivery::Refused {
            name_taken: true,
            message: "already registered".into(),
        }));
        assert!(app.handoff_pending);
        let _ = app.update(Message::Action(Action::View(View::Installed)));
        app.dialog = Some(Dialog::Shortcuts);
        let _ = app.update(Message::Bus(Delivery::Forwarded(Ok(()))));
        assert!(app.touched);
        assert!(!app.quitting);
        assert_eq!(app.selection.view, View::Installed);
        assert!(matches!(app.dialog, Some(Dialog::Shortcuts)));
    }
    #[test]
    fn untouched_initial_collision_closes_only_after_successful_handoff() {
        let mut app = app();
        let _ = app.update(Message::Bus(Delivery::Refused {
            name_taken: true,
            message: "already registered".into(),
        }));
        assert!(app.handoff_pending);
        assert!(!app.quitting);
        let _ = app.update(Message::Bus(Delivery::Forwarded(Err(
            "target disappeared".into()
        ))));
        assert!(!app.quitting);
        let _ = app.update(Message::Bus(Delivery::Forwarded(Ok(()))));
        assert!(
            !app.quitting,
            "unsolicited late completion must not close a window"
        );
        let mut untouched = super::tests::app();
        let _ = untouched.update(Message::Bus(Delivery::Refused {
            name_taken: true,
            message: "already registered".into(),
        }));
        let _ = untouched.update(Message::Bus(Delivery::Forwarded(Ok(()))));
        assert!(untouched.quitting);
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

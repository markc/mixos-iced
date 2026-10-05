// SPDX-License-Identifier: MIT OR Apache-2.0
//! A concrete Wayland adapter for the toolkit's backend-independent session.
#[allow(dead_code)]
#[path = "../../../libs/toolkit/examples/gallery/app.rs"]
mod gallery;
use iced::{Element, Subscription, Task, window};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, io::Write, path::PathBuf, sync::Arc};
use toolkit::{Theme, dnd::native as drag};

fn main() -> iced::Result {
    iced::daemon(App::new, App::update, App::view)
        .title(App::title)
        .theme(App::theme)
        .subscription(App::subscription)
        .scale_factor(App::scale_factor)
        .run()
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Source,
    Target,
}
struct Pane {
    role: Role,
    gallery: gallery::Gallery,
    payload: String,
    received: usize,
}
struct App {
    panes: BTreeMap<window::Id, Pane>,
    drag: drag::Session<String, window::Id>,
    trace: Option<PathBuf>,
    action: drag::Action,
    case: String,
    scale: f32,
    last_press: Option<iced::window::drag::Gesture>,
}
#[derive(Debug, Clone)]
enum Message {
    Gallery(window::Id, gallery::Message),
    Native(window::Id, iced::window::drag::Event),
    Start(window::Id, String),
    Queued(window::Id, bool, Result<(), iced::window::drag::Error>),
    Released(window::Id),
    Cancel,
    Closed(window::Id),
}
fn label(key: &str) -> String {
    use fluent_bundle::{FluentBundle, FluentResource};
    let resource =
        FluentResource::try_new(include_str!("../i18n/en/native-gallery.ftl").into()).unwrap();
    let mut bundle = FluentBundle::new(vec!["en".parse().unwrap()]);
    bundle.add_resource(resource).unwrap();
    let value = bundle.get_message(key).unwrap().value().unwrap();
    bundle.format_pattern(value, None, &mut vec![]).into_owned()
}
fn option(name: &str, fallback: &str) -> String {
    let arguments: Vec<_> = std::env::args().collect();
    arguments
        .windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
        .unwrap_or_else(|| fallback.into())
}
impl App {
    fn new() -> (Self, Task<Message>) {
        let trace = option("--trace", "");
        let role = option("--role", "both");
        let action = if option("--action", "move") == "copy" {
            drag::Action::Copy
        } else {
            drag::Action::Move
        };
        let bytes: usize = option("--bytes", "128").parse().unwrap();
        assert!(bytes <= drag::Source::MAX_BYTES);
        let mut panes = BTreeMap::new();
        let mut tasks = Vec::new();
        for (kind, wanted) in [
            (Role::Source, role != "target"),
            (Role::Target, role != "source"),
        ] {
            if !wanted {
                continue;
            }
            let (id, open) = window::open(window::Settings {
                size: iced::Size::new(620.0, 850.0),
                decorations: false,
                exit_on_close_request: false,
                ..Default::default()
            });
            panes.insert(
                id,
                Pane {
                    role: kind,
                    gallery: gallery::Gallery::new(),
                    payload: "a".repeat(bytes),
                    received: 0,
                },
            );
            tasks.push(open.discard());
        }
        let app = Self {
            panes,
            drag: drag::Session::new(drag::Text),
            trace: (!trace.is_empty()).then(|| trace.into()),
            action,
            case: option("--case", "transfer"),
            scale: option("--target-scale", "1").parse().unwrap(),
            last_press: None,
        };
        app.record(serde_json::json!({"event":"ready"}));
        (app, Task::batch(tasks))
    }
    fn record(&self, value: serde_json::Value) {
        println!("{value}");
        if let Some(path) = &self.trace {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap();
            file.write_all(format!("{value}\n").as_bytes()).unwrap();
        }
    }
    fn role(&self, id: window::Id) -> &'static str {
        if self.panes.get(&id).is_some_and(|p| p.role == Role::Source) {
            "source"
        } else {
            "target"
        }
    }
    fn effects(&mut self, effects: Vec<drag::Effect<String, window::Id>>) -> Task<Message> {
        let mut tasks = Vec::new();
        for effect in effects {
            match effect {
                drag::Effect::Request { window, request } => {
                    let starting = matches!(request, drag::Request::Start(_, _));
                    tasks.push(
                        window::drag_drop(window, request_to_iced(request))
                            .map(move |result| Message::Queued(window, starting, result)),
                    );
                }
                drag::Effect::Delivery {
                    window,
                    offer,
                    payload,
                    action,
                } => {
                    self.record(serde_json::json!({"event":"delivery", "role":self.role(window), "bytes":payload.len(), "sha256":format!("{:x}",Sha256::digest(payload.as_bytes())), "action":format!("{action:?}")}));
                    if let Some(pane) = self.panes.get_mut(&window) {
                        pane.received += 1;
                    }
                    if let Some(finish) = self.drag.applied(window, offer, true) {
                        tasks.push(self.effects(vec![finish]));
                    }
                }
                drag::Effect::Finished {
                    window,
                    payload,
                    action,
                } => {
                    self.record(serde_json::json!({"event":"finished", "role":self.role(window), "bytes":payload.len(), "sha256":format!("{:x}",Sha256::digest(payload.as_bytes())), "action":format!("{action:?}")}));
                    if action == drag::Action::Move
                        && let Some(pane) = self.panes.get_mut(&window)
                    {
                        pane.payload.clear();
                    }
                    self.record(serde_json::json!({"event":"source-state","role":self.role(window),"remaining":self.panes.get(&window).map_or(0, |pane| pane.payload.len())}));
                }
                drag::Effect::Cancelled { window } => {
                    self.record(serde_json::json!({"event":"cancelled", "role":self.role(window)}))
                }
                drag::Effect::Failed { window, .. } => {
                    self.record(serde_json::json!({"event":"failed", "role":self.role(window)}))
                }
            }
        }
        Task::batch(tasks)
    }
    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Gallery(id, message) => self
                .panes
                .get_mut(&id)
                .map(|pane| {
                    pane.gallery
                        .update_with_tasks(message)
                        .map(move |message| Message::Gallery(id, message))
                })
                .unwrap_or_else(Task::none),
            Message::Start(id, payload) => {
                match self.drag.start(id, payload, drag::Actions::BOTH) {
                    Ok(effect) => self.effects(vec![effect]),
                    Err(error) => {
                        self.record(serde_json::json!({"event":"start-error","error":error}));
                        Task::none()
                    }
                }
            }
            Message::Native(id, event) => {
                let kind = match &event {
                    window::drag::Event::Gesture(_) => "press",
                    window::drag::Event::Started(_) => "started",
                    window::drag::Event::Rejected(_) => "rejected",
                    window::drag::Event::Enter { .. } => "enter",
                    window::drag::Event::Motion { .. } => "motion",
                    window::drag::Event::Action { .. } => "action",
                    window::drag::Event::Leave(_) => "leave",
                    window::drag::Event::Drop(_) => "drop",
                    window::drag::Event::Data { .. } => "data",
                    window::drag::Event::Failed(_) => "backend-failed",
                    window::drag::Event::Finished { .. } => "backend-finished",
                    window::drag::Event::Cancelled(_) => "backend-cancelled",
                };
                self.record(serde_json::json!({"event":kind, "role":self.role(id)}));
                if let window::drag::Event::Gesture(gesture) = &event {
                    self.last_press = Some(*gesture);
                }
                let target = self.panes.get(&id).is_some_and(|p| p.role == Role::Target);
                let started = matches!(event, window::drag::Event::Started(_));
                let entered = matches!(event, window::drag::Event::Enter { .. });
                let reject = self.case == "reject";
                let action = self.action;
                let effects = self
                    .drag
                    .event(id, event_to_toolkit(event), move |_, point| {
                        (target
                            && !reject
                            && point.x >= 16.0
                            && point.x < 550.0
                            && point.y >= 16.0
                            && point.y < 120.0)
                            .then_some(action)
                    });
                let task = self.effects(effects);
                if started && self.case == "cancel" {
                    return Task::batch([
                        task,
                        self.drag
                            .cancel()
                            .map(|effect| self.effects(vec![effect]))
                            .unwrap_or_else(Task::none),
                    ]);
                }
                if (started && self.case == "source-close")
                    || (entered && self.case == "target-close" && target)
                {
                    return Task::batch([task, window::close(id)]);
                }
                if self.case == "wrong-window" && kind == "press" {
                    if let Some(target) = self
                        .panes
                        .iter()
                        .find(|(_, p)| p.role == Role::Target)
                        .map(|(id, _)| *id)
                    {
                        return Task::batch([task, self.stale_start(target)]);
                    }
                }
                task
            }
            Message::Queued(id, starting, result) => {
                if let Err(error) = result {
                    self.record(
                        serde_json::json!({"event":"queue-error","error":format!("{error:?}")}),
                    );
                    if starting && let Some(effect) = self.drag.start_failed(&id) {
                        return self.effects(vec![effect]);
                    }
                }
                Task::none()
            }
            Message::Released(id) => {
                self.drag.released(&id);
                if self.case == "stale" && self.role(id) == "source" {
                    self.stale_start(id)
                } else {
                    Task::none()
                }
            }
            Message::Cancel => self
                .drag
                .cancel()
                .map(|effect| self.effects(vec![effect]))
                .unwrap_or_else(Task::none),
            Message::Closed(id) => {
                self.record(serde_json::json!({"event":"closed", "role":self.role(id)}));
                let effects = self.drag.closed(&id).into_iter().collect();
                let task = self.effects(effects);
                self.panes.remove(&id);
                if self.panes.is_empty() {
                    iced::exit()
                } else {
                    task
                }
            }
        }
    }
    fn stale_start(&self, id: window::Id) -> Task<Message> {
        let Some(gesture) = self.last_press else {
            return Task::none();
        };
        window::drag_drop(
            id,
            window::drag::Request::Start(
                gesture,
                window::drag::Source {
                    mime: "text/plain;charset=utf-8".into(),
                    bytes: Arc::from(&b"stale"[..]),
                    actions: window::drag::Actions::COPY,
                },
            ),
        )
        .map(move |result| Message::Queued(id, true, result))
    }
    fn view(&self, id: window::Id) -> Element<'_, Message, Theme> {
        use iced::widget::{column, container, text};
        let Some(pane) = self.panes.get(&id) else {
            return iced::widget::space().into();
        };
        let tokens = pane.gallery.theme().tokens();
        let strip: Element<'_, Message, Theme> = match pane.role {
            Role::Source if !pane.payload.is_empty() => toolkit::dnd::DragArea::new(
                container(text(label("source-label")))
                    .height(88)
                    .width(iced::Fill)
                    .padding(tokens.metrics.spacing.md)
                    .style(toolkit::theme::container::card),
                pane.payload.clone(),
            )
            .on_drag(move |gesture| Message::Start(id, gesture))
            .into(),
            Role::Source => container(text(label("empty-label"))).height(88).into(),
            Role::Target => container(column![
                text(label("target-label")),
                text(format!("{}: {}", label("received-label"), pane.received))
            ])
            .height(88)
            .width(iced::Fill)
            .padding(tokens.metrics.spacing.md)
            .style(toolkit::theme::container::card)
            .into(),
        };
        container(
            column![
                strip,
                pane.gallery
                    .view()
                    .map(move |message| Message::Gallery(id, message))
            ]
            .spacing(tokens.metrics.spacing.md),
        )
        .padding(16)
        .into()
    }
    fn title(&self, id: window::Id) -> String {
        label(if self.role(id) == "source" {
            "source-title"
        } else {
            "target-title"
        })
    }
    fn theme(&self, id: window::Id) -> Theme {
        self.panes
            .get(&id)
            .map(|p| p.gallery.theme())
            .unwrap_or_default()
    }
    fn scale_factor(&self, id: window::Id) -> f32 {
        if self.role(id) == "target" {
            self.scale
        } else {
            1.0
        }
    }
    fn subscription(&self) -> Subscription<Message> {
        iced::event::listen_with(|event, _, id| match event {
            iced::Event::Window(window::Event::DragDrop(event)) => Some(Message::Native(id, event)),
            iced::Event::Window(window::Event::Closed) => Some(Message::Closed(id)),
            iced::Event::Mouse(iced::mouse::Event::ButtonReleased(iced::mouse::Button::Left)) => {
                Some(Message::Released(id))
            }
            iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
                ..
            }) => Some(Message::Cancel),
            _ => None,
        })
    }
}

fn event_to_toolkit(event: window::drag::Event) -> drag::Event {
    use window::drag::Event as Native;
    let gesture = |id: window::drag::Gesture| drag::Gesture(id.0);
    let offer = |id: window::drag::Offer| drag::Offer(id.0);
    let action = |action| match action {
        window::drag::Action::Copy => drag::Action::Copy,
        window::drag::Action::Move => drag::Action::Move,
    };
    match event {
        Native::Gesture(id) => drag::Event::Gesture(gesture(id)),
        Native::Started(id) => drag::Event::Started(gesture(id)),
        Native::Rejected(id) => drag::Event::Rejected(gesture(id)),
        Native::Enter {
            offer: id,
            position,
            mimes,
        } => drag::Event::Enter {
            offer: offer(id),
            position,
            mimes,
        },
        Native::Motion {
            offer: id,
            position,
        } => drag::Event::Motion {
            offer: offer(id),
            position,
        },
        Native::Action {
            offer: id,
            action: selected,
        } => drag::Event::Action {
            offer: offer(id),
            action: selected.map(action),
        },
        Native::Leave(id) => drag::Event::Leave(offer(id)),
        Native::Drop(id) => drag::Event::Drop(offer(id)),
        Native::Data {
            offer: id,
            mime,
            bytes,
            action: selected,
        } => drag::Event::Data {
            offer: offer(id),
            mime,
            bytes,
            action: action(selected),
        },
        Native::Failed(id) => drag::Event::Failed(offer(id)),
        Native::Finished {
            gesture: id,
            action: selected,
        } => drag::Event::Finished {
            gesture: gesture(id),
            action: action(selected),
        },
        Native::Cancelled(id) => drag::Event::Cancelled(gesture(id)),
    }
}
fn request_to_iced(request: drag::Request) -> window::drag::Request {
    let gesture = |id: drag::Gesture| window::drag::Gesture(id.0);
    let offer = |id: drag::Offer| window::drag::Offer(id.0);
    let action = |action| match action {
        drag::Action::Copy => window::drag::Action::Copy,
        drag::Action::Move => window::drag::Action::Move,
    };
    let actions = |actions: drag::Actions| window::drag::Actions {
        copy: actions.copy,
        move_: actions.move_,
    };
    match request {
        drag::Request::Start(id, source) => window::drag::Request::Start(
            gesture(id),
            window::drag::Source {
                mime: source.mime,
                bytes: source.bytes,
                actions: actions(source.actions),
            },
        ),
        drag::Request::Accept(id, mime, allowed, preferred) => {
            window::drag::Request::Accept(offer(id), mime, actions(allowed), action(preferred))
        }
        drag::Request::Receive(id, mime) => window::drag::Request::Receive(offer(id), mime),
        drag::Request::Finish(id, applied) => window::drag::Request::Finish(offer(id), applied),
        drag::Request::Cancel(id) => window::drag::Request::Cancel(gesture(id)),
    }
}

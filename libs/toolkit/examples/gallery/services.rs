// SPDX-License-Identifier: MIT OR Apache-2.0
//! The "Dialogs & toasts" gallery page: every dialog kind and every toast
//! severity, opened from a button or from its key; a key router at the
//! root, the modal frame over the page while a dialog is up, the toast
//! stack in the corner, and a log of the outcomes that came back.
use toolkit::Tokens;
use toolkit::dialog::{self, Button, Dialog, ModalQueue, Outcome, Progress, Severity};
use toolkit::iced::keyboard::{Key, key::Named};
use toolkit::iced::widget::{button, column, container, row, text};
use toolkit::iced::{self, Fill};
use toolkit::keys::{self, Bindings, Routed};
use toolkit::theme::{self, Theme};
use toolkit::toast::{self, Toast, Toaster};

use super::strings::{format, label};

pub type Element<'a> = iced::Element<'a, Message, Theme>;

/// What the page can open, each with a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Message,
    Confirm,
    Prompt,
    Secret,
    Choice,
    Progress,
    Busy,
    Queue,
    ToastInfo,
    ToastSuccess,
    ToastWarning,
    ToastError,
}

impl Action {
    pub const ALL: [Action; 12] = [
        Action::Message,
        Action::Confirm,
        Action::Prompt,
        Action::Secret,
        Action::Choice,
        Action::Progress,
        Action::Busy,
        Action::Queue,
        Action::ToastInfo,
        Action::ToastSuccess,
        Action::ToastWarning,
        Action::ToastError,
    ];

    /// The key that opens it.
    pub fn chord(self) -> &'static str {
        match self {
            Action::Message => "F2",
            Action::Confirm => "F3",
            Action::Prompt => "F4",
            Action::Secret => "F5",
            Action::Choice => "F6",
            Action::Progress => "F7",
            Action::Busy => "F8",
            Action::Queue => "F9",
            Action::ToastInfo => "Ctrl+1",
            Action::ToastSuccess => "Ctrl+2",
            Action::ToastWarning => "Ctrl+3",
            Action::ToastError => "Ctrl+4",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Action::Message => "open-message",
            Action::Confirm => "open-confirm",
            Action::Prompt => "open-prompt",
            Action::Secret => "open-secret",
            Action::Choice => "open-choice",
            Action::Progress => "open-progress",
            Action::Busy => "open-busy",
            Action::Queue => "open-queue",
            Action::ToastInfo => "toast-info",
            Action::ToastSuccess => "toast-success",
            Action::ToastWarning => "toast-warning",
            Action::ToastError => "toast-error",
        }
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    Open(Action),
    Dialog(dialog::Event),
    Toast(toast::Event),
    Route(Routed<Action>),
    /// Escape with no dialog up: the newest toast goes.
    DismissToast,
}

pub struct State {
    bindings: Bindings<Action>,
    dialog: Option<Dialog>,
    queue: ModalQueue<Dialog>,
    toaster: Toaster,
    outcomes: Vec<String>,
}

/// How many outcomes the log shows.
const LOG: usize = 6;

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

impl State {
    pub fn new() -> Self {
        Self {
            bindings: Bindings::from_table(Action::ALL.map(|action| (action.chord(), action)))
                .expect("the page's keys are distinct"),
            dialog: None,
            queue: ModalQueue::new(),
            toaster: Toaster::new(),
            outcomes: Vec::new(),
        }
    }

    pub fn dialog(&self) -> Option<&Dialog> {
        self.dialog.as_ref()
    }

    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    pub fn toaster(&self) -> &Toaster {
        &self.toaster
    }

    pub fn outcomes(&self) -> &[String] {
        &self.outcomes
    }

    fn dialog_for(action: Action) -> Option<Dialog> {
        Some(match action {
            Action::Message => Dialog::message(label("dlg-message-title"), label("dlg-message-body"))
                .severity(Severity::Error),
            Action::Confirm => Dialog::confirm(label("dlg-confirm-title"), label("dlg-confirm-body"))
                .buttons([
                    Button::destructive(label("dlg-confirm-delete")),
                    Button::cancel(label("dlg-confirm-keep")),
                ]),
            Action::Prompt => Dialog::prompt(label("dlg-prompt-title"), label("dlg-prompt-body"))
                .placeholder(label("dlg-prompt-placeholder"))
                .value(label("dlg-prompt-value")),
            Action::Secret => Dialog::secret(label("dlg-secret-title"), label("dlg-secret-body"))
                .placeholder(label("dlg-secret-placeholder")),
            Action::Choice => Dialog::choice(
                label("dlg-choice-title"),
                label("dlg-choice-body"),
                [
                    label("choice-viewer"),
                    label("choice-editor"),
                    label("choice-terminal"),
                    label("choice-player"),
                ],
            ),
            Action::Progress => {
                let mut dialog = Dialog::progress(label("dlg-progress-title"), label("dlg-progress-body"))
                    .cancellable(true);
                dialog.set_progress(Progress::Fraction(0.35));
                dialog
            }
            Action::Busy => {
                Dialog::progress(label("dlg-busy-title"), label("dlg-busy-body")).cancellable(true)
            }
            _ => return None,
        })
    }

    fn toast_for(action: Action) -> Option<Toast> {
        Some(match action {
            Action::ToastInfo => Toast::new(label("toast-info-title")).body(label("toast-info-body")),
            Action::ToastSuccess => Toast::new(label("toast-success-title"))
                .body(label("toast-success-body"))
                .severity(Severity::Success),
            Action::ToastWarning => Toast::new(label("toast-warning-title"))
                .body(label("toast-warning-body"))
                .severity(Severity::Warning)
                .sticky(),
            Action::ToastError => Toast::new(label("toast-error-title"))
                .body(label("toast-error-body"))
                .severity(Severity::Error)
                .action(label("toast-retry")),
            _ => return None,
        })
    }

    fn log(&mut self, line: String) {
        self.outcomes.push(line);
        if self.outcomes.len() > LOG {
            self.outcomes.remove(0);
        }
    }

    pub fn update(&mut self, message: Message) {
        match message {
            Message::Open(Action::Queue) => {
                for number in 1..=3 {
                    let dialog = Dialog::message(
                        format("dlg-queued-title", &[("number", number.to_string())]),
                        label("dlg-queued-body"),
                    );
                    self.queue.offer(&mut self.dialog, dialog);
                }
            }
            Message::Open(action) => {
                if let Some(dialog) = Self::dialog_for(action) {
                    self.queue.offer(&mut self.dialog, dialog);
                } else if let Some(toast) = Self::toast_for(action) {
                    let _ = self.toaster.push(toast);
                }
            }
            Message::Dialog(event) => {
                let Some(dialog) = &mut self.dialog else {
                    return;
                };
                if let Some(outcome) = dialog.update(event) {
                    let line = match outcome {
                        Outcome::Cancelled => label("outcome-cancelled"),
                        Outcome::Accepted => label("outcome-accepted"),
                        Outcome::Text(text) => format("outcome-text", &[("text", text)]),
                        Outcome::Chosen(index) => {
                            format("outcome-chosen", &[("index", index.to_string())])
                        }
                        Outcome::Button(index) => {
                            format("outcome-button", &[("index", index.to_string())])
                        }
                    };
                    self.log(line);
                    self.dialog = None;
                    self.queue.next(&mut self.dialog, |_| true);
                }
            }
            Message::Toast(event) => {
                if let Some(id) = self.toaster.update(event) {
                    self.log(format("outcome-toast-action", &[("id", format!("{id:?}"))]));
                }
            }
            Message::Route(Routed::Action(action)) => self.update(Message::Open(action)),
            Message::Route(Routed::Menu(_)) => {}
            Message::DismissToast => {
                if let Some((id, _)) = self.toaster.iter().last() {
                    self.toaster.dismiss(id);
                }
            }
        }
    }

    /// The page body: one button per action, with its key, and the log.
    pub fn view(&self, tokens: Tokens) -> Element<'_> {
        let m = tokens.metrics;
        let buttons = row(Action::ALL.into_iter().map(|action| {
            let caption = format(
                "action-key",
                &[
                    ("label", label(action.id())),
                    ("chord", action.chord().to_owned()),
                ],
            );
            let mut open = button(text(caption));
            if Self::toast_for(action).is_some() {
                open = open.style(theme::button::secondary);
            }
            open.on_press(Message::Open(action)).into()
        }))
        .spacing(m.spacing.md)
        .wrap();
        let mut log = column![text(label("outcomes")).size(m.text.lg)].spacing(m.spacing.xs);
        for line in &self.outcomes {
            log = log.push(text(line.as_str()).style(theme::text::muted));
        }
        column![
            text(label("services")).size(m.text.xxl),
            text(label("services-hint")),
            buttons,
            text(format("queued", &[("count", self.queue.len().to_string())])).style(theme::text::muted),
            container(log)
                .padding(m.spacing.lg)
                .width(Fill)
                .style(theme::container::card),
        ]
        .spacing(m.spacing.lg)
        .into()
    }

    /// Wraps the window content: the toast stack over it, the modal frame
    /// over that while a dialog is up, and the key router around all of it
    /// (bindings are off while the dialog owns the keyboard).
    pub fn wrap<'a, M>(
        &'a self,
        content: iced::Element<'a, M, Theme>,
        tokens: Tokens,
        lift: impl Fn(Message) -> M + Clone + 'a,
    ) -> iced::Element<'a, M, Theme>
    where
        M: Clone + 'a,
    {
        let toasts = lift.clone();
        let content = toast::overlay(content, &self.toaster, tokens, move |event| {
            toasts(Message::Toast(event))
        });
        let content = match &self.dialog {
            Some(dialog) => {
                let answers = lift.clone();
                dialog::modal(content, dialog, tokens, move |event| {
                    answers(Message::Dialog(event))
                })
            }
            None => content,
        };
        let routes = lift.clone();
        keys::router(content, &self.bindings, move |routed| routes(Message::Route(routed)))
            .modal(self.dialog.is_some())
            .on_unclaimed(move |key, _modifiers| {
                matches!(key.as_ref(), Key::Named(Named::Escape)).then(|| lift(Message::DismissToast))
            })
            .into()
    }
}

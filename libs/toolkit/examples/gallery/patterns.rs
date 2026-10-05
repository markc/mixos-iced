// SPDX-License-Identifier: MIT OR Apache-2.0
//! Common application patterns, with all UI strings supplied by Fluent.
use super::strings::label;
use toolkit::dialog::Severity;
use toolkit::iced::{
    Element,
    widget::{button, checkbox, column, container, row, text},
};
use toolkit::patterns::{
    About, Breadcrumbs, HeaderBar, InfoBar, InputField, PathBar, SettingRow, SettingsSection,
};
use toolkit::{Theme, Tokens};

#[derive(Debug, Clone)]
pub enum Message {
    Query(String),
    ClearQuery,
    SubmitQuery,
    Path(String),
    SubmitPath,
    CancelPath,
    Crumb(usize),
    Autosave(bool),
    Retry,
    Dismiss,
    Back,
    Help,
    Drag,
    Link,
}

pub struct State {
    query: String,
    path: String,
    autosave: bool,
    notice: bool,
    search_hint: String,
    path_hint: String,
    outcome: String,
}

impl State {
    pub fn new() -> Self {
        Self {
            query: String::new(),
            path: "/documents/project".into(),
            autosave: true,
            notice: true,
            search_hint: label("patterns-search-hint"),
            path_hint: label("patterns-path-hint"),
            outcome: String::new(),
        }
    }
    pub fn update(&mut self, message: Message) {
        match message {
            Message::Query(query) => self.query = query,
            Message::ClearQuery => self.query.clear(),
            Message::Path(path) => self.path = path,
            Message::Autosave(enabled) => self.autosave = enabled,
            Message::Dismiss => self.notice = false,
            Message::Crumb(index) => {
                self.outcome = format!("{} {index}", label("patterns-navigated"))
            }
            Message::SubmitQuery => self.outcome = self.query.clone(),
            Message::SubmitPath => self.outcome = self.path.clone(),
            Message::CancelPath => self.outcome = label("patterns-cancelled"),
            Message::Retry => self.outcome = label("patterns-retried"),
            Message::Back => self.outcome = label("patterns-back"),
            Message::Help => self.outcome = label("patterns-help"),
            Message::Drag => self.outcome = label("patterns-drag"),
            Message::Link => self.outcome = label("patterns-link"),
        }
    }
    pub fn view(&self, tokens: Tokens) -> Element<'_, Message, Theme> {
        let header = HeaderBar::new(text(label("patterns-title")).size(tokens.metrics.text.lg))
            .start(
                button(text(label("patterns-back")))
                    .on_press(Message::Back)
                    .style(toolkit::theme::button::text),
            )
            .end(
                button(text(label("patterns-help")))
                    .on_press(Message::Help)
                    .style(toolkit::theme::button::text),
            )
            .on_drag(Message::Drag)
            .view(tokens);
        let crumbs = Breadcrumbs::new(label("patterns-separator"))
            .push(label("patterns-home"), Message::Crumb(0))
            .push(label("patterns-documents"), Message::Crumb(1))
            .push(label("patterns-project"), Message::Crumb(2))
            .view(tokens);
        let path = PathBar::new(&self.path_hint, &self.path)
            .on_input(Message::Path)
            .on_submit(Message::SubmitPath)
            .on_cancel(Message::CancelPath)
            .view(tokens);
        let search = InputField::new(&self.search_hint, &self.query)
            .on_input(Message::Query)
            .on_submit(Message::SubmitQuery)
            .clear(label("patterns-clear"), Message::ClearQuery)
            .helper(label("patterns-search-helper"))
            .view(tokens);
        let settings = SettingsSection::new(label("patterns-settings"))
            .push(
                SettingRow::new(
                    label("patterns-autosave"),
                    checkbox(self.autosave)
                        .label(label("patterns-enabled"))
                        .on_toggle(Message::Autosave),
                )
                .description(label("patterns-autosave-description"))
                .view(tokens),
            )
            .view(tokens);
        let about = About::new(
            label("patterns-example-name"),
            env!("CARGO_PKG_VERSION"),
            text(label("patterns-credits")),
        )
        .link(label("patterns-website"), Message::Link)
        .view(tokens);
        let mut body = column![
            header,
            crumbs,
            path,
            search,
            settings,
            container(about).height(180),
            text(&self.outcome)
        ]
        .spacing(tokens.metrics.spacing.md);
        if self.notice {
            body = body.push(
                InfoBar::new(Severity::Warning, text(label("patterns-warning")))
                    .action(label("patterns-retry"), Message::Retry)
                    .dismiss(label("patterns-dismiss"), Message::Dismiss)
                    .view(tokens),
            );
        }
        // A wrap keeps the demonstration usable in a narrow gallery window.
        row![body.width(toolkit::core::Length::Fill)].wrap().into()
    }
}

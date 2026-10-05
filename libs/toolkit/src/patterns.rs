// SPDX-License-Identifier: MIT OR Apache-2.0
//! Small application patterns composed from the existing widgets. Labels,
//! content, window operations and navigation all belong to the caller.
//! Styles resolve from the drawing theme; metrics come from the current tokens.

use crate::dialog::Severity;
use crate::{Theme, Tokens};
use iced_core::{
    Element, Length, alignment,
    keyboard::{Key, key::Named},
    widget::Id,
};
use iced_widget::{button, column, container, mouse_area, row, scrollable, text, tooltip};

type View<'a, Message> = Element<'a, Message, Theme, iced_widget::Renderer>;

/// A semantic information/warning strip with optional actions and dismissal.
#[must_use]
pub struct InfoBar<'a, Message> {
    severity: Severity,
    content: View<'a, Message>,
    actions: Vec<(String, Message)>,
    dismiss: Option<(String, Message)>,
}

impl<'a, Message: Clone + 'a> InfoBar<'a, Message> {
    pub fn new(severity: Severity, content: impl Into<View<'a, Message>>) -> Self {
        Self {
            severity,
            content: content.into(),
            actions: vec![],
            dismiss: None,
        }
    }
    pub fn action(mut self, label: impl Into<String>, message: Message) -> Self {
        self.actions.push((label.into(), message));
        self
    }
    pub fn dismiss(mut self, label: impl Into<String>, message: Message) -> Self {
        self.dismiss = Some((label.into(), message));
        self
    }
    pub fn view(self, tokens: Tokens) -> View<'a, Message> {
        let mut actions = row![].spacing(tokens.metrics.spacing.sm);
        for (label, message) in self.actions {
            actions = actions.push(
                button(text(label))
                    .on_press(message)
                    .style(crate::theme::button::secondary),
            );
        }
        if let Some((label, message)) = self.dismiss {
            actions = actions.push(
                button(text(label))
                    .on_press(message)
                    .style(crate::theme::button::text),
            );
        }
        let body = column![container(self.content).width(Length::Fill), actions.wrap()]
            .spacing(tokens.metrics.spacing.sm);
        let severity = self.severity;
        container(body)
            .padding(tokens.metrics.spacing.md)
            .width(Length::Fill)
            .style(move |theme: &Theme| {
                let mut style = crate::theme::container::elevated(theme);
                style.border.color = severity.colour(theme);
                style.border.width = theme.metrics().border.focus_width;
                style
            })
            .into()
    }
}

/// Navigable crumbs. The full path remains reachable by horizontal scrolling;
/// individual long labels elide visually and retain a full-label tooltip.
#[must_use]
pub struct Breadcrumbs<Message> {
    crumbs: Vec<(String, Message)>,
    separator: String,
}

impl<Message: Clone> Breadcrumbs<Message> {
    pub fn new(separator: impl Into<String>) -> Self {
        Self {
            crumbs: vec![],
            separator: separator.into(),
        }
    }
    pub fn push(mut self, label: impl Into<String>, message: Message) -> Self {
        self.crumbs.push((label.into(), message));
        self
    }
    pub fn view<'a>(self, tokens: Tokens) -> View<'a, Message>
    where
        Message: 'a,
    {
        let mut crumbs = row![]
            .spacing(tokens.metrics.spacing.xs)
            .align_y(alignment::Vertical::Center);
        for (index, (label, message)) in self.crumbs.into_iter().enumerate() {
            if index > 0 {
                crumbs = crumbs.push(text(self.separator.clone()).style(crate::theme::text::muted));
            }
            let title = text(label.clone())
                .size(tokens.metrics.text.md)
                .wrapping(iced_core::text::Wrapping::None)
                .ellipsis(iced_core::text::Ellipsis::End);
            let crumb = button(title)
                .on_press(message)
                .style(crate::theme::button::text)
                .width(Length::Fit.max(tokens.metrics.text.md * 12.0));
            crumbs = crumbs.push(tooltip(
                crumb,
                container(text(label))
                    .padding(tokens.metrics.spacing.sm)
                    .style(crate::theme::container::tooltip),
                tooltip::Position::Bottom,
            ));
        }
        scrollable(crumbs)
            .direction(scrollable::Direction::Horizontal(
                scrollable::Scrollbar::new(),
            ))
            .width(Length::Fill)
            .height(Length::Shrink)
            .into()
    }
}

/// A setting's label/description and its trailing control. Wrapping moves the
/// control beneath long labels in a narrow window, preserving reachability.
#[must_use]
pub struct SettingRow<'a, Message> {
    label: String,
    description: Option<String>,
    control: View<'a, Message>,
}

impl<'a, Message: 'a> SettingRow<'a, Message> {
    pub fn new(label: impl Into<String>, control: impl Into<View<'a, Message>>) -> Self {
        Self {
            label: label.into(),
            description: None,
            control: control.into(),
        }
    }
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
    pub fn view(self, tokens: Tokens) -> View<'a, Message> {
        let mut labels = column![text(self.label).size(tokens.metrics.text.md)]
            .spacing(tokens.metrics.spacing.xs);
        if let Some(description) = self.description {
            labels = labels.push(
                text(description)
                    .size(tokens.metrics.text.sm)
                    .style(crate::theme::text::muted),
            );
        }
        row![labels.width(Length::Fill), self.control]
            .spacing(tokens.metrics.spacing.md)
            .align_y(alignment::Vertical::Center)
            .wrap()
            .into()
    }
}

/// A titled settings list column, with consistent row spacing and padding.
#[must_use]
pub struct SettingsSection<'a, Message> {
    title: String,
    rows: Vec<View<'a, Message>>,
}

impl<'a, Message: 'a> SettingsSection<'a, Message> {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            rows: vec![],
        }
    }
    pub fn push(mut self, row: impl Into<View<'a, Message>>) -> Self {
        self.rows.push(row.into());
        self
    }
    pub fn view(self, tokens: Tokens) -> View<'a, Message> {
        let mut column = column![text(self.title).size(tokens.metrics.text.lg)]
            .spacing(tokens.metrics.spacing.md);
        for row in self.rows {
            column = column.push(row);
        }
        container(column)
            .width(Length::Fill)
            .padding(tokens.metrics.spacing.md)
            .style(crate::theme::container::card)
            .into()
    }
}

/// Start/title/end chrome; the host supplies decorations and window messages.
/// Empty header presses can request a native window drag. Interactive children
/// claim their own presses, so pressing a control does not start a drag.
#[must_use]
pub struct HeaderBar<'a, Message> {
    start: View<'a, Message>,
    title: View<'a, Message>,
    end: View<'a, Message>,
    on_drag: Option<Message>,
}

impl<'a, Message: Clone + 'a> HeaderBar<'a, Message> {
    pub fn new(title: impl Into<View<'a, Message>>) -> Self {
        Self {
            start: iced_widget::Space::new().into(),
            title: title.into(),
            end: iced_widget::Space::new().into(),
            on_drag: None,
        }
    }
    pub fn start(mut self, content: impl Into<View<'a, Message>>) -> Self {
        self.start = content.into();
        self
    }
    pub fn end(mut self, content: impl Into<View<'a, Message>>) -> Self {
        self.end = content.into();
        self
    }
    pub fn on_drag(mut self, message: Message) -> Self {
        self.on_drag = Some(message);
        self
    }
    pub fn view(self, tokens: Tokens) -> View<'a, Message> {
        let bar = row![
            container(self.start)
                .width(Length::Fill)
                .align_left(Length::Fill),
            container(self.title).center_x(Length::Fill),
            container(self.end)
                .width(Length::Fill)
                .align_right(Length::Fill)
        ]
        .spacing(tokens.metrics.spacing.sm)
        .align_y(alignment::Vertical::Center);
        let bar = container(bar)
            .width(Length::Fill)
            .padding(tokens.metrics.spacing.sm)
            .style(crate::theme::container::elevated);
        let mut area = mouse_area(bar);
        if let Some(message) = self.on_drag {
            area = area.on_press(message);
        }
        area.into()
    }
}

/// Caller-supplied about/credits/licence content with link action messages.
#[must_use]
pub struct About<'a, Message> {
    name: String,
    version: String,
    content: View<'a, Message>,
    links: Vec<(String, Message)>,
    logo: Option<View<'a, Message>>,
}

impl<'a, Message: Clone + 'a> About<'a, Message> {
    pub fn new(
        name: impl Into<String>,
        version: impl Into<String>,
        content: impl Into<View<'a, Message>>,
    ) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            content: content.into(),
            links: vec![],
            logo: None,
        }
    }
    pub fn logo(mut self, logo: impl Into<View<'a, Message>>) -> Self {
        self.logo = Some(logo.into());
        self
    }
    pub fn link(mut self, label: impl Into<String>, message: Message) -> Self {
        self.links.push((label.into(), message));
        self
    }
    pub fn view(self, tokens: Tokens) -> View<'a, Message> {
        let mut body = column![
            text(self.name).size(tokens.metrics.text.xxl),
            text(self.version)
                .size(tokens.metrics.text.sm)
                .style(crate::theme::text::muted)
        ]
        .spacing(tokens.metrics.spacing.md);
        if let Some(logo) = self.logo {
            body = body.push(logo);
        }
        body = body.push(self.content);
        let mut links = row![].spacing(tokens.metrics.spacing.sm);
        for (label, message) in self.links {
            links = links.push(
                button(text(label))
                    .on_press(message)
                    .style(crate::theme::button::text),
            );
        }
        body = body.push(links.wrap());
        container(scrollable(body))
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(tokens.metrics.spacing.lg)
            .style(crate::theme::container::card)
            .into()
    }
}

/// Search, secure and inline input composition over [`crate::TextField`],
/// retaining its undo/IME behaviour. Clearing is an ordinary caller message.
#[must_use]
pub struct InputField<'a, Message> {
    placeholder: &'a str,
    value: &'a str,
    on_input: Option<Box<dyn Fn(String) -> Message + 'a>>,
    on_submit: Option<Message>,
    on_cancel: Option<Message>,
    clear: Option<(String, Message)>,
    helper: Option<String>,
    error: Option<String>,
    leading: Option<View<'a, Message>>,
    secure: bool,
    id: Option<Id>,
}

impl<'a, Message: Clone + 'a> InputField<'a, Message> {
    pub fn new(placeholder: &'a str, value: &'a str) -> Self {
        Self {
            placeholder,
            value,
            on_input: None,
            on_submit: None,
            on_cancel: None,
            clear: None,
            helper: None,
            error: None,
            leading: None,
            secure: false,
            id: None,
        }
    }
    pub fn on_input(mut self, callback: impl Fn(String) -> Message + 'a) -> Self {
        self.on_input = Some(Box::new(callback));
        self
    }
    pub fn on_submit(mut self, message: Message) -> Self {
        self.on_submit = Some(message);
        self
    }
    pub fn on_cancel(mut self, message: Message) -> Self {
        self.on_cancel = Some(message);
        self
    }
    pub fn clear(mut self, label: impl Into<String>, message: Message) -> Self {
        self.clear = Some((label.into(), message));
        self
    }
    pub fn helper(mut self, text: impl Into<String>) -> Self {
        self.helper = Some(text.into());
        self
    }
    pub fn error(mut self, text: impl Into<String>) -> Self {
        self.error = Some(text.into());
        self
    }
    pub fn leading(mut self, icon: impl Into<View<'a, Message>>) -> Self {
        self.leading = Some(icon.into());
        self
    }
    pub fn secure(mut self, secure: bool) -> Self {
        self.secure = secure;
        self
    }
    pub fn id(mut self, id: Id) -> Self {
        self.id = Some(id);
        self
    }
    pub fn view(self, tokens: Tokens) -> View<'a, Message> {
        let mut input = crate::TextField::new(self.placeholder, self.value)
            .secure(self.secure)
            .size(tokens.metrics.text.md)
            .padding(tokens.metrics.spacing.sm)
            .width(Length::Fill);
        if let Some(id) = self.id {
            input = input.id(id);
        }
        if let Some(callback) = self.on_input {
            input = input.on_input(callback);
        }
        if let Some(message) = self.on_submit {
            input = input.on_submit(message);
        }
        let mut controls = row![]
            .spacing(tokens.metrics.spacing.sm)
            .align_y(alignment::Vertical::Center);
        if let Some(leading) = self.leading {
            controls = controls.push(leading);
        }
        controls = controls.push(input);
        if let Some((label, message)) = self.clear {
            let mut clear = button(text(label)).style(crate::theme::button::text);
            if !self.value.is_empty() {
                clear = clear.on_press(message);
            }
            controls = controls.push(clear);
        }
        let mut body = column![controls].spacing(tokens.metrics.spacing.xs);
        if let Some(error) = self.error {
            body = body.push(
                text(error)
                    .size(tokens.metrics.text.sm)
                    .style(crate::theme::text::destructive),
            );
        } else if let Some(helper) = self.helper {
            body = body.push(
                text(helper)
                    .size(tokens.metrics.text.sm)
                    .style(crate::theme::text::muted),
            );
        }
        crate::keys::keys(body, move |event| {
            matches!(
                event,
                iced_core::keyboard::Event::KeyPressed {
                    key: Key::Named(Named::Escape),
                    ..
                }
            )
            .then(|| self.on_cancel.clone())
            .flatten()
        })
        .into()
    }
}

/// Editable path bar: a caller-owned string, submit and cancel actions. The
/// toolkit does not interpret paths or perform filesystem operations.
pub type PathBar<'a, Message> = InputField<'a, Message>;

// SPDX-License-Identifier: MIT OR Apache-2.0
//! Modal dialogs: a [`Dialog`] model with ready kinds (message, confirm,
//! prompt, secret, choice, progress), the [`Modal`] frame that shows one
//! over the window with a scrim, and a [`ModalQueue`] for dialogs that
//! arrive while another is open.
//!
//! The dialog is plain state the application owns: it draws the card with
//! [`Dialog::view`] (or the whole window with [`modal`]), feeds the
//! [`Event`]s back through [`Dialog::update`], and acts on the
//! [`Outcome`] that returns. Nothing here knows how the answer travels.
//!
//! Keyboard only is enough: Tab and Shift+Tab move between the field, the
//! list and the buttons; Enter activates the focused control (or the
//! default button from the field or list); Escape cancels; Up and Down
//! move the choice. The frame withholds keyboard and IME input from the
//! content under it, so focus never leaves the dialog. Colours come from
//! the theme's tokens; strings default to English and are overridable
//! ([`Strings`], [`Dialog::buttons`]).

use std::collections::VecDeque;

use iced_core::keyboard::key::Named;
use iced_core::keyboard::{self, Key, Modifiers};
use iced_core::widget::{self, Operation, Tree, operation, tree};
use iced_core::{
    Border, Color, Element, Event as CoreEvent, Layout, Length, Padding, Rectangle, Shadow, Shell,
    Size, Vector, Widget, layout, mouse, overlay, renderer, text, time::Instant,
};
use iced_widget::{
    button, center, column, container, mouse_area, opaque, progress_bar, row, scrollable, space,
    text as text_widget,
};

use crate::keys::is_input;
use crate::theme::{self, Theme};
use crate::{TextField, Tokens};

/// The `widget::Id` of a prompt's text field, for focus operations.
pub const FIELD_ID: &str = "toolkit-dialog-field";

/// How serious a message is; also used by toasts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Severity {
    #[default]
    Info,
    Success,
    Warning,
    Error,
}

impl Severity {
    pub const ALL: [Severity; 4] = [
        Severity::Info,
        Severity::Success,
        Severity::Warning,
        Severity::Error,
    ];

    /// The semantic accent colour, resolved from the drawing theme.
    pub fn colour(self, theme: &Theme) -> Color {
        let p = theme.palette();
        match self {
            Severity::Info => p.text,
            Severity::Success => theme.semantic().success,
            Severity::Warning => theme.semantic().warning,
            Severity::Error => p.destructive,
        }
    }
}

/// What a button does when pressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// The default action: Enter from the field or list presses it.
    Primary,
    Secondary,
    Destructive,
    /// Dismisses without acting, as Escape and the scrim do.
    Cancel,
}

/// A dialog button.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Button {
    pub label: String,
    pub role: Role,
}

impl Button {
    pub fn new(label: impl Into<String>, role: Role) -> Self {
        Self {
            label: label.into(),
            role,
        }
    }

    pub fn primary(label: impl Into<String>) -> Self {
        Self::new(label, Role::Primary)
    }

    pub fn secondary(label: impl Into<String>) -> Self {
        Self::new(label, Role::Secondary)
    }

    pub fn destructive(label: impl Into<String>) -> Self {
        Self::new(label, Role::Destructive)
    }

    pub fn cancel(label: impl Into<String>) -> Self {
        Self::new(label, Role::Cancel)
    }
}

/// The default button labels, in English; replace them per dialog with
/// [`Dialog::strings`] or per button with [`Dialog::buttons`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Strings {
    pub ok: String,
    pub cancel: String,
    pub close: String,
}

impl Default for Strings {
    fn default() -> Self {
        Self {
            ok: "OK".into(),
            cancel: "Cancel".into(),
            close: "Close".into(),
        }
    }
}

/// What a dialog asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Something to read, with one button.
    Message(Severity),
    /// A yes/no question.
    Confirm,
    /// A line of text.
    Prompt,
    /// A line of text drawn as dots.
    Secret,
    /// One of a list.
    Choice,
    /// A running operation, cancellable or not.
    Progress,
}

/// How far a progress dialog has got.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Progress {
    /// Unknown length: a sliding bar.
    Indeterminate,
    /// A fraction, 0 to 1.
    Fraction(f32),
}

/// What a dialog reports back to the application.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Button `index` of [`Dialog::buttons`] was pressed.
    Button(usize),
    /// The text field changed.
    Input(String),
    /// Option `index` was picked in the list.
    Select(usize),
    /// Tab.
    FocusNext,
    /// Shift+Tab.
    FocusPrevious,
    /// Enter, or Space on a button.
    Activate,
    /// Up in the list.
    Up,
    /// Down in the list.
    Down,
    /// Escape, the scrim or a cancel button.
    Cancel,
}

/// The answer, once the dialog is done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Escape, the scrim, or a button with [`Role::Cancel`].
    Cancelled,
    /// The [`Role::Primary`] button of a message, confirm or progress
    /// dialog.
    Accepted,
    /// The prompt's or secret's text, from its primary button.
    Text(String),
    /// The chosen option's index, from the choice's primary button.
    Chosen(usize),
    /// Any other button, by index.
    Button(usize),
}

/// A dialog's state: what it asks, what the user has entered and where the
/// keyboard focus is.
#[derive(Debug, Clone, PartialEq)]
pub struct Dialog {
    kind: Kind,
    title: String,
    body: String,
    buttons: Vec<Button>,
    options: Vec<String>,
    selected: usize,
    value: String,
    placeholder: String,
    error: Option<String>,
    progress: Progress,
    cancellable: bool,
    focus: usize,
    width: f32,
}

impl Dialog {
    fn new(kind: Kind, title: impl Into<String>, body: impl Into<String>) -> Self {
        let strings = Strings::default();
        let buttons = match kind {
            Kind::Message(_) => vec![Button::primary(strings.close)],
            Kind::Progress => Vec::new(),
            _ => vec![Button::primary(strings.ok), Button::cancel(strings.cancel)],
        };
        let mut dialog = Self {
            kind,
            title: title.into(),
            body: body.into(),
            buttons,
            options: Vec::new(),
            selected: 0,
            value: String::new(),
            placeholder: String::new(),
            error: None,
            progress: Progress::Indeterminate,
            cancellable: kind != Kind::Progress,
            focus: 0,
            width: 420.0,
        };
        dialog.focus = dialog.default_focus();
        dialog
    }

    /// Something to read, with a close button; `Severity::Info` unless
    /// [`Dialog::severity`] says otherwise.
    pub fn message(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self::new(Kind::Message(Severity::Info), title, body)
    }

    /// A question with OK and Cancel.
    pub fn confirm(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self::new(Kind::Confirm, title, body)
    }

    /// A line of text; the field has focus when the dialog opens.
    pub fn prompt(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self::new(Kind::Prompt, title, body)
    }

    /// A password or other text drawn as dots.
    pub fn secret(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self::new(Kind::Secret, title, body)
    }

    /// One of `options`; the first is selected and the list has focus.
    pub fn choice(
        title: impl Into<String>,
        body: impl Into<String>,
        options: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        let mut dialog = Self::new(Kind::Choice, title, body);
        dialog.options = options.into_iter().map(Into::into).collect();
        dialog
    }

    /// A running operation: indeterminate, with no button, until
    /// [`Dialog::cancellable`] and [`Dialog::set_progress`] say otherwise.
    /// The application closes it when the work is done.
    pub fn progress(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self::new(Kind::Progress, title, body)
    }

    /// The message's severity, which colours its title.
    pub fn severity(mut self, severity: Severity) -> Self {
        if let Kind::Message(_) = self.kind {
            self.kind = Kind::Message(severity);
        }
        self
    }

    /// Replaces the buttons. The first [`Role::Primary`] is the default;
    /// a [`Role::Cancel`] button answers [`Outcome::Cancelled`].
    pub fn buttons(mut self, buttons: impl IntoIterator<Item = Button>) -> Self {
        self.buttons = buttons.into_iter().collect();
        self.focus = self.default_focus();
        self
    }

    /// Relabels the default buttons.
    pub fn strings(mut self, strings: &Strings) -> Self {
        for button in &mut self.buttons {
            button.label = match button.role {
                Role::Primary if matches!(self.kind, Kind::Message(_)) => strings.close.clone(),
                Role::Primary => strings.ok.clone(),
                Role::Cancel => strings.cancel.clone(),
                Role::Secondary | Role::Destructive => continue,
            };
        }
        self
    }

    /// The prompt's initial text.
    pub fn value(mut self, value: impl Into<String>) -> Self {
        self.value = value.into();
        self
    }

    /// The prompt's placeholder.
    pub fn placeholder(mut self, placeholder: impl Into<String>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    /// The choice's initial selection.
    pub fn selected(mut self, index: usize) -> Self {
        self.selected = index.min(self.options.len().saturating_sub(1));
        self
    }

    /// Whether Escape and the scrim cancel (a progress dialog gains a
    /// Cancel button). On by default except for progress.
    pub fn cancellable(mut self, cancellable: bool) -> Self {
        self.cancellable = cancellable;
        if self.kind == Kind::Progress {
            let strings = Strings::default();
            self.buttons = if cancellable {
                vec![Button::cancel(strings.cancel)]
            } else {
                Vec::new()
            };
            self.focus = self.default_focus();
        }
        self
    }

    /// The card width in logical pixels (420 by default).
    pub fn width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }

    /// A validation message under the prompt's field; while set, the
    /// primary button is disabled and Enter does nothing.
    pub fn set_error(&mut self, error: Option<String>) {
        self.error = error;
    }

    pub fn set_value(&mut self, value: impl Into<String>) {
        self.value = value.into();
    }

    pub fn set_body(&mut self, body: impl Into<String>) {
        self.body = body.into();
    }

    pub fn set_progress(&mut self, progress: Progress) {
        self.progress = progress;
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn body(&self) -> &str {
        &self.body
    }

    pub fn button_list(&self) -> &[Button] {
        &self.buttons
    }

    pub fn options(&self) -> &[String] {
        &self.options
    }

    pub fn selection(&self) -> usize {
        self.selected
    }

    pub fn text(&self) -> &str {
        &self.value
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn progress_state(&self) -> Progress {
        self.progress
    }

    pub fn is_cancellable(&self) -> bool {
        self.cancellable
    }

    /// Whether the dialog has a leading focus slot before the buttons: the
    /// prompt's field or the choice's list.
    fn has_leading(&self) -> bool {
        matches!(self.kind, Kind::Prompt | Kind::Secret | Kind::Choice)
    }

    /// The number of keyboard focus slots: the field or list, then the
    /// buttons.
    pub fn slots(&self) -> usize {
        usize::from(self.has_leading()) + self.buttons.len()
    }

    /// The focused slot: 0 is the field or list when there is one, then
    /// the buttons in order.
    pub fn focus(&self) -> usize {
        self.focus
    }

    /// The focused button's index, if a button has focus.
    pub fn focused_button(&self) -> Option<usize> {
        let leading = usize::from(self.has_leading());
        (self.focus >= leading && self.focus < self.slots()).then(|| self.focus - leading)
    }

    /// Whether the field or list has focus.
    pub fn is_leading_focused(&self) -> bool {
        self.has_leading() && self.focus == 0
    }

    /// The default button: the first [`Role::Primary`], else the first
    /// that is not a cancel.
    pub fn default_button(&self) -> Option<usize> {
        self.buttons
            .iter()
            .position(|button| button.role == Role::Primary)
            .or_else(|| {
                self.buttons
                    .iter()
                    .position(|button| button.role != Role::Cancel)
            })
    }

    fn default_focus(&self) -> usize {
        if self.has_leading() {
            0
        } else {
            self.default_button().unwrap_or(0)
        }
    }

    /// The widget to give keyboard focus for the current slot: the text
    /// field, or nothing (buttons are tracked here, not by iced).
    pub fn focus_target(&self) -> Option<widget::Id> {
        (matches!(self.kind, Kind::Prompt | Kind::Secret) && self.focus == 0)
            .then(|| widget::Id::new(FIELD_ID))
    }

    /// Whether the primary button can fire: not while a prompt shows an
    /// error, and never for a progress dialog without one.
    fn primary_enabled(&self) -> bool {
        self.error.is_none()
    }

    /// The event a key press means to this dialog, if any. Tab and
    /// Shift+Tab move focus; Enter activates (Space too, on a button);
    /// Escape cancels; Up and Down move the choice; Left and Right move
    /// between buttons.
    pub fn key(&self, key: &Key, modifiers: Modifiers) -> Option<Event> {
        let on_button = self.focused_button().is_some();
        match key.as_ref() {
            Key::Named(Named::Tab) if modifiers.shift() => Some(Event::FocusPrevious),
            Key::Named(Named::Tab) => Some(Event::FocusNext),
            Key::Named(Named::Enter) => Some(Event::Activate),
            Key::Named(Named::Space) if on_button => Some(Event::Activate),
            Key::Named(Named::Escape) => self.cancellable.then_some(Event::Cancel),
            Key::Named(Named::ArrowUp)
                if self.kind == Kind::Choice && self.is_leading_focused() =>
            {
                Some(Event::Up)
            }
            Key::Named(Named::ArrowDown)
                if self.kind == Kind::Choice && self.is_leading_focused() =>
            {
                Some(Event::Down)
            }
            Key::Named(Named::ArrowLeft) if on_button => Some(Event::FocusPrevious),
            Key::Named(Named::ArrowRight) if on_button => Some(Event::FocusNext),
            _ => None,
        }
    }

    /// Applies an event; `Some` means the dialog is answered and should be
    /// closed.
    pub fn update(&mut self, event: Event) -> Option<Outcome> {
        match event {
            Event::FocusNext => {
                let slots = self.slots();
                if slots > 0 {
                    self.focus = (self.focus + 1) % slots;
                }
                None
            }
            Event::FocusPrevious => {
                let slots = self.slots();
                if slots > 0 {
                    self.focus = (self.focus + slots - 1) % slots;
                }
                None
            }
            Event::Input(value) => {
                self.value = value;
                self.error = None;
                None
            }
            Event::Select(index) => {
                if index < self.options.len() {
                    self.selected = index;
                    self.focus = 0;
                }
                None
            }
            Event::Up => {
                self.selected = self.selected.saturating_sub(1);
                None
            }
            Event::Down => {
                if self.selected + 1 < self.options.len() {
                    self.selected += 1;
                }
                None
            }
            Event::Button(index) => self.press(index),
            Event::Activate => match self.focused_button() {
                Some(index) => self.press(index),
                None => self.default_button().and_then(|index| self.press(index)),
            },
            Event::Cancel => self.cancellable.then_some(Outcome::Cancelled),
        }
    }

    fn press(&self, index: usize) -> Option<Outcome> {
        let button = self.buttons.get(index)?;
        match button.role {
            Role::Cancel => Some(Outcome::Cancelled),
            Role::Primary if !self.primary_enabled() => None,
            Role::Primary => Some(match self.kind {
                Kind::Prompt | Kind::Secret => Outcome::Text(self.value.clone()),
                Kind::Choice => Outcome::Chosen(self.selected),
                Kind::Message(_) | Kind::Confirm | Kind::Progress => Outcome::Accepted,
            }),
            Role::Secondary | Role::Destructive => Some(Outcome::Button(index)),
        }
    }

    /// The dialog card: title, body, the field, list or bar, and the
    /// buttons. Wrap it with [`modal`] to show it over the window.
    pub fn view<Renderer>(&self, tokens: Tokens) -> Element<'_, Event, Theme, Renderer>
    where
        Renderer: text::Renderer + 'static,
    {
        let m = tokens.metrics;
        let severity = match self.kind {
            Kind::Message(severity) => severity,
            _ => Severity::Info,
        };
        let title = text_widget(self.title.as_str())
            .size(m.text.lg)
            .style(move |theme: &Theme| text_widget::Style {
                color: Some(severity.colour(theme)),
            });
        let mut content = column![title].spacing(m.spacing.md).width(Length::Fill);
        if !self.body.is_empty() {
            content = content.push(text_widget(self.body.as_str()).size(m.text.md));
        }
        match self.kind {
            Kind::Prompt | Kind::Secret => {
                content = content.push(
                    TextField::new(&self.placeholder, &self.value)
                        .id(FIELD_ID)
                        .secure(self.kind == Kind::Secret)
                        .on_input(Event::Input)
                        .on_submit(Event::Activate)
                        .width(Length::Fill)
                        .padding(m.spacing.sm)
                        .size(m.text.md),
                );
                if let Some(error) = &self.error {
                    content = content.push(
                        text_widget(error.as_str())
                            .size(m.text.sm)
                            .style(theme::text::destructive),
                    );
                }
            }
            Kind::Choice => {
                let list_focused = self.is_leading_focused();
                let options = self.options.iter().enumerate().map(|(index, option)| {
                    let selected = index == self.selected;
                    button(
                        text_widget(option.as_str())
                            .size(m.text.md)
                            .width(Length::Fill),
                    )
                    .width(Length::Fill)
                    .padding(Padding {
                        top: m.spacing.sm,
                        right: m.spacing.md,
                        bottom: m.spacing.sm,
                        left: m.spacing.md,
                    })
                    .style(move |theme: &Theme, status| {
                        option_style(theme, status, selected, selected && list_focused)
                    })
                    .on_press(Event::Select(index))
                    .into()
                });
                let list = column(options).spacing(m.spacing.xs).width(Length::Fill);
                content = content.push(
                    scrollable(list)
                        .width(Length::Fill)
                        .height(Length::Fit.max(m.text.md * 16.0)),
                );
            }
            Kind::Progress => {
                content = content.push(match self.progress {
                    Progress::Fraction(fraction) => Element::from(
                        row![
                            progress_bar(0.0..=1.0, fraction.clamp(0.0, 1.0))
                                .length(Length::Fill)
                                .girth(m.spacing.md),
                            text_widget(format!("{:.0}%", fraction.clamp(0.0, 1.0) * 100.0))
                                .size(m.text.sm)
                                .style(theme::text::muted),
                        ]
                        .spacing(m.spacing.md)
                        .align_y(iced_core::alignment::Vertical::Center),
                    ),
                    Progress::Indeterminate => Indeterminate::new().height(m.spacing.md).into(),
                });
            }
            Kind::Message(_) | Kind::Confirm => {}
        }
        if !self.buttons.is_empty() {
            let mut actions = row![space::horizontal()].spacing(m.spacing.sm);
            for (index, spec) in self.buttons.iter().enumerate() {
                let focused = self.focused_button() == Some(index);
                let enabled = spec.role != Role::Primary || self.primary_enabled();
                let role = spec.role;
                actions = actions.push(
                    button(text_widget(spec.label.as_str()).size(m.text.md))
                        .padding(Padding {
                            top: m.spacing.sm,
                            right: m.spacing.lg,
                            bottom: m.spacing.sm,
                            left: m.spacing.lg,
                        })
                        .style(move |theme: &Theme, status| {
                            button_style(theme, status, role, focused)
                        })
                        .on_press_maybe(enabled.then_some(Event::Button(index))),
                );
            }
            content = content.push(actions);
        }
        container(content)
            .padding(m.spacing.lg)
            .width(Length::Fixed(self.width))
            .style(card)
            .into()
    }
}

/// The dialog card surface: the `popover` pair, outlined, large radius, a
/// soft shadow.
pub fn card(theme: &Theme) -> container::Style {
    let p = theme.palette();
    let m = theme.metrics();
    container::Style {
        background: Some(p.popover.into()),
        text_color: Some(p.popover_text),
        border: Border {
            color: p.border,
            width: m.border.width,
            radius: m.radius.lg.into(),
        },
        shadow: Shadow {
            color: Color {
                a: 0.35,
                ..darker(p.surface, p.text)
            },
            offset: Vector::new(0.0, m.spacing.sm),
            blur_radius: m.spacing.xl,
        },
        ..container::Style::default()
    }
}

/// The scrim over the content under a dialog: the darker of the surface
/// and the text colour, translucent, so it dims in light and dark alike.
pub fn scrim(theme: &Theme) -> container::Style {
    let p = theme.palette();
    container::Style {
        background: Some(
            Color {
                a: 0.55,
                ..darker(p.surface, p.text)
            }
            .into(),
        ),
        ..container::Style::default()
    }
}

/// The keyboard focus ring.
pub fn focus_ring(theme: &Theme) -> Border {
    let m = theme.metrics();
    Border {
        color: theme.palette().ring,
        width: m.border.focus_width,
        radius: m.radius.md.into(),
    }
}

/// A dialog button by role, with the focus ring when it has keyboard
/// focus.
pub fn button_style(
    theme: &Theme,
    status: button::Status,
    role: Role,
    focused: bool,
) -> button::Style {
    let mut style = match role {
        Role::Primary => theme::button::primary(theme, status),
        Role::Destructive => theme::button::destructive(theme, status),
        Role::Secondary | Role::Cancel => theme::button::secondary(theme, status),
    };
    if focused {
        style.border = focus_ring(theme);
    }
    style
}

/// A choice option: a ghost button, the `selection` pair when selected,
/// the focus ring when the list has focus.
pub fn option_style(
    theme: &Theme,
    status: button::Status,
    selected: bool,
    focused: bool,
) -> button::Style {
    let p = theme.palette();
    let mut style = theme::button::text(theme, status);
    if selected {
        style.background = Some(p.selection.into());
        style.text_color = p.selection_text;
    }
    if focused {
        style.border = focus_ring(theme);
    }
    style
}

/// The darker of two colours.
pub(crate) fn darker(a: Color, b: Color) -> Color {
    if a.relative_luminance() <= b.relative_luminance() {
        a
    } else {
        b
    }
}

/// Shows `dialog` over `base`: the content is dimmed and receives no
/// keyboard or IME input; the card is centred; a press on the scrim
/// cancels when the dialog is cancellable; keys the card does not use go to
/// [`Dialog::key`] and come back as `on_event` messages; the prompt's field
/// is focused while it is the dialog's focused slot.
pub fn modal<'a, Message, Renderer>(
    base: impl Into<Element<'a, Message, Theme, Renderer>>,
    dialog: &'a Dialog,
    tokens: Tokens,
    on_event: impl Fn(Event) -> Message + Clone + 'a,
) -> Element<'a, Message, Theme, Renderer>
where
    Message: Clone + 'a,
    Renderer: text::Renderer + 'static,
{
    let card = dialog.view(tokens).map(on_event.clone());
    let mut backdrop = mouse_area(center(opaque(card)).style(scrim));
    if dialog.is_cancellable() {
        backdrop = backdrop.on_press(on_event(Event::Cancel));
    }
    let keys = on_event;
    Modal::new(base, opaque(backdrop))
        .focus(dialog.focus_target())
        .on_key(move |key, modifiers| dialog.key(key, modifiers).map(&keys))
        .into()
}

/// A persistent modal host. Call this on every view, including when no
/// dialog is open, so the base widget tree survives opening and closing.
/// The identified base focus is restored after the last queued dialog.
pub fn modal_host<'a, Message, Renderer>(
    base: impl Into<Element<'a, Message, Theme, Renderer>>,
    dialog: Option<&'a Dialog>,
    tokens: Tokens,
    on_event: impl Fn(Event) -> Message + Clone + 'a,
) -> Element<'a, Message, Theme, Renderer>
where
    Message: Clone + 'a,
    Renderer: text::Renderer + 'static,
{
    match dialog {
        Some(dialog) => modal(base, dialog, tokens, on_event),
        None => Modal::host(base, None).into(),
    }
}

type KeyFn<'a, Message> = Box<dyn Fn(&Key, Modifiers) -> Option<Message> + 'a>;

/// A layer over a base: the base gets no keyboard or IME input and the
/// layer covers it; keys the layer leaves go to `on_key`, and every key is
/// captured so focus stays in the layer. The widget with the `focus` id in
/// the layer is given keyboard focus (or every focusable unfocused, for
/// `None`) whenever that request changes, so the application states where
/// focus is and the frame makes it so.
pub struct Modal<'a, Message, Theme, Renderer> {
    base: Element<'a, Message, Theme, Renderer>,
    layer: Element<'a, Message, Theme, Renderer>,
    focus: Option<widget::Id>,
    on_key: Option<KeyFn<'a, Message>>,
    open: bool,
    return_focus: Option<widget::Id>,
}

#[derive(Debug, Default)]
struct Applied {
    once: bool,
    target: Option<widget::Id>,
    open: bool,
    previous: Option<widget::Id>,
}

impl<'a, Message, Theme, Renderer> Modal<'a, Message, Theme, Renderer> {
    pub fn new(
        base: impl Into<Element<'a, Message, Theme, Renderer>>,
        layer: impl Into<Element<'a, Message, Theme, Renderer>>,
    ) -> Self
    where
        Renderer: iced_core::Renderer + 'a,
        Message: 'a,
        Theme: 'a,
    {
        Self::host(base, Some(layer.into()))
    }

    /// Keeps the base tree stable when the optional layer opens or closes.
    /// Use the same host at the same tree position on every frame.
    pub fn host(
        base: impl Into<Element<'a, Message, Theme, Renderer>>,
        layer: Option<Element<'a, Message, Theme, Renderer>>,
    ) -> Self
    where
        Renderer: iced_core::Renderer + 'a,
        Message: 'a,
        Theme: 'a,
    {
        let open = layer.is_some();
        Self {
            base: base.into(),
            layer: layer.unwrap_or_else(|| iced_widget::Space::new().into()),
            focus: None,
            on_key: None,
            open,
            return_focus: None,
        }
    }

    /// Fallback focus after close when the prior control has no ID.
    pub fn return_focus(mut self, target: widget::Id) -> Self {
        self.return_focus = Some(target);
        self
    }

    /// The layer widget to focus, or `None` to unfocus them all.
    pub fn focus(mut self, target: Option<widget::Id>) -> Self {
        self.focus = target;
        self
    }

    /// Called with each key press the layer did not capture (Tab and
    /// Escape before the layer sees them, since no text input uses them).
    pub fn on_key(mut self, f: impl Fn(&Key, Modifiers) -> Option<Message> + 'a) -> Self {
        self.on_key = Some(Box::new(f));
        self
    }

    fn apply_focus(&mut self, tree: &mut Tree, layout: Layout<'_>, renderer: &Renderer)
    where
        Renderer: iced_core::Renderer,
    {
        let applied = tree.state.downcast_mut::<Applied>();
        if applied.once && applied.target == self.focus {
            return;
        }
        applied.once = true;
        applied.target = self.focus.clone();
        let layer = &mut self.layer;
        let state = &mut tree.children[1];
        match &self.focus {
            Some(id) => {
                let mut focus = operation::focusable::focus::<()>(id.clone());
                layer
                    .as_widget_mut()
                    .operate(state, layout, renderer, &mut focus);
                // A fresh focus puts the cursor after the text, as a click
                // at the end would.
                let mut to_end = operation::text_input::move_cursor_to_end::<()>(id.clone());
                layer
                    .as_widget_mut()
                    .operate(state, layout, renderer, &mut to_end);
            }
            None => {
                let mut unfocus = operation::focusable::unfocus::<()>();
                layer
                    .as_widget_mut()
                    .operate(state, layout, renderer, &mut unfocus);
            }
        }
    }

    fn sync_base_focus(&mut self, tree: &mut Tree, layout: Layout<'_>, renderer: &Renderer)
    where
        Renderer: iced_core::Renderer,
    {
        let applied = tree.state.downcast_mut::<Applied>();
        if self.open && !applied.open {
            let mut find = operation::focusable::find_focused();
            // The helper returns an ID; our widget's operate expects a unit
            // operation, so capture it through the type-erasing adapter.
            let mut adapter = operation::black_box(&mut find);
            self.base.as_widget_mut().operate(
                &mut tree.children[0],
                layout,
                renderer,
                &mut adapter,
            );
            drop(adapter);
            applied.previous = match find.finish() {
                operation::Outcome::Some(id) => Some(id),
                _ => self.return_focus.clone(),
            };
            applied.once = false;
            let mut unfocus = operation::focusable::unfocus::<()>();
            self.base.as_widget_mut().operate(
                &mut tree.children[0],
                layout,
                renderer,
                &mut unfocus,
            );
        } else if !self.open && applied.open {
            if let Some(id) = applied
                .previous
                .take()
                .or_else(|| self.return_focus.clone())
            {
                let mut focus = operation::focusable::focus::<()>(id);
                self.base.as_widget_mut().operate(
                    &mut tree.children[0],
                    layout,
                    renderer,
                    &mut focus,
                );
            }
            applied.once = false;
        }
        applied.open = self.open;
    }
}

/// Keys the frame takes before the layer: no text input uses them.
fn is_frame_key(key: &Key) -> bool {
    matches!(key.as_ref(), Key::Named(Named::Tab | Named::Escape))
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for Modal<'_, Message, Theme, Renderer>
where
    Renderer: iced_core::Renderer,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Applied>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(Applied::default())
    }

    fn diff(&mut self, tree: &mut Tree) {
        tree.diff_children(&mut [&mut self.base, &mut self.layer]);
    }

    fn size(&self) -> Size<Length> {
        self.base.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let size = self.base.as_widget().size();
        let limits = limits.width(size.width).height(size.height);
        let base = self
            .base
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, &limits);
        let bounds = limits.resolve(size.width, size.height, base.size());
        let layer = self.layer.as_widget_mut().layout(
            &mut tree.children[1],
            renderer,
            &layout::Limits::new(Size::ZERO, bounds),
        );
        let node = layout::Node::with_children(bounds, vec![base, layer]);
        let layout = Layout::new(&node);
        self.sync_base_focus(tree, layout.children().next().expect("base"), renderer);
        if self.open {
            self.apply_focus(tree, layout.children().nth(1).expect("layer"), renderer);
        }
        node
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.container(None, layout.bounds());
        operation.traverse(&mut |operation| {
            for (index, ((child, state), layout)) in [&mut self.base, &mut self.layer]
                .into_iter()
                .zip(&mut tree.children)
                .zip(layout.children())
                .enumerate()
            {
                if (self.open && index == 0) || (!self.open && index == 1) {
                    continue;
                }
                child
                    .as_widget_mut()
                    .operate(state, layout, renderer, operation);
            }
        });
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &CoreEvent,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let mut layouts = layout.children();
        let (Some(base_layout), Some(layer_layout)) = (layouts.next(), layouts.next()) else {
            return;
        };
        if !self.open {
            self.base.as_widget_mut().update(
                &mut tree.children[0],
                event,
                base_layout,
                cursor,
                renderer,
                shell,
                viewport,
            );
            return;
        }
        self.apply_focus(tree, layer_layout, renderer);
        let key_press = match event {
            CoreEvent::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) => {
                Some((key, *modifiers))
            }
            _ => None,
        };
        if let Some((key, modifiers)) = key_press
            && is_frame_key(key)
        {
            if let Some(on_key) = &self.on_key
                && let Some(message) = on_key(key, modifiers)
            {
                shell.publish(message);
                shell.capture_event();
                return;
            }
        }
        self.layer.as_widget_mut().update(
            &mut tree.children[1],
            event,
            layer_layout,
            cursor,
            renderer,
            shell,
            viewport,
        );
        if shell.is_event_captured() {
            return;
        }
        if let Some((key, modifiers)) = key_press {
            if let Some(on_key) = &self.on_key
                && let Some(message) = on_key(key, modifiers)
            {
                shell.publish(message);
            } else if matches!(key, Key::Named(Named::Tab))
                && !modifiers.control()
                && !modifiers.alt()
                && !modifiers.logo()
            {
                let mut operation: Box<dyn widget::Operation> =
                    Box::new(crate::focus::cycle(modifiers.shift()));
                loop {
                    self.layer.as_widget_mut().operate(
                        &mut tree.children[1],
                        layer_layout,
                        renderer,
                        operation.as_mut(),
                    );
                    match operation.finish() {
                        operation::Outcome::Chain(next) => operation = next,
                        _ => break,
                    }
                }
                shell.request_redraw();
            }
            shell.capture_event();
            return;
        }
        if is_input(event) {
            shell.capture_event();
            return;
        }
        self.base.as_widget_mut().update(
            &mut tree.children[0],
            event,
            base_layout,
            cursor,
            renderer,
            shell,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        let mut layouts = layout.children();
        let (Some(base_layout), Some(layer_layout)) = (layouts.next(), layouts.next()) else {
            return mouse::Interaction::None;
        };
        if !self.open {
            return self.base.as_widget().mouse_interaction(
                &tree.children[0],
                base_layout,
                cursor,
                viewport,
                renderer,
            );
        }
        let layer = self.layer.as_widget().mouse_interaction(
            &tree.children[1],
            layer_layout,
            cursor,
            viewport,
            renderer,
        );
        if layer != mouse::Interaction::None {
            return layer;
        }
        mouse::Interaction::None
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let mut layouts = layout.children();
        let (Some(base_layout), Some(layer_layout)) = (layouts.next(), layouts.next()) else {
            return;
        };
        self.base.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            base_layout,
            if self.open {
                mouse::Cursor::Unavailable
            } else {
                cursor
            },
            viewport,
        );
        if self.open {
            renderer.with_layer(*viewport, |renderer| {
                self.layer.as_widget().draw(
                    &tree.children[1],
                    renderer,
                    theme,
                    style,
                    layer_layout,
                    cursor,
                    viewport,
                );
            });
        }
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        if !self.open {
            let base_layout = layout.children().next()?;
            return self.base.as_widget_mut().overlay(
                &mut tree.children[0],
                base_layout,
                renderer,
                viewport,
                translation,
            );
        }
        let layer_layout = layout.children().nth(1)?;
        self.layer.as_widget_mut().overlay(
            &mut tree.children[1],
            layer_layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message, Theme, Renderer> From<Modal<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a,
    Renderer: iced_core::Renderer + 'a,
{
    fn from(modal: Modal<'a, Message, Theme, Renderer>) -> Self {
        Element::new(modal)
    }
}

/// A sliding bar for progress of unknown length: `primary` on the muted
/// surface, one sweep per 1.6 s, redrawn every frame while shown.
pub struct Indeterminate {
    height: f32,
}

struct Started(Instant);

/// Seconds per sweep.
const SWEEP: f32 = 1.6;
/// The moving segment, as a fraction of the width.
const SEGMENT: f32 = 0.3;

impl Indeterminate {
    pub fn new() -> Self {
        Self { height: 8.0 }
    }

    pub fn height(mut self, height: f32) -> Self {
        self.height = height;
        self
    }

    /// Where the segment's left edge is at `elapsed` seconds, as a fraction
    /// of the width: it enters from the left and leaves to the right.
    pub fn phase(elapsed: f32) -> f32 {
        let t = (elapsed / SWEEP).rem_euclid(1.0);
        -SEGMENT + t * (1.0 + SEGMENT)
    }
}

impl Default for Indeterminate {
    fn default() -> Self {
        Self::new()
    }
}

impl<Message, Renderer> Widget<Message, Theme, Renderer> for Indeterminate
where
    Renderer: iced_core::Renderer,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Started>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(Started(Instant::now()))
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fixed(self.height))
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        _renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::Node::new(limits.resolve(Length::Fill, Length::Fixed(self.height), Size::ZERO))
    }

    fn update(
        &mut self,
        _tree: &mut Tree,
        event: &CoreEvent,
        _layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _renderer: &Renderer,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        if let CoreEvent::Window(iced_core::window::Event::RedrawRequested(_)) = event {
            shell.request_redraw();
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        let p = theme.palette();
        let radius = theme.metrics().radius.sm;
        let bounds = layout.bounds();
        let quad = |bounds: Rectangle| renderer::Quad {
            bounds,
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: radius.into(),
            },
            shadow: Shadow::default(),
            snap: false,
        };
        renderer.fill_quad(quad(bounds), p.muted_surface);
        let elapsed = tree
            .state
            .downcast_ref::<Started>()
            .0
            .elapsed()
            .as_secs_f32();
        let left = bounds.x + Self::phase(elapsed) * bounds.width;
        let right = (left + SEGMENT * bounds.width).min(bounds.x + bounds.width);
        let left = left.max(bounds.x);
        if right > left {
            renderer.fill_quad(
                quad(Rectangle {
                    x: left,
                    width: right - left,
                    ..bounds
                }),
                p.primary,
            );
        }
    }
}

impl<'a, Message, Renderer> From<Indeterminate> for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Renderer: iced_core::Renderer + 'a,
{
    fn from(bar: Indeterminate) -> Self {
        Element::new(bar)
    }
}

/// Dialogs that arrived while another was open. A new dialog never
/// replaces one a person is answering (the pending decision would be
/// lost): it waits, first in first out, and shows once nothing is open.
#[derive(Debug, Clone)]
pub struct ModalQueue<T> {
    waiting: VecDeque<T>,
}

impl<T> Default for ModalQueue<T> {
    fn default() -> Self {
        Self {
            waiting: VecDeque::new(),
        }
    }
}

impl<T> ModalQueue<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Shows `dialog` now if nothing is open, else queues it.
    pub fn offer(&mut self, current: &mut Option<T>, dialog: T) {
        match current {
            Some(_) => self.waiting.push_back(dialog),
            None => *current = Some(dialog),
        }
    }

    /// Once nothing is open, shows the oldest queued dialog that is still
    /// `relevant` (what it asked about may have gone meanwhile), dropping
    /// stale ones on the way.
    pub fn next(&mut self, current: &mut Option<T>, relevant: impl Fn(&T) -> bool) {
        while current.is_none() {
            let Some(dialog) = self.waiting.pop_front() else {
                break;
            };
            if relevant(&dialog) {
                *current = Some(dialog);
            }
        }
    }

    /// Removes every queued dialog that is not `relevant`.
    pub fn retain(&mut self, relevant: impl Fn(&T) -> bool) {
        self.waiting.retain(|dialog| relevant(dialog));
    }

    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.waiting.iter()
    }

    pub fn len(&self) -> usize {
        self.waiting.len()
    }

    pub fn is_empty(&self) -> bool {
        self.waiting.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persistent_modal_host_restores_selection_and_undo_after_a_queue() {
        use crate::test_renderer::LayoutRenderer;
        use iced_core::shell::{Bus, Waker};
        use iced_core::window::Headless;
        #[derive(Debug, Clone, PartialEq)]
        enum Message {
            Input(String),
            Dialog(Event),
        }
        fn key(key: Key, modifiers: Modifiers, text: Option<&str>) -> CoreEvent {
            CoreEvent::Keyboard(keyboard::Event::KeyPressed {
                key: key.clone(),
                modified_key: key,
                physical_key: keyboard::key::Physical::Unidentified(
                    keyboard::key::NativeCode::Unidentified,
                ),
                location: keyboard::Location::Standard,
                modifiers,
                text: text.map(Into::into),
                repeat: false,
            })
        }
        fn view<'a>(
            value: &'a str,
            dialog: Option<&'a Dialog>,
        ) -> Element<'a, Message, Theme, LayoutRenderer> {
            let base = crate::TextField::new("", value)
                .id(widget::Id::new("base-field"))
                .on_input(Message::Input);
            modal_host(base, dialog, Tokens::dark(), Message::Dialog)
        }
        fn build(
            element: &mut Element<'_, Message, Theme, LayoutRenderer>,
            tree: &mut Tree,
        ) -> layout::Node {
            tree.diff(element.as_widget_mut());
            element.as_widget_mut().layout(
                tree,
                &LayoutRenderer::new(),
                &layout::Limits::new(Size::ZERO, Size::new(800.0, 600.0)),
            )
        }
        fn send(
            element: &mut Element<'_, Message, Theme, LayoutRenderer>,
            tree: &mut Tree,
            node: &layout::Node,
            event: CoreEvent,
        ) -> Vec<Message> {
            let mut bus = Bus::new();
            let mut shell = Shell::new(&Headless, Waker::noop(), &mut bus);
            element.as_widget_mut().update(
                tree,
                &event,
                Layout::new(node),
                mouse::Cursor::Unavailable,
                &LayoutRenderer::new(),
                &mut shell,
                &Rectangle::with_size(Size::new(800.0, 600.0)),
            );
            bus.drain().collect()
        }
        let mut closed = view("draft", None);
        let mut tree = Tree::new(closed.as_widget());
        let node = build(&mut closed, &mut tree);
        let mut focus = operation::focusable::focus::<()>(widget::Id::new("base-field"));
        closed.as_widget_mut().operate(
            &mut tree,
            Layout::new(&node),
            &LayoutRenderer::new(),
            &mut focus,
        );
        send(
            &mut closed,
            &mut tree,
            &node,
            key(Key::Character("a".into()), Modifiers::CTRL, None),
        );
        send(
            &mut closed,
            &mut tree,
            &node,
            CoreEvent::InputMethod(iced_core::input_method::Event::Preedit(
                "界".into(),
                Some(0..3),
            )),
        );
        let first = Dialog::prompt("First", "Body");
        let mut opened = view("draft", Some(&first));
        let node = build(&mut opened, &mut tree);
        assert_eq!(
            send(
                &mut opened,
                &mut tree,
                &node,
                key(Key::Named(Named::Escape), Modifiers::empty(), None)
            ),
            vec![Message::Dialog(Event::Cancel)]
        );
        let queued = Dialog::confirm("Second", "Body");
        let mut opened = view("draft", Some(&queued));
        let node = build(&mut opened, &mut tree);
        send(
            &mut opened,
            &mut tree,
            &node,
            key(Key::Named(Named::Enter), Modifiers::empty(), None),
        );
        let mut closed = view("draft", None);
        let node = build(&mut closed, &mut tree);
        assert_eq!(
            send(
                &mut closed,
                &mut tree,
                &node,
                key(Key::Character("x".into()), Modifiers::empty(), Some("x"))
            ),
            vec![Message::Input("x".into())]
        );
        let mut closed = view("x", None);
        let node = build(&mut closed, &mut tree);
        assert_eq!(
            send(
                &mut closed,
                &mut tree,
                &node,
                key(Key::Character("z".into()), Modifiers::CTRL, None)
            ),
            vec![Message::Input("draft".into())]
        );
    }

    fn tab(dialog: &mut Dialog) -> Option<Outcome> {
        let event = dialog
            .key(&Key::Named(Named::Tab), Modifiers::empty())
            .expect("Tab always moves focus");
        dialog.update(event)
    }

    fn enter(dialog: &mut Dialog) -> Option<Outcome> {
        let event = dialog
            .key(&Key::Named(Named::Enter), Modifiers::empty())
            .expect("Enter always activates");
        dialog.update(event)
    }

    fn escape(dialog: &mut Dialog) -> Option<Outcome> {
        dialog
            .key(&Key::Named(Named::Escape), Modifiers::empty())
            .and_then(|event| dialog.update(event))
    }

    #[test]
    fn a_queued_dialog_never_replaces_an_open_one() {
        let mut queue = ModalQueue::new();
        let mut open = None;
        queue.offer(&mut open, 1);
        assert_eq!(open, Some(1), "nothing open: shown at once");
        queue.offer(&mut open, 2);
        queue.offer(&mut open, 3);
        queue.offer(&mut open, 4);
        assert_eq!(
            (open, queue.len()),
            (Some(1), 3),
            "the open one stays; the rest wait"
        );
        queue.next(&mut open, |_| true);
        assert_eq!(open, Some(1), "nothing shows over an open dialog");
        open = None;
        queue.next(&mut open, |dialog| *dialog != 2);
        assert_eq!(
            (open, queue.len()),
            (Some(3), 1),
            "oldest first; a stale one is skipped"
        );
        open = None;
        queue.next(&mut open, |_| true);
        assert_eq!(open, Some(4));
        open = None;
        queue.next(&mut open, |_| true);
        assert!(open.is_none() && queue.is_empty());
        queue.offer(&mut Some(0), 5);
        queue.offer(&mut Some(0), 6);
        queue.retain(|dialog| *dialog == 6);
        assert_eq!(queue.iter().copied().collect::<Vec<_>>(), [6]);
    }

    #[test]
    fn confirm_is_keyboard_operable() {
        let mut dialog = Dialog::confirm("Delete?", "This cannot be undone.");
        assert_eq!(dialog.slots(), 2);
        assert_eq!(dialog.focus(), 0, "the primary button has focus");
        assert_eq!(dialog.focused_button(), Some(0));
        assert_eq!(
            dialog.focus_target(),
            None,
            "buttons are not iced focusables"
        );
        assert_eq!(tab(&mut dialog), None);
        assert_eq!(dialog.focused_button(), Some(1), "Tab moves to Cancel");
        assert_eq!(tab(&mut dialog), None);
        assert_eq!(dialog.focused_button(), Some(0), "and wraps");
        assert_eq!(
            dialog.key(&Key::Named(Named::Tab), Modifiers::SHIFT),
            Some(Event::FocusPrevious)
        );
        assert_eq!(dialog.update(Event::FocusPrevious), None);
        assert_eq!(dialog.focused_button(), Some(1));
        assert_eq!(
            dialog.key(&Key::Named(Named::ArrowLeft), Modifiers::empty()),
            Some(Event::FocusPrevious)
        );
        assert_eq!(
            enter(&mut dialog),
            Some(Outcome::Cancelled),
            "Enter on Cancel"
        );
        assert_eq!(dialog.update(Event::FocusPrevious), None);
        assert_eq!(dialog.focused_button(), Some(0));
        assert_eq!(enter(&mut dialog), Some(Outcome::Accepted), "Enter on OK");
        assert_eq!(
            dialog.key(&Key::Named(Named::Space), Modifiers::empty()),
            Some(Event::Activate)
        );
        assert_eq!(escape(&mut dialog), Some(Outcome::Cancelled));
        assert_eq!(
            dialog.update(Event::Cancel),
            Some(Outcome::Cancelled),
            "the scrim"
        );
        assert_eq!(dialog.update(Event::Button(1)), Some(Outcome::Cancelled));
        assert_eq!(dialog.update(Event::Button(0)), Some(Outcome::Accepted));
        assert_eq!(dialog.update(Event::Button(7)), None, "no such button");
        assert_eq!(dialog.update(Event::Up), None, "arrows mean nothing here");
    }

    #[test]
    fn custom_buttons_report_their_index() {
        let mut dialog = Dialog::confirm("Save changes?", "").buttons([
            Button::primary("Save"),
            Button::destructive("Discard"),
            Button::cancel("Keep editing"),
        ]);
        assert_eq!(dialog.default_button(), Some(0));
        assert_eq!(dialog.focused_button(), Some(0));
        assert_eq!(dialog.update(Event::Button(0)), Some(Outcome::Accepted));
        assert_eq!(dialog.update(Event::Button(1)), Some(Outcome::Button(1)));
        assert_eq!(dialog.update(Event::Button(2)), Some(Outcome::Cancelled));
        let no_primary =
            Dialog::confirm("", "").buttons([Button::cancel("No"), Button::destructive("Yes")]);
        assert_eq!(no_primary.default_button(), Some(1), "the first non-cancel");
        assert_eq!(no_primary.focused_button(), Some(1));
        let none = Dialog::confirm("", "").buttons(Vec::new());
        assert_eq!(none.default_button(), None);
        assert_eq!(none.slots(), 0);
        let mut none = none;
        assert_eq!(tab(&mut none), None, "nothing to focus");
        assert_eq!(enter(&mut none), None);
    }

    #[test]
    fn prompt_focuses_its_field_and_returns_the_text() {
        let mut dialog = Dialog::prompt("Name", "Pick a name.")
            .value("draft")
            .placeholder("name");
        assert_eq!(dialog.slots(), 3);
        assert!(dialog.is_leading_focused());
        assert_eq!(
            dialog.focus_target(),
            Some(widget::Id::new(FIELD_ID)),
            "the field is focused while it is the slot"
        );
        assert_eq!(dialog.focused_button(), None);
        assert_eq!(
            dialog.key(&Key::Named(Named::ArrowLeft), Modifiers::empty()),
            None,
            "the field keeps its arrows"
        );
        assert_eq!(dialog.update(Event::Input("final".into())), None);
        assert_eq!(dialog.text(), "final");
        assert_eq!(
            enter(&mut dialog),
            Some(Outcome::Text("final".into())),
            "Enter in the field"
        );
        assert_eq!(tab(&mut dialog), None);
        assert_eq!(dialog.focus_target(), None, "Tab unfocuses the field");
        assert_eq!(dialog.focused_button(), Some(0));
        assert_eq!(enter(&mut dialog), Some(Outcome::Text("final".into())));
        assert_eq!(tab(&mut dialog), None);
        assert_eq!(enter(&mut dialog), Some(Outcome::Cancelled));
        assert_eq!(tab(&mut dialog), None);
        assert!(dialog.is_leading_focused(), "wraps back to the field");

        dialog.set_error(Some("taken".into()));
        assert_eq!(dialog.error(), Some("taken"));
        assert_eq!(enter(&mut dialog), None, "an invalid prompt cannot submit");
        assert_eq!(dialog.update(Event::Button(0)), None);
        assert_eq!(
            dialog.update(Event::Button(1)),
            Some(Outcome::Cancelled),
            "but can cancel"
        );
        assert_eq!(dialog.update(Event::Input("other".into())), None);
        assert_eq!(dialog.error(), None, "typing clears the error");

        let secret = Dialog::secret("Passphrase", "");
        assert_eq!(secret.kind(), Kind::Secret);
        assert_eq!(secret.focus_target(), Some(widget::Id::new(FIELD_ID)));
    }

    #[test]
    fn choice_moves_with_arrows_and_returns_the_index() {
        let mut dialog =
            Dialog::choice("Open with", "", ["Viewer", "Editor", "Terminal"]).selected(5);
        assert_eq!(dialog.selection(), 2, "clamped");
        let mut dialog2 = Dialog::choice("Open with", "", ["Viewer", "Editor", "Terminal"]);
        assert_eq!(dialog2.selection(), 0);
        assert!(dialog2.is_leading_focused());
        assert_eq!(
            dialog2.key(&Key::Named(Named::ArrowDown), Modifiers::empty()),
            Some(Event::Down)
        );
        assert_eq!(dialog2.update(Event::Down), None);
        assert_eq!(dialog2.update(Event::Down), None);
        assert_eq!(dialog2.update(Event::Down), None);
        assert_eq!(dialog2.selection(), 2, "stops at the end");
        assert_eq!(dialog2.update(Event::Up), None);
        assert_eq!(dialog2.selection(), 1);
        assert_eq!(
            enter(&mut dialog2),
            Some(Outcome::Chosen(1)),
            "Enter in the list"
        );
        assert_eq!(dialog2.update(Event::Select(0)), None);
        assert_eq!(dialog2.selection(), 0);
        assert_eq!(dialog2.update(Event::Select(9)), None);
        assert_eq!(dialog2.selection(), 0, "no such option");
        assert_eq!(tab(&mut dialog2), None);
        assert_eq!(
            dialog2.key(&Key::Named(Named::ArrowDown), Modifiers::empty()),
            None,
            "arrows only move the list while it has focus"
        );
        assert_eq!(enter(&mut dialog2), Some(Outcome::Chosen(0)), "Enter on OK");
        assert_eq!(dialog.update(Event::Up), None);
        assert_eq!(dialog.selection(), 1);
    }

    #[test]
    fn message_and_progress_have_the_right_buttons() {
        let mut message = Dialog::message("Saved", "All good.").severity(Severity::Success);
        assert_eq!(message.kind(), Kind::Message(Severity::Success));
        assert_eq!(message.button_list().len(), 1);
        assert_eq!(message.button_list()[0].label, "Close");
        assert_eq!(enter(&mut message), Some(Outcome::Accepted));
        assert_eq!(escape(&mut message), Some(Outcome::Cancelled));
        let relabelled = Dialog::confirm("", "").strings(&Strings {
            ok: "Ja".into(),
            cancel: "Nein".into(),
            close: "Zu".into(),
        });
        assert_eq!(relabelled.button_list()[0].label, "Ja");
        assert_eq!(relabelled.button_list()[1].label, "Nein");
        assert_eq!(
            Dialog::message("", "")
                .strings(&Strings {
                    ok: "Ja".into(),
                    cancel: "Nein".into(),
                    close: "Zu".into(),
                })
                .button_list()[0]
                .label,
            "Zu"
        );
        assert_eq!(
            Dialog::confirm("", "").severity(Severity::Error).kind(),
            Kind::Confirm
        );

        let mut busy = Dialog::progress("Copying", "3 of 10 files");
        assert!(!busy.is_cancellable());
        assert!(busy.button_list().is_empty());
        assert_eq!(busy.slots(), 0);
        assert_eq!(escape(&mut busy), None, "Escape does nothing");
        assert_eq!(enter(&mut busy), None);
        assert_eq!(busy.update(Event::Cancel), None, "nor the scrim");
        assert_eq!(busy.progress_state(), Progress::Indeterminate);
        busy.set_progress(Progress::Fraction(0.3));
        busy.set_body("4 of 10 files");
        assert_eq!(busy.progress_state(), Progress::Fraction(0.3));
        assert_eq!(busy.body(), "4 of 10 files");
        let mut cancellable = Dialog::progress("Copying", "").cancellable(true);
        assert_eq!(cancellable.button_list().len(), 1);
        assert_eq!(cancellable.focused_button(), Some(0));
        assert_eq!(escape(&mut cancellable), Some(Outcome::Cancelled));
        assert_eq!(enter(&mut cancellable), Some(Outcome::Cancelled));
        let mut uncancellable = Dialog::confirm("", "").cancellable(false);
        assert_eq!(uncancellable.update(Event::Cancel), None);
        assert_eq!(uncancellable.button_list().len(), 2, "its buttons stay");
    }

    #[test]
    fn the_indeterminate_segment_sweeps_left_to_right() {
        assert_eq!(Indeterminate::phase(0.0), -SEGMENT);
        assert!(
            (Indeterminate::phase(SWEEP) - -SEGMENT).abs() < 1e-5,
            "periodic"
        );
        assert!(
            (Indeterminate::phase(SWEEP / 2.0) - (-SEGMENT + (1.0 + SEGMENT) / 2.0)).abs() < 1e-5
        );
        assert!(Indeterminate::phase(SWEEP * 0.999) > 0.9);
    }

    #[test]
    fn the_styles_come_from_the_theme() {
        let theme = Theme::dark();
        let p = theme.palette();
        assert_eq!(Severity::Error.colour(&theme), p.destructive);
        assert_eq!(Severity::Success.colour(&theme), theme.semantic().success);
        assert_eq!(Severity::Info.colour(&theme), p.text);
        assert_ne!(Severity::Warning.colour(&theme), p.destructive);
        assert_eq!(card(&theme).background, Some(p.popover.into()));
        assert_eq!(
            scrim(&theme).background,
            Some(
                Color {
                    a: 0.55,
                    ..p.surface
                }
                .into()
            )
        );
        let light = Theme::light();
        assert_eq!(
            scrim(&light).background,
            Some(
                Color {
                    a: 0.55,
                    ..light.palette().text
                }
                .into()
            ),
            "the scrim darkens a light theme too"
        );
        let focused = button_style(&theme, button::Status::Active, Role::Primary, true);
        assert_eq!(focused.border.color, p.ring);
        assert_eq!(focused.background, Some(p.primary.into()));
        let plain = button_style(&theme, button::Status::Active, Role::Cancel, false);
        assert_ne!(plain.border.color, p.ring);
        let selected = option_style(&theme, button::Status::Active, true, true);
        assert_eq!(selected.background, Some(p.selection.into()));
        assert_eq!(selected.border.color, p.ring);
        assert_eq!(
            option_style(&theme, button::Status::Active, false, false).background,
            None
        );
    }
}

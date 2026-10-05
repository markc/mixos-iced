// SPDX-License-Identifier: MIT OR Apache-2.0
//! Toasts: short notices stacked in a corner over the window, each with a
//! severity, an optional action button, a dismiss button and a lifetime.
//!
//! A [`Toaster`] is plain state the application owns: [`Toaster::push`]
//! adds a toast and returns its id and deadline; [`overlay`] draws the
//! stack over the application's content; the [`Event`]s it publishes go
//! back through [`Toaster::update`]. Expiry is driven by the application,
//! either from its own timer with [`Toaster::sweep`], or by letting the
//! overlay watch the deadlines: it asks for a redraw at the nearest one
//! and publishes [`Event::Expired`] when it passes, which costs nothing
//! while no toast is showing. No timer runs outside the application's
//! update loop.

use iced_core::time::{Duration, Instant};
use iced_core::widget::{Operation, Tree, tree};
use iced_core::{
    Border, Color, Element, Event as CoreEvent, Layout, Length, Padding, Rectangle, Shadow, Shell,
    Size, Vector, Widget, layout, mouse, overlay, renderer, text,
};
use iced_widget::{button, column, container, row, space, stack, text as text_widget};

use crate::Tokens;
pub use crate::dialog::Severity;
use crate::dialog::darker;
use crate::keys::forward_to_content;
use crate::theme::{self, Theme};

/// Identifies a toast for the life of a [`Toaster`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ToastId(u64);

/// How long a toast stays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timeout {
    /// The toaster's default.
    Default,
    /// Until dismissed.
    Never,
    After(Duration),
}

/// What a toast shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    title: String,
    body: String,
    severity: Severity,
    action: Option<String>,
    timeout: Timeout,
    dismissable: bool,
}

impl Toast {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            body: String::new(),
            severity: Severity::Info,
            action: None,
            timeout: Timeout::Default,
            dismissable: true,
        }
    }

    pub fn body(mut self, body: impl Into<String>) -> Self {
        self.body = body.into();
        self
    }

    pub fn severity(mut self, severity: Severity) -> Self {
        self.severity = severity;
        self
    }

    /// A button; pressing it answers [`Event::Action`] and removes the
    /// toast.
    pub fn action(mut self, label: impl Into<String>) -> Self {
        self.action = Some(label.into());
        self
    }

    pub fn timeout(mut self, timeout: Timeout) -> Self {
        self.timeout = timeout;
        self
    }

    /// Stays until dismissed.
    pub fn sticky(self) -> Self {
        self.timeout(Timeout::Never)
    }

    /// Whether the dismiss button is shown (it is by default).
    pub fn dismissable(mut self, dismissable: bool) -> Self {
        self.dismissable = dismissable;
        self
    }

    pub fn title_text(&self) -> &str {
        &self.title
    }

    pub fn body_text(&self) -> &str {
        &self.body
    }

    pub fn severity_level(&self) -> Severity {
        self.severity
    }

    pub fn action_label(&self) -> Option<&str> {
        self.action.as_deref()
    }

    pub fn is_dismissable(&self) -> bool {
        self.dismissable
    }
}

/// What the overlay reports back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// The dismiss button.
    Dismiss(ToastId),
    /// The action button.
    Action(ToastId),
    /// The toast's deadline passed.
    Expired(ToastId),
}

/// What [`Toaster::push`] returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Handle {
    pub id: ToastId,
    /// When the toast expires, if it does.
    pub deadline: Option<Instant>,
}

#[derive(Debug, Clone)]
struct Entry {
    id: ToastId,
    toast: Toast,
    deadline: Option<Instant>,
}

/// The toasts on show, oldest first. Pushing past the limit drops the
/// oldest.
#[derive(Debug, Clone)]
pub struct Toaster {
    entries: Vec<Entry>,
    next: u64,
    limit: usize,
    default_timeout: Option<Duration>,
}

impl Default for Toaster {
    fn default() -> Self {
        Self::new()
    }
}

impl Toaster {
    /// At most five toasts, five seconds each by default.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            next: 1,
            limit: 5,
            default_timeout: Some(Duration::from_secs(5)),
        }
    }

    /// The most toasts shown at once (at least one).
    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = limit.max(1);
        self
    }

    /// The lifetime of a toast with [`Timeout::Default`]; `None` keeps
    /// them until dismissed.
    pub fn default_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.default_timeout = timeout;
        self
    }

    /// Shows `toast` now.
    pub fn push(&mut self, toast: Toast) -> Handle {
        self.push_at(toast, Instant::now())
    }

    /// Shows `toast` as of `now`, from which its deadline counts.
    pub fn push_at(&mut self, toast: Toast, now: Instant) -> Handle {
        let id = ToastId(self.next);
        self.next += 1;
        let lifetime = match toast.timeout {
            Timeout::Default => self.default_timeout,
            Timeout::Never => None,
            Timeout::After(duration) => Some(duration),
        };
        let deadline = lifetime.map(|lifetime| now + lifetime);
        self.entries.push(Entry {
            id,
            toast,
            deadline,
        });
        if self.entries.len() > self.limit {
            let excess = self.entries.len() - self.limit;
            self.entries.drain(..excess);
        }
        Handle { id, deadline }
    }

    /// Removes a toast; `false` if it was already gone.
    pub fn dismiss(&mut self, id: ToastId) -> bool {
        let before = self.entries.len();
        self.entries.retain(|entry| entry.id != id);
        self.entries.len() < before
    }

    /// Applies an overlay event. An action returns its toast's id for the
    /// application to act on; every event removes the toast.
    pub fn update(&mut self, event: Event) -> Option<ToastId> {
        match event {
            Event::Dismiss(id) | Event::Expired(id) => {
                self.dismiss(id);
                None
            }
            Event::Action(id) => self.dismiss(id).then_some(id),
        }
    }

    /// Removes every toast whose deadline has passed, returning their ids.
    pub fn sweep(&mut self, now: Instant) -> Vec<ToastId> {
        let (due, kept): (Vec<_>, Vec<_>) = self
            .entries
            .drain(..)
            .partition(|entry| entry.deadline.is_some_and(|deadline| deadline <= now));
        self.entries = kept;
        due.into_iter().map(|entry| entry.id).collect()
    }

    /// The nearest deadline, for a timer.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.entries.iter().filter_map(|entry| entry.deadline).min()
    }

    pub fn deadline(&self, id: ToastId) -> Option<Instant> {
        self.entries
            .iter()
            .find(|entry| entry.id == id)
            .and_then(|entry| entry.deadline)
    }

    pub fn get(&self, id: ToastId) -> Option<&Toast> {
        self.entries
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| &entry.toast)
    }

    pub fn contains(&self, id: ToastId) -> bool {
        self.entries.iter().any(|entry| entry.id == id)
    }

    /// The toasts on show, oldest first.
    pub fn iter(&self) -> impl Iterator<Item = (ToastId, &Toast)> {
        self.entries.iter().map(|entry| (entry.id, &entry.toast))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn deadlines(&self) -> Vec<(ToastId, Instant)> {
        self.entries
            .iter()
            .filter_map(|entry| entry.deadline.map(|deadline| (entry.id, deadline)))
            .collect()
    }
}

/// A toast card: the `elevated` pair, outlined in the severity's colour.
pub fn style(theme: &Theme, severity: Severity) -> container::Style {
    let p = theme.palette();
    let m = theme.metrics();
    let accent = severity.colour(theme);
    let emphasised = severity != Severity::Info;
    container::Style {
        background: Some(p.elevated.into()),
        text_color: Some(p.elevated_text),
        border: Border {
            color: if emphasised { accent } else { p.border },
            width: if emphasised {
                m.border.focus_width
            } else {
                m.border.width
            },
            radius: m.radius.md.into(),
        },
        shadow: Shadow {
            color: Color {
                a: 0.3,
                ..darker(p.surface, p.text)
            },
            offset: Vector::new(0.0, m.spacing.xs),
            blur_radius: m.spacing.lg,
        },
        ..container::Style::default()
    }
}

/// The card width in logical pixels.
pub const WIDTH: f32 = 320.0;

/// One toast card.
pub fn card<Renderer>(
    id: ToastId,
    toast: &Toast,
    tokens: Tokens,
) -> Element<'_, Event, Theme, Renderer>
where
    Renderer: text::Renderer + 'static,
{
    let m = tokens.metrics;
    let severity = toast.severity;
    let title = text_widget(toast.title.as_str())
        .size(m.text.md)
        .style(move |theme: &Theme| text_widget::Style {
            color: Some(severity.colour(theme)),
        });
    let mut head = row![title, space::horizontal()]
        .spacing(m.spacing.sm)
        .align_y(iced_core::alignment::Vertical::Center);
    if toast.dismissable {
        head = head.push(
            button(text_widget("\u{00d7}").size(m.text.md))
                .padding(Padding {
                    top: 0.0,
                    right: m.spacing.sm,
                    bottom: 0.0,
                    left: m.spacing.sm,
                })
                .style(theme::button::text)
                .on_press(Event::Dismiss(id)),
        );
    }
    let mut content = column![head].spacing(m.spacing.sm).width(Length::Fill);
    if !toast.body.is_empty() {
        content = content.push(text_widget(toast.body.as_str()).size(m.text.sm));
    }
    if let Some(action) = &toast.action {
        content = content.push(
            row![
                space::horizontal(),
                button(text_widget(action.as_str()).size(m.text.sm))
                    .padding(Padding {
                        top: m.spacing.xs,
                        right: m.spacing.md,
                        bottom: m.spacing.xs,
                        left: m.spacing.md,
                    })
                    .style(theme::button::secondary)
                    .on_press(Event::Action(id)),
            ]
            .spacing(m.spacing.sm),
        );
    }
    container(content)
        .padding(m.spacing.md)
        .width(Length::Fixed(WIDTH))
        .style(move |theme: &Theme| style(theme, severity))
        .into()
}

/// The toasts stacked in the bottom-right corner over `base`, newest at
/// the bottom. Deadlines are watched: a redraw is requested at the
/// nearest one and [`Event::Expired`] is published when it passes. With no
/// toast showing, `base` is returned as it is.
pub fn overlay<'a, Message, Renderer>(
    base: impl Into<Element<'a, Message, Theme, Renderer>>,
    toaster: &'a Toaster,
    tokens: Tokens,
    on_event: impl Fn(Event) -> Message + Clone + 'a,
) -> Element<'a, Message, Theme, Renderer>
where
    Message: Clone + 'a,
    Renderer: text::Renderer + 'static,
{
    if toaster.is_empty() {
        return base.into();
    }
    let m = tokens.metrics;
    let cards = toaster
        .iter()
        .map(|(id, toast)| card(id, toast, tokens).map(on_event.clone()));
    let corner = container(column(cards).spacing(m.spacing.sm).width(Length::Shrink))
        .align_right(Length::Fill)
        .align_bottom(Length::Fill)
        .padding(m.spacing.lg);
    let stacked = stack([base.into(), corner.into()]);
    Expiry {
        content: stacked.into(),
        deadlines: toaster.deadlines(),
        on_expire: Box::new(move |id| on_event(Event::Expired(id))),
    }
    .into()
}

/// Publishes an expiry for each deadline that has passed at a redraw, and
/// asks for the next redraw at the nearest one still to come.
struct Expiry<'a, Message, Theme, Renderer> {
    content: Element<'a, Message, Theme, Renderer>,
    deadlines: Vec<(ToastId, Instant)>,
    on_expire: Box<dyn Fn(ToastId) -> Message + 'a>,
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for Expiry<'_, Message, Theme, Renderer>
where
    Renderer: iced_core::Renderer,
{
    forward_to_content!();

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
        if let CoreEvent::Window(iced_core::window::Event::RedrawRequested(now)) = event {
            let mut next: Option<Instant> = None;
            for (id, deadline) in &self.deadlines {
                if *deadline <= *now {
                    shell.publish((self.on_expire)(*id));
                } else {
                    next = Some(next.map_or(*deadline, |soonest| soonest.min(*deadline)));
                }
            }
            if let Some(at) = next {
                shell.request_redraw_at(at);
            }
        }
        self.content
            .as_widget_mut()
            .update(tree, event, layout, cursor, renderer, shell, viewport);
    }
}

impl<'a, Message, Theme, Renderer> From<Expiry<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: 'a,
    Renderer: iced_core::Renderer + 'a,
{
    fn from(expiry: Expiry<'a, Message, Theme, Renderer>) -> Self {
        Element::new(expiry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(start: Instant, seconds: u64) -> Instant {
        start + Duration::from_secs(seconds)
    }

    #[test]
    fn toasts_expire_in_order_and_the_limit_drops_the_oldest() {
        let start = Instant::now();
        let mut toaster = Toaster::new().limit(3);
        let first = toaster.push_at(Toast::new("one"), start);
        let second = toaster.push_at(
            Toast::new("two").timeout(Timeout::After(Duration::from_secs(2))),
            at(start, 1),
        );
        let sticky = toaster.push_at(Toast::new("three").sticky(), at(start, 1));
        assert_eq!(first.deadline, Some(at(start, 5)));
        assert_eq!(second.deadline, Some(at(start, 3)));
        assert_eq!(sticky.deadline, None);
        assert_eq!(toaster.next_deadline(), Some(at(start, 3)));
        assert_eq!(
            toaster
                .iter()
                .map(|(_, toast)| toast.title_text())
                .collect::<Vec<_>>(),
            ["one", "two", "three"],
            "oldest first"
        );
        assert_eq!(toaster.sweep(at(start, 2)), Vec::<ToastId>::new());
        assert_eq!(toaster.sweep(at(start, 3)), [second.id]);
        assert_eq!(toaster.len(), 2);
        assert_eq!(toaster.next_deadline(), Some(at(start, 5)));
        assert_eq!(toaster.sweep(at(start, 60)), [first.id]);
        assert_eq!(
            toaster.next_deadline(),
            None,
            "a sticky toast has no deadline"
        );
        assert!(toaster.contains(sticky.id));

        let fourth = toaster.push_at(Toast::new("four"), start);
        let fifth = toaster.push_at(Toast::new("five"), start);
        assert_eq!(toaster.len(), 3);
        let sixth = toaster.push_at(Toast::new("six"), start);
        assert_eq!(toaster.len(), 3, "the limit holds");
        assert!(!toaster.contains(sticky.id), "the oldest went");
        assert_eq!(
            toaster.iter().map(|(id, _)| id).collect::<Vec<_>>(),
            [fourth.id, fifth.id, sixth.id]
        );
        assert!(fourth.id < fifth.id && fifth.id < sixth.id, "ids rise");
        assert_eq!(Toaster::new().limit(0).limit, 1);
    }

    #[test]
    fn events_remove_and_actions_report() {
        let mut toaster = Toaster::new().default_timeout(None);
        let plain = toaster.push(Toast::new("saved").severity(Severity::Success));
        let acting = toaster.push(
            Toast::new("deleted")
                .body("one file")
                .action("Undo")
                .dismissable(false),
        );
        assert_eq!(plain.deadline, None, "no default timeout");
        assert_eq!(
            toaster.get(acting.id).and_then(Toast::action_label),
            Some("Undo")
        );
        assert!(!toaster.get(acting.id).unwrap().is_dismissable());
        assert_eq!(toaster.get(acting.id).unwrap().body_text(), "one file");
        assert_eq!(
            toaster.get(plain.id).unwrap().severity_level(),
            Severity::Success
        );
        assert_eq!(toaster.update(Event::Dismiss(plain.id)), None);
        assert!(!toaster.contains(plain.id));
        assert_eq!(toaster.update(Event::Action(acting.id)), Some(acting.id));
        assert!(toaster.is_empty());
        assert_eq!(
            toaster.update(Event::Action(acting.id)),
            None,
            "already gone"
        );
        assert_eq!(toaster.update(Event::Expired(acting.id)), None);
        assert!(!toaster.dismiss(acting.id));
        assert_eq!(toaster.deadline(acting.id), None);
    }

    #[test]
    fn styles_follow_the_severity() {
        let theme = Theme::light();
        let p = theme.palette();
        assert_eq!(style(&theme, Severity::Info).border.color, p.border);
        assert_eq!(style(&theme, Severity::Error).border.color, p.destructive);
        assert_eq!(
            style(&theme, Severity::Success).border.color,
            theme.semantic().success
        );
        assert_eq!(
            style(&theme, Severity::Info).background,
            Some(p.elevated.into())
        );
        assert!(
            style(&theme, Severity::Warning).border.width
                > style(&theme, Severity::Info).border.width
        );
    }
}

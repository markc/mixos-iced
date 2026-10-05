// SPDX-License-Identifier: MIT OR Apache-2.0
//! A validated clock-time picker with 12/24-hour display and optional seconds.
//! The caller supplies the time, range and localised strings; there is no
//! dependency on a host clock or timezone.

use crate::{Theme, Tokens};
use iced_core::{
    Element, Length,
    keyboard::{Key, key::Named},
    widget::Id,
};
use iced_widget::{button, column, container, row, text, text_input};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Time {
    hour: u8,
    minute: u8,
    second: u8,
}

impl Time {
    pub const MIN: Self = Self {
        hour: 0,
        minute: 0,
        second: 0,
    };
    pub const MAX: Self = Self {
        hour: 23,
        minute: 59,
        second: 59,
    };
    pub const fn new(hour: u8, minute: u8, second: u8) -> Option<Self> {
        if hour < 24 && minute < 60 && second < 60 {
            Some(Self {
                hour,
                minute,
                second,
            })
        } else {
            None
        }
    }
    pub const fn hour(self) -> u8 {
        self.hour
    }
    pub const fn minute(self) -> u8 {
        self.minute
    }
    pub const fn second(self) -> u8 {
        self.second
    }
    fn seconds(self) -> i64 {
        i64::from(self.hour) * 3600 + i64::from(self.minute) * 60 + i64::from(self.second)
    }
    /// Clock arithmetic wraps at midnight, including large negative deltas.
    pub fn step(self, part: Part, delta: i32) -> Self {
        let unit = match part {
            Part::Hour => 3600,
            Part::Minute => 60,
            Part::Second => 1,
        };
        let total = (self.seconds() + i64::from(delta) * unit).rem_euclid(86400);
        Self {
            hour: (total / 3600) as u8,
            minute: ((total / 60) % 60) as u8,
            second: (total % 60) as u8,
        }
    }
}

/// Inclusive same-day range. Overnight ranges must be represented by the
/// caller as two ranges; inverted bounds are refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeRange {
    min: Time,
    max: Time,
}

impl TimeRange {
    pub fn new(min: Time, max: Time) -> Option<Self> {
        (min <= max).then_some(Self { min, max })
    }
    pub fn contains(self, time: Time) -> bool {
        (self.min..=self.max).contains(&time)
    }
}

impl Default for TimeRange {
    fn default() -> Self {
        Self {
            min: Time::MIN,
            max: Time::MAX,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Hour24,
    Hour12,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Hour,
    Minute,
    Second,
}

impl Part {
    fn index(self) -> usize {
        match self {
            Self::Hour => 0,
            Self::Minute => 1,
            Self::Second => 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Input(Part, String),
    Step(Part, i32),
    TogglePeriod,
    Submit,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Selected(Time),
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct Strings {
    pub hour: String,
    pub minute: String,
    pub second: String,
    pub am: String,
    pub pm: String,
    pub increment: String,
    pub decrement: String,
    pub invalid: String,
    pub submit: String,
    pub cancel: String,
}

#[derive(Debug, Clone)]
pub struct TimePicker {
    fields: [String; 3],
    ids: [Id; 3],
    pm: bool,
    format: Format,
    seconds: bool,
    range: TimeRange,
    committed: Time,
}

impl TimePicker {
    pub fn new(time: Time) -> Self {
        let mut picker = Self {
            fields: Default::default(),
            ids: std::array::from_fn(|_| Id::unique()),
            pm: false,
            format: Format::Hour24,
            seconds: true,
            range: TimeRange::default(),
            committed: time,
        };
        picker.set(time);
        picker
    }
    pub fn format(mut self, format: Format) -> Self {
        let time = self.selected().unwrap_or(self.committed);
        self.format = format;
        self.set(time);
        self
    }
    /// Hiding seconds retains the caller's second value.
    pub fn seconds(mut self, seconds: bool) -> Self {
        self.seconds = seconds;
        self
    }
    pub fn range(mut self, range: TimeRange) -> Self {
        self.range = range;
        self.committed = self.committed.clamp(range.min, range.max);
        self.set(self.committed);
        self
    }
    pub fn field(&self, part: Part) -> &str {
        &self.fields[part.index()]
    }

    pub fn field_id(&self, part: Part) -> Id {
        self.ids[part.index()].clone()
    }

    pub fn selected(&self) -> Option<Time> {
        let parse = |part: Part| {
            let field = self.field(part);
            (!field.is_empty() && field.bytes().all(|b| b.is_ascii_digit()))
                .then(|| field.parse::<u8>().ok())
                .flatten()
        };
        let mut hour = parse(Part::Hour)?;
        if self.format == Format::Hour12 {
            if !(1..=12).contains(&hour) {
                return None;
            }
            hour = hour % 12 + u8::from(self.pm) * 12;
        }
        let time = Time::new(hour, parse(Part::Minute)?, parse(Part::Second)?)?;
        self.range.contains(time).then_some(time)
    }

    fn set(&mut self, time: Time) {
        self.pm = time.hour >= 12;
        let hour = match self.format {
            Format::Hour24 => time.hour,
            Format::Hour12 => (time.hour + 11) % 12 + 1,
        };
        self.fields = [
            format!("{hour:02}"),
            format!("{:02}", time.minute),
            format!("{:02}", time.second),
        ];
    }

    pub fn update(&mut self, event: Event) -> Option<Outcome> {
        match event {
            Event::Input(part, value) => self.fields[part.index()] = value,
            Event::Step(part, delta) => {
                let time = self.selected().unwrap_or(self.committed).step(part, delta);
                self.set(time.clamp(self.range.min, self.range.max));
            }
            Event::TogglePeriod => self.pm = !self.pm,
            Event::Submit => {
                let time = self.selected()?;
                self.committed = time;
                return Some(Outcome::Selected(time));
            }
            Event::Cancel => return Some(Outcome::Cancelled),
        }
        None
    }

    pub fn view<'a, Message: Clone + 'a>(
        &'a self,
        strings: &'a Strings,
        tokens: Tokens,
        on_event: impl Fn(Event) -> Message + Clone + 'a,
    ) -> Element<'a, Message, Theme, iced_widget::Renderer> {
        let m = tokens.metrics;
        let mut fields = row![].spacing(m.spacing.sm);
        for part in [Part::Hour, Part::Minute, Part::Second] {
            if part == Part::Second && !self.seconds {
                continue;
            }
            let label = match part {
                Part::Hour => &strings.hour,
                Part::Minute => &strings.minute,
                Part::Second => &strings.second,
            };
            let id = self.field_id(part);
            let input_messages = on_event.clone();
            let field = text_input("", self.field(part))
                .id(id.clone())
                .size(m.text.lg)
                .padding(m.spacing.sm)
                .on_input(move |value| input_messages(Event::Input(part, value)))
                .on_submit(on_event(Event::Submit));
            let keys = on_event.clone();
            let field =
                crate::keys::keys(field, |_| None).on_key_before_focused(id, move |event| {
                    if let iced_core::keyboard::Event::KeyPressed { key, modifiers, .. } = event {
                        if modifiers.is_empty() {
                            return match key {
                                Key::Named(Named::ArrowUp) => Some(keys(Event::Step(part, 1))),
                                Key::Named(Named::ArrowDown) => Some(keys(Event::Step(part, -1))),
                                _ => None,
                            };
                        }
                    }
                    None
                });
            fields = fields.push(
                column![
                    text(label).size(m.text.sm),
                    button(text(&strings.increment))
                        .on_press(on_event(Event::Step(part, 1)))
                        .width(Length::Fill)
                        .style(crate::theme::button::text),
                    field,
                    button(text(&strings.decrement))
                        .on_press(on_event(Event::Step(part, -1)))
                        .width(Length::Fill)
                        .style(crate::theme::button::text),
                ]
                .spacing(m.spacing.xs)
                .width(Length::Fill),
            );
        }
        if self.format == Format::Hour12 {
            fields = fields.push(
                button(text(if self.pm { &strings.pm } else { &strings.am }))
                    .on_press(on_event(Event::TogglePeriod))
                    .style(crate::theme::button::secondary),
            );
        }
        let mut body = column![fields].spacing(m.spacing.md);
        if self.selected().is_none() {
            body = body.push(
                text(&strings.invalid)
                    .size(m.text.sm)
                    .style(|theme: &Theme| iced_core::widget::text::Style {
                        color: Some(theme.palette().destructive),
                    }),
            );
        }
        let mut submit = button(text(&strings.submit));
        if self.selected().is_some() {
            submit = submit.on_press(on_event(Event::Submit));
        }
        body = body.push(
            row![
                button(text(&strings.cancel))
                    .on_press(on_event(Event::Cancel))
                    .style(crate::theme::button::secondary),
                submit
            ]
            .spacing(m.spacing.sm),
        );
        let card = container(body)
            .padding(m.spacing.md)
            .width(Length::Fill.max(m.text.md * 24.0))
            .style(crate::theme::container::card);
        crate::keys::keys(card, move |event| {
            matches!(
                event,
                iced_core::keyboard::Event::KeyPressed {
                    key: Key::Named(Named::Escape),
                    ..
                }
            )
            .then(|| on_event(Event::Cancel))
        })
        .tab_navigation()
        .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn noon_midnight_invalid_drafts_and_extreme_steps() {
        assert!(Time::new(24, 0, 0).is_none());
        assert!(Time::new(0, 60, 0).is_none());
        assert_eq!(Time::MIN.step(Part::Second, -1), Time::MAX);
        assert_eq!(Time::MAX.step(Part::Second, 1), Time::MIN);
        assert!(TimeRange::default().contains(Time::MIN.step(Part::Hour, i32::MIN)));
        let mut picker = TimePicker::new(Time::MIN).format(Format::Hour12);
        assert_eq!(picker.field(Part::Hour), "12");
        picker.update(Event::TogglePeriod);
        assert_eq!(picker.selected(), Time::new(12, 0, 0));
        picker.update(Event::Input(Part::Hour, "0".into()));
        assert_eq!(picker.update(Event::Submit), None);
        for invalid in ["xx", "256", "-1", ""] {
            picker.update(Event::Input(Part::Hour, invalid.into()));
            assert_eq!(picker.selected(), None);
        }
        picker.update(Event::Step(Part::Minute, 1));
        assert_eq!(picker.selected(), Time::new(0, 1, 0));
        assert_eq!(
            picker.update(Event::Submit),
            Some(Outcome::Selected(Time::new(0, 1, 0).unwrap()))
        );
        assert_eq!(picker.update(Event::Cancel), Some(Outcome::Cancelled));
    }

    #[test]
    fn inclusive_ranges_and_hidden_seconds_are_retained() {
        let min = Time::new(9, 30, 15).unwrap();
        let max = Time::new(17, 0, 0).unwrap();
        assert!(TimeRange::new(max, min).is_none());
        let mut picker = TimePicker::new(Time::MIN)
            .range(TimeRange::new(min, max).unwrap())
            .seconds(false);
        assert_eq!(picker.selected(), Some(min));
        picker.update(Event::Step(Part::Hour, -1));
        assert_eq!(picker.selected(), Some(min));
        picker.update(Event::Input(Part::Hour, "18".into()));
        assert_eq!(picker.update(Event::Submit), None);
    }
}

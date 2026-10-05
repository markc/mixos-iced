// SPDX-License-Identifier: MIT OR Apache-2.0
//! A caller-owned Gregorian calendar picker. Dates are validated at the
//! boundary; no host clock, timezone, locale or platform dialog is required.
//! Mount [`DatePicker::view`] in a popover or modal host as appropriate.

use crate::{Theme, Tokens};
use iced_core::{
    Element, Length,
    keyboard::{Key, Modifiers, key::Named},
};
use iced_widget::{button, column, container, row, text};

/// A valid proleptic Gregorian date in years 1–9999.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Date {
    year: u16,
    month: u8,
    day: u8,
}

impl Date {
    pub const MIN: Self = Self {
        year: 1,
        month: 1,
        day: 1,
    };
    pub const MAX: Self = Self {
        year: 9999,
        month: 12,
        day: 31,
    };

    pub const fn new(year: u16, month: u8, day: u8) -> Option<Self> {
        if year == 0
            || year > 9999
            || month == 0
            || month > 12
            || day == 0
            || day > days_in_month(year, month)
        {
            None
        } else {
            Some(Self { year, month, day })
        }
    }

    pub const fn year(self) -> u16 {
        self.year
    }
    pub const fn month(self) -> u8 {
        self.month
    }
    pub const fn day(self) -> u8 {
        self.day
    }

    /// Zero-based weekday, Monday = 0.
    pub fn weekday(self) -> u8 {
        (self.ordinal() % 7) as u8
    }

    fn ordinal(self) -> i64 {
        let year = i64::from(self.year) - 1;
        let previous_months: i64 = (1..self.month)
            .map(|month| i64::from(days_in_month(self.year, month)))
            .sum();
        365 * year + year / 4 - year / 100 + year / 400 + previous_months + i64::from(self.day) - 1
    }

    /// Checked calendar arithmetic, bounded independently of the delta size.
    pub fn add_days(self, days: i32) -> Option<Self> {
        let target = self.ordinal() + i64::from(days);
        if !(0..=Self::MAX.ordinal()).contains(&target) {
            return None;
        }
        let (mut low, mut high) = (1u16, 10000u16);
        while high - low > 1 {
            let middle = low + (high - low) / 2;
            if (Self {
                year: middle,
                month: 1,
                day: 1,
            })
            .ordinal()
                <= target
            {
                low = middle;
            } else {
                high = middle;
            }
        }
        let mut day = target
            - (Self {
                year: low,
                month: 1,
                day: 1,
            })
            .ordinal();
        let mut month = 1;
        while day >= i64::from(days_in_month(low, month)) {
            day -= i64::from(days_in_month(low, month));
            month += 1;
        }
        Self::new(low, month, day as u8 + 1)
    }

    /// Month arithmetic clamps the day to the destination month's last day.
    pub fn add_months(self, months: i32) -> Option<Self> {
        let month = (i64::from(self.year) - 1) * 12 + i64::from(self.month) - 1 + i64::from(months);
        if !(0..9999 * 12).contains(&month) {
            return None;
        }
        let year = (month / 12 + 1) as u16;
        let month = (month % 12 + 1) as u8;
        Self::new(year, month, self.day.min(days_in_month(year, month)))
    }
}

pub const fn is_leap_year(year: u16) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

/// Zero for invalid months; callers cannot manufacture an invalid [`Date`].
pub const fn days_in_month(year: u16, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Inclusive selectable range, validated once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateRange {
    min: Date,
    max: Date,
}

impl DateRange {
    pub fn new(min: Date, max: Date) -> Option<Self> {
        (min <= max).then_some(Self { min, max })
    }
    pub fn contains(self, date: Date) -> bool {
        (self.min..=self.max).contains(&date)
    }
    fn clamp(self, date: Date) -> Date {
        date.clamp(self.min, self.max)
    }
}

impl Default for DateRange {
    fn default() -> Self {
        Self {
            min: Date::MIN,
            max: Date::MAX,
        }
    }
}

/// Weekday labels are Monday-first; display can start on another weekday.
#[derive(Debug, Clone)]
pub struct Strings {
    pub months: [String; 12],
    pub weekdays: [String; 7],
    pub previous: String,
    pub next: String,
    pub submit: String,
    pub cancel: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Select(Date),
    MoveDays(i32),
    MoveMonths(i32),
    StartOfWeek,
    EndOfWeek,
    Submit,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Selected(Date),
    Cancelled,
}

/// Draft selection changes do not commit until Submit. Cancel returns no date.
#[derive(Debug, Clone)]
pub struct DatePicker {
    selected: Date,
    range: DateRange,
    first_weekday: u8,
    id: iced_core::widget::Id,
}

impl DatePicker {
    pub fn new(date: Date) -> Self {
        Self {
            selected: date,
            range: DateRange::default(),
            first_weekday: 0,
            id: iced_core::widget::Id::unique(),
        }
    }
    pub fn range(mut self, range: DateRange) -> Self {
        self.range = range;
        self.selected = range.clamp(self.selected);
        self
    }
    /// Monday = 0 through Sunday = 6. Other values wrap modulo seven.
    pub fn first_weekday(mut self, day: u8) -> Self {
        self.first_weekday = day % 7;
        self
    }
    pub fn selected(&self) -> Date {
        self.selected
    }
    /// The calendar's focus ID; use a runtime focus operation on popup open.
    pub fn id(&self) -> iced_core::widget::Id {
        self.id.clone()
    }

    pub fn key(&self, key: &Key, modifiers: Modifiers) -> Option<Event> {
        if modifiers.control() || modifiers.alt() || modifiers.logo() {
            return None;
        }
        match key.as_ref() {
            Key::Named(Named::ArrowLeft) => Some(Event::MoveDays(-1)),
            Key::Named(Named::ArrowRight) => Some(Event::MoveDays(1)),
            Key::Named(Named::ArrowUp) => Some(Event::MoveDays(-7)),
            Key::Named(Named::ArrowDown) => Some(Event::MoveDays(7)),
            Key::Named(Named::PageUp) => {
                Some(Event::MoveMonths(if modifiers.shift() { -12 } else { -1 }))
            }
            Key::Named(Named::PageDown) => {
                Some(Event::MoveMonths(if modifiers.shift() { 12 } else { 1 }))
            }
            Key::Named(Named::Home) => Some(Event::StartOfWeek),
            Key::Named(Named::End) => Some(Event::EndOfWeek),
            Key::Named(Named::Enter) => Some(Event::Submit),
            Key::Named(Named::Escape) => Some(Event::Cancel),
            _ => None,
        }
    }

    pub fn update(&mut self, event: Event) -> Option<Outcome> {
        let offset = (self.selected.weekday() + 7 - self.first_weekday) % 7;
        let date = match event {
            Event::Select(date) if self.range.contains(date) => Some(date),
            Event::Select(_) => None,
            Event::MoveDays(days) => self.selected.add_days(days),
            Event::MoveMonths(months) => self.selected.add_months(months),
            Event::StartOfWeek => self.selected.add_days(-i32::from(offset)),
            Event::EndOfWeek => self.selected.add_days(i32::from(6 - offset)),
            Event::Submit => return Some(Outcome::Selected(self.selected)),
            Event::Cancel => return Some(Outcome::Cancelled),
        };
        if let Some(date) = date {
            self.selected = self.range.clamp(date);
        }
        None
    }

    pub fn view<'a, Message: Clone + 'a>(
        &'a self,
        strings: &'a Strings,
        tokens: Tokens,
        on_event: impl Fn(Event) -> Message + Clone + 'a,
    ) -> Element<'a, Message, Theme, iced_widget::Renderer> {
        let metrics = tokens.metrics;
        let first = Date {
            day: 1,
            ..self.selected
        };
        let offset = (first.weekday() + 7 - self.first_weekday) % 7;
        let heading = format!(
            "{} {}",
            strings.months[usize::from(first.month - 1)],
            first.year
        );
        let mut previous = button(text(&strings.previous)).style(crate::theme::button::text);
        if first.add_months(-1).is_some_and(|d| {
            d.add_months(1)
                .and_then(|next| next.add_days(-1))
                .is_some_and(|last| last >= self.range.min)
        }) {
            previous = previous.on_press(on_event(Event::MoveMonths(-1)));
        }
        let mut next = button(text(&strings.next)).style(crate::theme::button::text);
        if first.add_months(1).is_some_and(|d| d <= self.range.max) {
            next = next.on_press(on_event(Event::MoveMonths(1)));
        }
        let mut body = column![
            row![
                previous,
                container(text(heading)).center_x(Length::Fill),
                next
            ]
            .align_y(iced_core::alignment::Vertical::Center)
        ]
        .spacing(metrics.spacing.sm);
        let mut weekdays = row![].spacing(metrics.spacing.xs);
        for index in 0..7 {
            weekdays = weekdays.push(
                container(
                    text(&strings.weekdays[usize::from((index + self.first_weekday) % 7)])
                        .size(metrics.text.sm),
                )
                .center_x(Length::Fill),
            );
        }
        body = body.push(weekdays);
        let last = days_in_month(first.year, first.month);
        let cell_height = metrics.text.md + metrics.spacing.sm * 2.0;
        for week in 0..6i16 {
            let mut cells = row![].spacing(metrics.spacing.xs);
            for weekday in 0..7i16 {
                let day = week * 7 + weekday - i16::from(offset) + 1;
                if !(1..=i16::from(last)).contains(&day) {
                    cells = cells.push(
                        iced_widget::Space::new()
                            .width(Length::Fill)
                            .height(cell_height),
                    );
                    continue;
                }
                let date = Date {
                    day: day as u8,
                    ..first
                };
                let selected = date == self.selected;
                let mut cell = button(
                    container(text(day.to_string()).size(metrics.text.md)).center_x(Length::Fill),
                )
                .width(Length::Fill)
                .height(cell_height)
                .style(move |theme: &Theme, status| {
                    if selected {
                        crate::theme::button::primary(theme, status)
                    } else {
                        crate::theme::button::text(theme, status)
                    }
                });
                if self.range.contains(date) {
                    cell = cell.on_press(on_event(Event::Select(date)));
                }
                cells = cells.push(cell);
            }
            body = body.push(cells);
        }
        let actions = row![
            button(text(&strings.cancel))
                .on_press(on_event(Event::Cancel))
                .style(crate::theme::button::secondary),
            button(text(&strings.submit)).on_press(on_event(Event::Submit))
        ]
        .spacing(metrics.spacing.sm);
        body = body.push(container(actions).align_right(Length::Fill));
        let card = container(body)
            .padding(metrics.spacing.md)
            .width(Length::Fill.max(metrics.text.md * 24.0))
            .style(crate::theme::container::card);
        let keyboard = crate::keys::keys(card, |_| None).on_key_before(move |event| {
            if let iced_core::keyboard::Event::KeyPressed { key, modifiers, .. } = event {
                self.key(key, *modifiers).map(&on_event)
            } else {
                None
            }
        });
        crate::focus::region(keyboard, self.id.clone()).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_validation_boundaries_and_constant_cost_arithmetic() {
        assert!(Date::new(1900, 2, 29).is_none());
        assert!(Date::new(2000, 2, 29).is_some());
        assert!(Date::new(2026, 0, 1).is_none());
        assert!(Date::new(0, 1, 1).is_none());
        let leap = Date::new(2024, 2, 29).unwrap();
        assert_eq!(leap.add_months(12), Date::new(2025, 2, 28));
        assert_eq!(leap.add_days(1), Date::new(2024, 3, 1));
        assert_eq!(Date::new(2026, 10, 6).unwrap().weekday(), 1);
        assert_eq!(Date::MIN.add_days(-1), None);
        assert_eq!(Date::MAX.add_days(1), None);
        assert_eq!(leap.add_days(i32::MAX), None);
        assert_eq!(leap.add_months(i32::MIN), None);
        for year in [1, 4, 100, 400, 1900, 2000, 9999] {
            for month in 1..=12 {
                let date = Date::new(year, month, days_in_month(year, month)).unwrap();
                assert_eq!(date.add_days(0), Some(date));
                if let Some(next) = date.add_days(1) {
                    assert_eq!(next.add_days(-1), Some(date));
                }
            }
        }
    }

    #[test]
    fn range_draft_submission_and_week_ordering() {
        let start = Date::new(2026, 10, 5).unwrap();
        let end = Date::new(2026, 10, 10).unwrap();
        assert!(DateRange::new(end, start).is_none());
        let mut picker = DatePicker::new(Date::MIN)
            .range(DateRange::new(start, end).unwrap())
            .first_weekday(6);
        assert_eq!(picker.selected(), start);
        picker.update(Event::MoveDays(100));
        assert_eq!(picker.selected(), end);
        picker.update(Event::Select(Date::MAX));
        assert_eq!(picker.selected(), end);
        picker.update(Event::StartOfWeek);
        assert_eq!(
            picker.selected(),
            start,
            "Sunday lies before the allowed range"
        );
        assert_eq!(picker.update(Event::Cancel), Some(Outcome::Cancelled));
        assert_eq!(picker.update(Event::Submit), Some(Outcome::Selected(start)));
    }
}

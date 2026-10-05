// SPDX-License-Identifier: MIT OR Apache-2.0
//! Calendar and clock pickers with entirely caller-supplied strings.
use super::strings::label;
use toolkit::date_picker::{self, Date, DatePicker};
use toolkit::iced::{
    Element,
    widget::{column, row, text},
};
use toolkit::time_picker::{self, Format, Time, TimePicker};
use toolkit::{Theme, Tokens};

#[derive(Debug, Clone)]
pub enum Message {
    Date(date_picker::Event),
    Time(time_picker::Event),
}

pub struct State {
    date: DatePicker,
    time: TimePicker,
    date_strings: date_picker::Strings,
    time_strings: time_picker::Strings,
    outcome: String,
}

impl State {
    pub fn new() -> Self {
        Self {
            date: DatePicker::new(Date::new(2024, 2, 29).unwrap()),
            time: TimePicker::new(Time::new(12, 30, 45).unwrap()).format(Format::Hour12),
            date_strings: date_picker::Strings {
                months: std::array::from_fn(|index| {
                    label(&format!("calendar-month-{}", index + 1))
                }),
                weekdays: std::array::from_fn(|index| label(&format!("calendar-day-{index}"))),
                previous: label("picker-previous"),
                next: label("picker-next"),
                submit: label("picker-apply"),
                cancel: label("picker-cancel"),
            },
            time_strings: time_picker::Strings {
                hour: label("clock-hour"),
                minute: label("clock-minute"),
                second: label("clock-second"),
                am: label("clock-am"),
                pm: label("clock-pm"),
                increment: label("clock-up"),
                decrement: label("clock-down"),
                invalid: label("clock-invalid"),
                submit: label("picker-apply"),
                cancel: label("picker-cancel"),
            },
            outcome: String::new(),
        }
    }

    #[allow(dead_code)]
    pub fn date(&self) -> Date {
        self.date.selected()
    }
    #[allow(dead_code)]
    pub fn time(&self) -> Option<Time> {
        self.time.selected()
    }

    #[allow(dead_code)]
    pub fn time_field_id(&self, part: time_picker::Part) -> toolkit::core::widget::Id {
        self.time.field_id(part)
    }

    pub fn update(&mut self, message: Message) {
        match message {
            Message::Date(event) => {
                if let Some(outcome) = self.date.update(event) {
                    self.outcome = match outcome {
                        date_picker::Outcome::Cancelled => label("picker-cancelled"),
                        date_picker::Outcome::Selected(date) => {
                            format!("{:04}-{:02}-{:02}", date.year(), date.month(), date.day())
                        }
                    };
                }
            }
            Message::Time(event) => {
                if let Some(outcome) = self.time.update(event) {
                    self.outcome = match outcome {
                        time_picker::Outcome::Cancelled => label("picker-cancelled"),
                        time_picker::Outcome::Selected(time) => format!(
                            "{:02}:{:02}:{:02}",
                            time.hour(),
                            time.minute(),
                            time.second()
                        ),
                    };
                }
            }
        }
    }

    pub fn view(&self, tokens: Tokens) -> Element<'_, Message, Theme> {
        let content = column![
            text(label("picker-heading")).size(tokens.metrics.text.xxl),
            row![
                column![
                    text(label("calendar-heading")).size(tokens.metrics.text.lg),
                    self.date.view(&self.date_strings, tokens, Message::Date)
                ]
                .spacing(tokens.metrics.spacing.sm),
                column![
                    text(label("clock-heading")).size(tokens.metrics.text.lg),
                    self.time.view(&self.time_strings, tokens, Message::Time)
                ]
                .spacing(tokens.metrics.spacing.sm),
            ]
            .spacing(tokens.metrics.spacing.lg)
            .wrap(),
            text(&self.outcome).size(tokens.metrics.text.lg),
        ]
        .spacing(tokens.metrics.spacing.lg);
        toolkit::keys::keys(content, |_| None)
            .tab_navigation()
            .into()
    }
}

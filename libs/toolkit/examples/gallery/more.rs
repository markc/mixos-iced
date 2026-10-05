// SPDX-License-Identifier: MIT OR Apache-2.0
//! The "More widgets" page: the absorbed iced_aw set — badge, card,
//! labeled frame, slide bar, spinner, wrap, selection list and drop-down.
use toolkit::badge::Badge;
use toolkit::card::Card;
use toolkit::iced::widget::{button, column, container, row, text};
use toolkit::iced::{self, Center, Fill};
use toolkit::labeled_frame::LabeledFrame;
use toolkit::selection_list::SelectionList;
use toolkit::slide_bar::SlideBar;
use toolkit::spinner::Spinner;
use toolkit::theme::{self, Theme};
use toolkit::wrap::Wrap;
use toolkit::{DropDown, Tokens};

use super::strings::label;

pub type Element<'a> = iced::Element<'a, Message, Theme>;

/// Options the selection list shows.
const OPTIONS: [&str; 4] = ["Alpha", "Beta", "Gamma", "Delta"];

#[derive(Debug, Clone)]
pub enum Message {
    /// The card's close glyph.
    Close,
    /// The slide bar moved.
    Level(f32),
    /// The slide bar was released.
    LevelDone,
    /// An option was picked from the selection list.
    Picked(usize, String),
    /// The drop-down underlay was pressed.
    Toggle,
}

#[derive(Default)]
pub struct State {
    level: f32,
    expanded: bool,
    picked: Option<usize>,
    picked_value: Option<String>,
}

impl State {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update(&mut self, message: Message) {
        match message {
            Message::Close | Message::LevelDone => {}
            Message::Level(level) => self.level = level,
            Message::Picked(index, value) => {
                self.picked = Some(index);
                self.picked_value = Some(value);
            }
            Message::Toggle => self.expanded = !self.expanded,
        }
    }

    pub fn view(&self, tokens: Tokens) -> Element<'_> {
        let heading = tokens.metrics.text.xxl;
        let body = tokens.metrics.text.md;
        let pad = tokens.metrics.spacing.md;
        let gap = tokens.metrics.spacing.sm;

        let badges = row([
            Badge::new(text(label("badge-primary"))).padding(4).into(),
            Badge::new(text(label("badge-neutral")))
                .padding(4)
                .style(theme::badge::neutral)
                .into(),
            Badge::new(text(label("badge-destructive")))
                .padding(4)
                .style(theme::badge::destructive)
                .into(),
        ])
        .spacing(gap)
        .align_y(Center);

        let card = Card::new(
            text(label("card-head")).size(body),
            text(label("card-body")).size(body),
        )
        .foot(text(label("card-foot")).size(body))
        .on_close(Message::Close)
        .width(360);

        let frame = LabeledFrame::new(
            text(label("frame-title")).size(body),
            text(label("frame-body")).size(body),
        )
        .width(360);

        let slide = SlideBar::new(0.0..=1.0, self.level, Message::Level)
            .on_release(Message::LevelDone)
            .height(Some(iced::Length::Fixed(12.0)))
            .width(360);

        let spin = Spinner::new().width(24).height(24);

        let chips = Wrap::new()
            .spacing(gap)
            .padding(0)
            .width_items(iced::Length::Fill)
            .push(Badge::new(text(label("badge-primary"))).padding(4))
            .push(Badge::new(text(label("badge-neutral"))).padding(4))
            .push(Badge::new(text(label("badge-destructive"))).padding(4))
            .push(Badge::new(text("01")).padding(4))
            .push(Badge::new(text("02")).padding(4))
            .push(Badge::new(text("03")).padding(4));

        let options: Vec<String> = OPTIONS.iter().map(|option| (*option).to_owned()).collect();
        let mut list = SelectionList::new(options, Message::Picked).height(120);
        let picked_text = match (&self.picked, &self.picked_value) {
            (Some(_), Some(value)) => super::strings::format("picked-option", &[("option", value.clone())]),
            _ => String::new(),
        };
        if let Some(picked) = self.picked {
            list = list.selected(Some(picked));
        }

        let menu = column((0..3).map(|index| {
            text(super::strings::format(
                "drop-down-item",
                &[("index", index.to_string())],
            ))
            .size(body)
            .into()
        }))
        .padding(pad);
        let drop = DropDown::new(
            button(text(label(if self.expanded {
                "drop-down-close"
            } else {
                "drop-down-open"
            })))
            .on_press(Message::Toggle),
            container(menu).style(theme::container::popover),
            self.expanded,
        )
        .alignment(toolkit::drop_down::Alignment::Bottom)
        .on_dismiss(Message::Toggle);

        column![
            text(label("badges")).size(heading),
            badges,
            text(label("card")).size(heading),
            card,
            text(label("frames")).size(heading),
            frame,
            text(label("slide-bar")).size(heading),
            row![slide, spin].spacing(gap).align_y(Center),
            text(label("wrap")).size(heading),
            chips,
            text(label("selection-list")).size(heading),
            list,
            text(picked_text).size(body),
            text(label("drop-down")).size(heading),
            drop,
        ]
        .spacing(gap)
        .width(Fill)
        .into()
    }
}

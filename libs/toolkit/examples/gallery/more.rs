// SPDX-License-Identifier: MIT OR Apache-2.0
//! The "More widgets" page: the absorbed iced_aw set — badge, card,
//! labeled frame, slide bar, spinner, wrap, selection list and drop-down.
use toolkit::badge::Badge;
use toolkit::card::Card;
use toolkit::color_picker::{self, Hsv};
use toolkit::iced::widget::{button, column, container, row, text};
use toolkit::iced::{self, Center, Color, Fill};
use toolkit::labeled_frame::LabeledFrame;
use toolkit::number_input::NumberInput;
use toolkit::selection_list::SelectionList;
use toolkit::slide_bar::SlideBar;
use toolkit::spinner::Spinner;
use toolkit::tab_bar::TabLabel;
use toolkit::tabs::Tabs;
use toolkit::theme::{self, Theme};
use toolkit::wrap::Wrap;
use toolkit::{DropDown, Tokens};

use super::strings::label;

pub type Element<'a> = iced::Element<'a, Message, Theme>;

/// Options the selection list shows.
const OPTIONS: [&str; 4] = ["Alpha", "Beta", "Gamma", "Delta"];

/// The picker field and hue strip are square-ish; the swatch beside them.
const PICKER_SIDE: f32 = 176.0;
const HUE_STRIP: f32 = 24.0;
const SWATCH: f32 = 48.0;

#[derive(Debug, Clone)]
pub enum Message {
    /// The card's close glyph.
    Close,
    /// The slide bar moved.
    Level(f32),
    /// The slide bar was released.
    LevelDone,
    /// The number input changed.
    Amount(u32),
    /// An option was picked from the selection list.
    Picked(usize, String),
    /// A tab was selected.
    Tab(usize),
    /// The colour picker's field moved.
    ColorPicked(Hsv),
    /// The colour picker's hue strip moved.
    HuePicked(Hsv),
    /// The drop-down underlay was pressed.
    Toggle,
}

#[derive(Default)]
pub struct State {
    level: f32,
    amount: u32,
    expanded: bool,
    picked: Option<usize>,
    picked_value: Option<String>,
    tab: usize,
    /// The picked colour; `None` before the first pick — a pleasant
    /// default (teal-ish) shows until then.
    color: Option<Hsv>,
}

impl State {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update(&mut self, message: Message) {
        match message {
            Message::Close | Message::LevelDone => {}
            Message::Level(level) => self.level = level,
            Message::Amount(amount) => self.amount = amount,
            Message::Picked(index, value) => {
                self.picked = Some(index);
                self.picked_value = Some(value);
            }
            Message::Tab(tab) => self.tab = tab,
            Message::ColorPicked(field) => {
                self.color = Some(match self.color {
                    // The field sets saturation/value; keep the strip's hue.
                    Some(color) => Hsv {
                        s: field.s,
                        v: field.v,
                        ..color
                    },
                    None => field,
                });
            }
            Message::HuePicked(hue) => {
                self.color = Some(match self.color {
                    Some(color) => Hsv { h: hue.h, ..color },
                    None => hue,
                });
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
            (Some(_), Some(value)) => {
                super::strings::format("picked-option", &[("option", value.clone())])
            }
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

        let number = NumberInput::new(&self.amount, 0..=100, Message::Amount)
            .step(5)
            .width(180);

        let tabs = Tabs::new(Message::Tab)
            .push(
                0,
                TabLabel::Text(label("tab-first")),
                text(label("tab-first-body")).size(body),
            )
            .push(
                1,
                TabLabel::Text(label("tab-second")),
                text(label("tab-second-body")).size(body),
            )
            .set_active_tab(&self.tab);

        // The colour picker: a saturation/value field, a hue strip, and
        // a swatch of the picked colour. Before the first pick a default
        // shows, so the demo starts painted.
        let shown = self.color.unwrap_or_else(|| {
            let [r, g, b, _] = tokens.palette.primary.into_rgba8();
            Hsv::from_rgb8([r, g, b])
        });
        let field = color_picker::color_picker(shown, Message::ColorPicked)
            .spectrum(color_picker::saturation_value())
            .width(PICKER_SIDE)
            .height(PICKER_SIDE);
        let strip = color_picker::color_picker(shown, Message::HuePicked)
            .spectrum(color_picker::hue_vertical())
            .width(HUE_STRIP)
            .height(PICKER_SIDE);
        let picked_rgba = Color::from(shown);
        let swatch = container(iced::widget::Space::new())
            .width(SWATCH)
            .height(SWATCH)
            .style(move |_| iced::widget::container::Style {
                background: Some(picked_rgba.into()),
                border: iced::Border {
                    color: tokens.palette.border,
                    width: tokens.metrics.border.width,
                    radius: tokens.metrics.radius.sm.into(),
                },
                ..iced::widget::container::Style::default()
            });
        let picker_row = row![
            field,
            strip,
            column![
                swatch,
                text(format!(
                    "#{:02X}{:02X}{:02X}",
                    shown.to_rgb8()[0],
                    shown.to_rgb8()[1],
                    shown.to_rgb8()[2]
                ))
                .size(body)
            ]
            .spacing(gap)
        ]
        .spacing(gap)
        .align_y(Center);

        column![
            text(label("badges")).size(heading),
            badges,
            text(label("more-card")).size(heading),
            card,
            text(label("frames")).size(heading),
            frame,
            text(label("slide-bar")).size(heading),
            row![slide, spin].spacing(gap).align_y(Center),
            text(label("number-input")).size(heading),
            number,
            text(label("color-picker")).size(heading),
            picker_row,
            text(label("tabs")).size(heading),
            tabs,
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

// SPDX-License-Identifier: MIT OR Apache-2.0
//! The "Text" page: middle elision that keeps the extension, text that
//! solves its own size, and themed tooltips.
use toolkit::elide;
use toolkit::fit_text::FitText;
use toolkit::iced::widget::{column, container, row, text};
use toolkit::iced::{self, Center, Fill};
use toolkit::theme::{self, Theme};
use toolkit::tips;
use toolkit::Tokens;

use super::strings::{format, label};

/// The page is static; it never produces the app's messages.
pub type Element<'a> = iced::Element<'a, super::Message, Theme>;

/// Width of the elision cards: narrow enough to force a cut.
const CARD: f32 = 240.0;
/// Height of the fit-to-bounds box.
const FIT_HEIGHT: f32 = 96.0;

#[derive(Default)]
pub struct State;

impl State {
    pub fn new() -> Self {
        Self
    }

    pub fn view(&self, tokens: Tokens) -> Element<'static> {
        let heading = tokens.metrics.text.xxl;
        let body = tokens.metrics.text.md;
        let pad = tokens.metrics.spacing.md;
        let gap = tokens.metrics.spacing.sm;
        let card = move |content| {
            container(content)
                .width(CARD)
                .padding(pad)
                .style(theme::container::card)
        };
        let elided = column![
            elide::Label::new(label("text-sample-short")).size(body),
            elide::Label::new(label("text-sample-path")).size(body),
            elide::Label::new(label("text-sample-long")).size(body),
        ]
        .spacing(gap);
        let fitted = FitText::<Theme, iced::Renderer>::new(label("text-fit-sample"))
            .min_size(10)
            .max_size(72)
            .width(Fill)
            .height(FIT_HEIGHT)
            .center();
        let tipped = row((0..3).map(|index| {
            let tip = format("text-tip-region", &[("index", index.to_string())]);
            let content = container(text(label("text-tip-hover")))
                .padding(pad)
                .style(theme::container::elevated);
            tips::tip(&tokens, content, tip)
        }))
        .spacing(gap)
        .align_y(Center);
        column![
            text(label("text-elision")).size(heading),
            elided,
            text(label("text-fit")).size(heading),
            container(fitted)
                .width(Fill)
                .height(FIT_HEIGHT)
                .style(theme::container::card),
            text(label("text-tips")).size(heading),
            tipped,
        ]
        .spacing(gap)
        .into()
    }
}

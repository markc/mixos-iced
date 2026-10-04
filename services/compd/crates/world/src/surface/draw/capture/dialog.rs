//! `ContinueDialog` — the 5-minute keep-alive dialog for video captures. Shows
//! a countdown; the user must click Continue within the window or the capture
//! stops. Its own small centered screen-space instance.

use iced_core::{Alignment, Background, Border, Element, Length, Shadow, Theme};
use iced_widget::{button, column, container, row, text};
use crate::capture::session::message::CaptureMessage;
use ui::engine::{IcedUi, Renderer};

use crate::surface::draw::capture::style;

pub struct ContinueDialog {
    /// Seconds remaining before the capture auto-stops.
    pub seconds: u32,
}

impl ContinueDialog {
    pub fn new(seconds: u32) -> Self {
        Self { seconds }
    }
}

impl IcedUi for ContinueDialog {
    type Message = CaptureMessage;

    fn update(&mut self, message: Self::Message) {
        if let CaptureMessage::SetCountdown(s) = message {
            self.seconds = s;
        }
    }

    fn view(&self) -> Element<'_, Self::Message, Theme, Renderer> {
        let title = text("Still capturing?").size(20).color(style::TEXT);
        let sub = text(format!(
            "Continue within {}s or the capture will stop.",
            self.seconds
        ))
        .size(14)
        .color(style::TEXT_DIM);

        let buttons = row![
            button(text("Continue").size(15).color(style::TEXT))
                .padding([8, 18])
                .on_press(CaptureMessage::ContinueCapture)
                .style(style::button_with(style::ACCENT)),
            button(text("Stop").size(15).color(style::TEXT))
                .padding([8, 18])
                .on_press(CaptureMessage::StopFromDialog)
                .style(style::button_with(style::STOP_BG)),
        ]
        .spacing(12);

        let panel = container(
            column![title, sub, buttons]
                .spacing(14)
                .align_x(Alignment::Center),
        )
        .padding(24)
        .style(|_t| container::Style {
            background: Some(Background::Color(style::PANEL_BG)),
            text_color: Some(style::TEXT),
            border: Border {
                color: style::ACCENT,
                width: 1.0,
                radius: style::RADIUS.into(),
            },
            shadow: Shadow::default(),
            snap: true,
        });

        container(panel)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Alignment::Center)
            .align_y(Alignment::Center)
            .into()
    }
}

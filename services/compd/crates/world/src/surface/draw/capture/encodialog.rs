//! `EncodingDialog` — shown while the optimized (software) re-encode runs in the
//! manual "Optimized encoding" path. A title + a progress bar updated each frame
//! by the interface via [`CaptureMessage::SetEncodeProgress`]. Own small centered
//! screen-space instance (replaces the save dialog during encoding).

use iced_core::{Alignment, Background, Border, Element, Length, Shadow, Theme};
use iced_widget::{column, container, progress_bar, text};
use crate::capture::session::message::CaptureMessage;
use ui::engine::{IcedUi, Renderer};

use crate::surface::draw::capture::style;

pub struct EncodingDialog {
    /// Progress percent (0..=100).
    pub percent: u32,
}

impl EncodingDialog {
    pub fn new() -> Self {
        Self { percent: 0 }
    }
}

impl Default for EncodingDialog {
    fn default() -> Self {
        Self::new()
    }
}

impl IcedUi for EncodingDialog {
    type Message = CaptureMessage;

    fn update(&mut self, message: Self::Message) {
        if let CaptureMessage::SetEncodeProgress(p) = message {
            self.percent = p.min(100);
        }
    }

    fn view(&self) -> Element<'_, Self::Message, Theme, Renderer> {
        let title = text("Optimizing recording…").size(20).color(style::TEXT);
        let sub = text(format!("Encoding a smaller file — {}%", self.percent))
            .size(14)
            .color(style::TEXT_DIM);
        let bar = progress_bar(0.0..=100.0, self.percent as f32)
            .length(Length::Fixed(320.0))
            .girth(Length::Fixed(10.0));

        let panel = container(
            column![title, sub, bar]
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

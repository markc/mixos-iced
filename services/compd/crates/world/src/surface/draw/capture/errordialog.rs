//! `ErrorDialog` — a small centered modal shown when a capture can't proceed
//! (e.g. no working hardware video encoder). A message + a single OK button that
//! emits [`CaptureMessage::Discard`] (the interface tears the capture down).

use iced_core::{Alignment, Background, Border, Element, Length, Shadow, Theme};
use iced_widget::{button, column, container, text};
use crate::capture::session::message::CaptureMessage;
use ui::engine::{IcedUi, Renderer};

use crate::surface::draw::capture::style;

pub struct ErrorDialog {
    message: String,
}

impl ErrorDialog {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl IcedUi for ErrorDialog {
    type Message = CaptureMessage;

    fn update(&mut self, _message: Self::Message) {}

    fn view(&self) -> Element<'_, Self::Message, Theme, Renderer> {
        let title = text("Capture error").size(20).color(style::STOP_BG);
        let msg = text(self.message.clone()).size(14).color(style::TEXT_DIM);
        let ok = button(text("OK").size(15).color(style::TEXT))
            .padding([8, 18])
            .on_press(CaptureMessage::Discard)
            .style(style::button_with(style::ACCENT));

        let panel = container(
            column![title, msg, ok]
                .spacing(14)
                .align_x(Alignment::Center),
        )
        .padding(24)
        .style(|_t| container::Style {
            background: Some(Background::Color(style::PANEL_BG)),
            text_color: Some(style::TEXT),
            border: Border {
                color: style::STOP_BG,
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

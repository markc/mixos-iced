//! `StopHud` — the top-right Stop button shown while a capture is active. Its
//! own small screen-space instance (positioned by the interface) so it only
//! intercepts clicks where the button is.

use iced_core::{Element, Length, Theme};
use iced_widget::{button, container, text};
use crate::capture::session::message::CaptureMessage;
use ui::engine::{IcedUi, Renderer};

use crate::surface::draw::capture::style;

pub struct StopHud;

impl IcedUi for StopHud {
    type Message = CaptureMessage;

    fn update(&mut self, _message: Self::Message) {}

    fn view(&self) -> Element<'_, Self::Message, Theme, Renderer> {
        let btn = button(text("\u{23F9} Stop").size(15).color(style::TEXT))
            .padding([8, 16])
            .on_press(CaptureMessage::Stop)
            .style(style::button_with(style::STOP_BG));

        container(btn)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(10)
            .into()
    }
}

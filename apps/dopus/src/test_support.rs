// SPDX-License-Identifier: MIT OR Apache-2.0
//! Run the retained UI event fixtures through iced's current runtime API.
use application::iced::advanced::shell::{Bus, Waker};
use application::iced::{Event, mouse};
use application::runtime::UserInterface;

pub fn update_ui<M, T, R: application::iced::advanced::Renderer>(
    ui: &mut UserInterface<'_, M, T, R>,
    events: &[Event],
    cursor: mouse::Cursor,
    renderer: &mut R,
    messages: &mut Vec<M>,
) -> (
    application::runtime::user_interface::State,
    Vec<application::iced::event::Status>,
) {
    let mut bus = Bus::new();
    let result = ui.update(
        &application::iced::window::Headless,
        &Waker::new(|| {}),
        events,
        cursor,
        renderer,
        &mut bus,
    );
    messages.extend(bus.drain());
    result
}

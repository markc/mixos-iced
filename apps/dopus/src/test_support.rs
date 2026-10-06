// SPDX-License-Identifier: MIT OR Apache-2.0
//! Run the retained UI event fixtures through iced's current runtime API.
use iced::advanced::shell::{Bus, Waker};
use iced::{Event, mouse};
use iced_runtime::UserInterface;

pub fn update_ui<M, T, R: iced::advanced::Renderer>(
    ui: &mut UserInterface<'_, M, T, R>,
    events: &[Event],
    cursor: mouse::Cursor,
    renderer: &mut R,
    messages: &mut Vec<M>,
) -> (
    iced_runtime::user_interface::State,
    Vec<iced::event::Status>,
) {
    let mut bus = Bus::new();
    let result = ui.update(
        &iced::window::Headless,
        &Waker::new(|| {}),
        events,
        cursor,
        renderer,
        &mut bus,
    );
    messages.extend(bus.drain());
    result
}

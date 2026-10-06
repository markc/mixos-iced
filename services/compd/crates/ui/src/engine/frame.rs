// SPDX-License-Identifier: MIT OR Apache-2.0
//! A rebuilt iced view needs a frame event before its transient styles draw.

use iced_core::{Event, Renderer, mouse, shell, time::Instant, window};
use iced_runtime::user_interface::{State, UserInterface};

pub(super) fn prepare<Message, Theme, R: Renderer>(
    ui: &mut UserInterface<'_, Message, Theme, R>,
    cursor: mouse::Cursor,
    renderer: &mut R,
    waker: &shell::Waker,
    messages: &mut shell::Bus<Message>,
    now: Instant,
) -> State {
    ui.update(
        &window::Headless, waker,
        &[Event::Window(window::Event::RedrawRequested(now))],
        cursor, renderer, messages,
    ).0
}

/// Rebuilt widgets have no previous transient status to compare during tick.
/// Repaint the receiving surface for changed pointer/touch input, then idle.
pub(super) fn visual_input(event: &Event, cursor: mouse::Cursor) -> bool {
    match event {
        Event::Mouse(mouse::Event::CursorMoved { position }) => cursor.position() != Some(*position),
        Event::Mouse(mouse::Event::CursorLeft) => cursor.position().is_some(),
        Event::Mouse(mouse::Event::ButtonPressed(_) | mouse::Event::ButtonReleased(_))
        | Event::Touch(_) => true,
        _ => false,
    }
}

/// A NextFrame from tick is answered by this draw. Preserve a future deadline
/// promised on an earlier event as well as requests made while preparing draw.
pub(super) fn after_draw(previous: window::RedrawRequest, state: State, now: Instant) -> window::RedrawRequest {
    let future = match previous {
        window::RedrawRequest::At(at) if at > now => previous,
        _ => window::RedrawRequest::Wait,
    };
    let requested = match state {
        State::Updated { redraw_request, .. } => redraw_request,
        State::Outdated => window::RedrawRequest::NextFrame,
    };
    future.min(requested)
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_core::{Background, Element, Point, Rectangle, Size, Theme, Transformation, image, renderer};
    use iced_runtime::user_interface::Cache;
    use iced_widget::{Space, button};

    #[derive(Default)]
    struct Paint(Vec<Background>);
    impl Renderer for Paint {
        fn start_layer(&mut self, _: Rectangle) {}
        fn end_layer(&mut self) {}
        fn start_transformation(&mut self, _: Transformation) {}
        fn end_transformation(&mut self) {}
        fn fill_quad(&mut self, _: renderer::Quad, background: impl Into<Background>) { self.0.push(background.into()); }
        fn allocate_image(&mut self, _: &image::Handle, callback: impl FnOnce(Result<image::Allocation, image::Error>) + Send + 'static) {
            callback(Err(image::Error::Unsupported));
        }
        fn hint(&mut self, _: renderer::Scale) {}
        fn scale(&self) -> Option<renderer::Scale> { None }
        fn reset(&mut self, _: Rectangle) { self.0.clear(); }
        fn settings(&self) -> renderer::Settings { renderer::Settings::default() }
    }

    fn view(enabled: bool) -> Element<'static, u8, Theme, Paint> {
        button(Space::new().width(20).height(10)).width(60).height(30)
            .style(button::background).on_press_maybe(enabled.then_some(7)).into()
    }

    #[test]
    fn rebuilt_draws_show_hover_press_release_leave_and_disabled_styles() {
        let mut cache = Cache::default();
        let mut renderer = Paint::default();
        let waker = shell::Waker::new(|| {});
        let inside = mouse::Cursor::Available(Point::new(5.0, 5.0));
        for (enabled, cursor, event, expected) in [
            (true, mouse::Cursor::Unavailable, None, button::Status::Active),
            (true, inside, None, button::Status::Hovered),
            (true, inside, Some(Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))), button::Status::Pressed),
            (true, inside, Some(Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))), button::Status::Hovered),
            (true, mouse::Cursor::Unavailable, Some(Event::Mouse(mouse::Event::CursorLeft)), button::Status::Active),
            (false, inside, None, button::Status::Disabled),
        ] {
            // The production host also discards and rebuilds its view between
            // processing input and drawing, carrying only the widget cache.
            if let Some(event) = event {
                let mut ui = UserInterface::build(view(enabled), Size::new(100.0, 50.0), cache, &mut renderer);
                let mut messages = shell::Bus::new();
                ui.update(&window::Headless, &waker, &[event], cursor, &mut renderer, &mut messages);
                let emitted: Vec<_> = messages.drain().collect();
                assert_eq!(emitted, if expected == button::Status::Hovered { vec![7] } else { vec![] });
                cache = ui.into_cache();
            }
            let mut ui = UserInterface::build(view(enabled), Size::new(100.0, 50.0), cache, &mut renderer);
            let mut messages = shell::Bus::new();
            prepare(&mut ui, cursor, &mut renderer, &waker, &mut messages, Instant::now());
            assert_eq!(messages.drain().count(), 0, "a frame must not activate a button");
            renderer.0.clear();
            ui.draw(&mut renderer, &Theme::Dark, &renderer::Style::default(), cursor);
            assert_eq!(renderer.0, vec![button::background(&Theme::Dark, expected).background.unwrap()], "{expected:?}");
            cache = ui.into_cache();
        }
    }

    #[test]
    fn pointer_damage_is_event_driven_and_repeated_positions_do_not_repaint() {
        let point = Point::new(5.0, 5.0);
        let moved = Event::Mouse(mouse::Event::CursorMoved { position: point });
        assert!(visual_input(&moved, mouse::Cursor::Unavailable));
        assert!(!visual_input(&moved, mouse::Cursor::Available(point)));
        assert!(visual_input(&Event::Mouse(mouse::Event::CursorLeft), mouse::Cursor::Available(point)));
        assert!(!visual_input(&Event::Mouse(mouse::Event::CursorLeft), mouse::Cursor::Unavailable));
        assert!(!visual_input(&Event::Window(window::Event::RedrawRequested(Instant::now())), mouse::Cursor::Unavailable));
    }

    #[test]
    fn a_completed_frame_stops_next_frame_work_but_retains_future_wakes() {
        let now = Instant::now();
        let mut renderer = Paint::default();
        let waker = shell::Waker::new(|| {});
        let mut idle = || {
            let view: Element<'_, u8, Theme, Paint> = Space::new().into();
            let mut ui = UserInterface::build(view, Size::new(100.0, 50.0), Cache::default(), &mut renderer);
            prepare(&mut ui, mouse::Cursor::Unavailable, &mut renderer, &waker, &mut shell::Bus::new(), now)
        };
        assert_eq!(after_draw(window::RedrawRequest::NextFrame, idle(), now), window::RedrawRequest::Wait);
        let later = now + iced_core::time::Duration::from_secs(1);
        assert_eq!(after_draw(window::RedrawRequest::At(later), idle(), now), window::RedrawRequest::At(later));
        assert_eq!(after_draw(window::RedrawRequest::At(now), idle(), now), window::RedrawRequest::Wait);
        assert_eq!(after_draw(window::RedrawRequest::Wait, State::Outdated, now), window::RedrawRequest::NextFrame);
    }
}

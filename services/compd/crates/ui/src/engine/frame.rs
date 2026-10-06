// SPDX-License-Identifier: MIT OR Apache-2.0
//! A rebuilt iced view needs a frame event before its transient styles draw.

use iced_core::{Event, Renderer, mouse, shell, time::Instant, window};
use iced_runtime::user_interface::{State, UserInterface};

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Pass {
    Draw,
    Retry,
    Defer,
}

/// Match iced's three-pass budget: settle layout/messages at one timestamp,
/// then yield to the host instead of letting a publishing widget block it.
pub(super) fn next_pass(attempt: usize, state: &State, messages: bool) -> Pass {
    if !messages && !state.has_layout_changed() {
        Pass::Draw
    } else if attempt < 2 {
        Pass::Retry
    } else {
        Pass::Defer
    }
}

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

/// Preserve outstanding redraw requests across ordinary input. A frame answers
/// NextFrame and elapsed deadlines, retaining future and newly requested frames.
pub(super) fn after_update(
    previous: window::RedrawRequest,
    state: State,
    now: Instant,
    drew: bool,
) -> window::RedrawRequest {
    let future = match previous {
        window::RedrawRequest::NextFrame if !drew => previous,
        window::RedrawRequest::At(at) if !drew || at > now => previous,
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
        assert_eq!(after_update(window::RedrawRequest::NextFrame, idle(), now, true), window::RedrawRequest::Wait);
        let later = now + iced_core::time::Duration::from_secs(1);
        assert_eq!(after_update(window::RedrawRequest::At(later), idle(), now, true), window::RedrawRequest::At(later));
        assert_eq!(after_update(window::RedrawRequest::At(now), idle(), now, true), window::RedrawRequest::Wait);
        assert_eq!(after_update(window::RedrawRequest::Wait, State::Outdated, now, true), window::RedrawRequest::NextFrame);
    }

    #[test]
    fn ignored_input_keeps_an_unfulfilled_deadline_until_its_frame() {
        let now = Instant::now();
        let deadline = now + iced_core::time::Duration::from_secs(1);
        let mut renderer = Paint::default();
        let waker = shell::Waker::new(|| {});
        let view: Element<'_, u8, Theme, Paint> = Space::new().into();
        let mut ui = UserInterface::build(view, Size::new(100.0, 50.0), Cache::default(), &mut renderer);
        let mut messages = shell::Bus::new();
        let event = Event::Keyboard(iced_core::keyboard::Event::ModifiersChanged(iced_core::keyboard::Modifiers::SHIFT));
        let (state, _) = ui.update(&window::Headless, &waker, &[event], mouse::Cursor::Unavailable, &mut renderer, &mut messages);
        assert_eq!(messages.drain().count(), 0);
        let next = after_update(window::RedrawRequest::At(deadline), state, now, false);
        assert_eq!(next, window::RedrawRequest::At(deadline));
        let state = prepare(&mut ui, mouse::Cursor::Unavailable, &mut renderer, &waker, &mut messages, deadline);
        assert_eq!(after_update(next, state, deadline, true), window::RedrawRequest::Wait);
    }

    #[test]
    fn redraw_callback_retries_at_the_same_instant_then_settles_to_idle() {
        let now = Instant::now();
        let mut last = None;
        let mut cache = Cache::default();
        let mut renderer = Paint::default();
        let waker = shell::Waker::new(|| {});
        let mut reductions = 0;
        for attempt in 0..3 {
            let view: Element<'_, Instant, Theme, Paint> = toolkit::keys::keys(Space::new(), |_| None)
                .on_redraw(last, |at| at).into();
            let mut ui = UserInterface::build(view, Size::new(100.0, 50.0), cache, &mut renderer);
            let mut messages = shell::Bus::new();
            let state = prepare(&mut ui, mouse::Cursor::Unavailable, &mut renderer, &waker, &mut messages, now);
            let emitted: Vec<_> = messages.drain().collect();
            cache = ui.into_cache();
            if emitted.is_empty() {
                assert_eq!(attempt, 1, "exactly one reducer/rebuild retry");
                assert_eq!(reductions, 1);
                assert_eq!(after_update(window::RedrawRequest::Wait, state, now, true), window::RedrawRequest::Wait);
                return;
            }
            for at in emitted { last = Some(at); reductions += 1; }
        }
        panic!("redraw callback did not settle");
    }

    #[test]
    fn a_persistent_redraw_publisher_yields_after_three_passes() {
        let now = Instant::now();
        let mut renderer = Paint::default();
        let waker = shell::Waker::new(|| {});
        let mut cache = Cache::default();
        let mut delivered = Vec::new();
        for attempt in 0..4 {
            // This valid callback deliberately has no reducer-owned last time.
            let view: Element<'_, Instant, Theme, Paint> = toolkit::keys::keys(Space::new(), |_| None)
                .on_redraw(None, |at| at).into();
            let mut ui = UserInterface::build(view, Size::new(100.0, 50.0), cache, &mut renderer);
            let mut messages = shell::Bus::new();
            let state = prepare(&mut ui, mouse::Cursor::Unavailable, &mut renderer, &waker, &mut messages, now);
            let emitted: Vec<_> = messages.drain().collect();
            let pass = next_pass(attempt, &state, !emitted.is_empty());
            cache = ui.into_cache();
            delivered.extend(emitted);
            if pass == Pass::Defer {
                assert_eq!(attempt, 2);
                assert_eq!(delivered, vec![now; 3], "retain all messages, including the deferred pass");
                return;
            }
            assert_eq!(pass, Pass::Retry);
        }
        panic!("persistent redraw callback exceeded the frame budget");
    }

    #[test]
    fn relayout_retries_prepare_recreated_responsive_buttons() {
        struct Resize(bool);
        impl iced_widget::transition::Program for Resize {
            type Value = bool;
            fn go(&mut self, large: bool, _: Instant) { self.0 = large; }
            fn is_animating(&self, _: Instant) -> bool { true }
        }
        let view = || -> Element<'_, u8, Theme, Paint> {
            iced_widget::transition::Transition::new(|| Resize(false), true, |size: &Resize, _| {
                iced_widget::Responsive::new(|_| self::view(true))
                    .height(if size.0 { 40 } else { 20 })
            }).into()
        };
        let mut renderer = Paint::default();
        let waker = shell::Waker::new(|| {});
        let cursor = mouse::Cursor::Available(Point::new(5.0, 5.0));
        let mut ui = UserInterface::build(view(), Size::new(100.0, 50.0), Cache::default(), &mut renderer);
        let now = Instant::now() + iced_core::time::Duration::from_secs(1);
        let mut messages = shell::Bus::new();
        let state = prepare(&mut ui, cursor, &mut renderer, &waker, &mut messages, now);
        assert_eq!(messages.drain().count(), 0);
        assert!(state.has_layout_changed());
        assert_eq!(next_pass(0, &state, false), Pass::Retry);
        renderer.0.clear();
        ui.draw(&mut renderer, &Theme::Dark, &renderer::Style::default(), cursor);
        assert_eq!(renderer.0, vec![button::background(&Theme::Dark, button::Status::Disabled).background.unwrap()], "relayout created a button after its frame event");
        let cache = ui.into_cache();
        let mut ui = UserInterface::build(view(), Size::new(100.0, 50.0), cache, &mut renderer);
        let state = prepare(&mut ui, cursor, &mut renderer, &waker, &mut messages, now);
        assert_eq!(messages.drain().count(), 0);
        assert_eq!(next_pass(1, &state, false), Pass::Draw);
        renderer.0.clear();
        ui.draw(&mut renderer, &Theme::Dark, &renderer::Style::default(), cursor);
        assert_eq!(renderer.0, vec![button::background(&Theme::Dark, button::Status::Hovered).background.unwrap()]);
    }
}

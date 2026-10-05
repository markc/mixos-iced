// SPDX-License-Identifier: MIT OR Apache-2.0
//! Regression coverage for composite controls through the real widget tree.
#![cfg(all(feature = "gallery-tiny-skia", not(feature = "wgpu")))]

use iced_test::Simulator;
use std::sync::{Arc, Mutex};
use toolkit::command_palette::{self, Command};
use toolkit::core::{Element, Event, Point, Settings, Size, keyboard, mouse, widget, window};
use toolkit::dnd;
use toolkit::requester::{self, Requester};
use toolkit::{Theme, Tokens};

fn simulation<'a, M>(
    view: Element<'a, M, Theme, iced_widget::Renderer>,
) -> Simulator<'a, M, Theme> {
    Simulator::with_size(Settings::default(), Size::new(800.0, 700.0), view)
}

#[test]
fn palette_pointer_enter_escape_and_later_results_are_reachable() {
    let commands: Vec<_> = (0..40)
        .map(|i| Command::new(format!("Command {i}"), i))
        .collect();
    let view = || {
        command_palette::command_palette_with_placeholder(
            "",
            Some(1),
            &commands,
            &Tokens::dark(),
            "Search",
        )
    };
    let mut ui = simulation(view());
    ui.click("Command 1").unwrap();
    assert!(matches!(
        ui.into_messages().next(),
        Some(command_palette::Event::Activated(1))
    ));
    let mut ui = simulation(view());
    ui.click(widget::Id::new(command_palette::INPUT_ID))
        .unwrap();
    ui.tap_key(keyboard::key::Named::Enter);
    assert!(matches!(
        ui.into_messages().next(),
        Some(command_palette::Event::Activated(1))
    ));
    let mut ui = simulation(view());
    ui.tap_key(keyboard::key::Named::Escape);
    assert!(matches!(
        ui.into_messages().next(),
        Some(command_palette::Event::Dismissed)
    ));
    let mut ui = simulation(view());
    ui.point_at(Point::new(790.0, 690.0));
    ui.simulate(iced_test::simulator::click());
    assert!(matches!(
        ui.into_messages().next(),
        Some(command_palette::Event::Dismissed)
    ));
    let mut ui = simulation(view());
    assert!(ui.find("Command 39").unwrap().visible_bounds().is_none());
    ui.point_at(Point::new(400.0, 300.0));
    ui.simulate([Event::Mouse(mouse::Event::WheelScrolled {
        delta: mouse::ScrollDelta::Lines { x: 0.0, y: -80.0 },
    })]);
    ui.click("Command 39").unwrap();
    assert!(matches!(
        ui.into_messages().next(),
        Some(command_palette::Event::Activated(39))
    ));
}

#[test]
fn requester_completion_navigation_and_save_outcomes_use_actual_keys() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("documents")).unwrap();
    std::fs::write(temp.path().join("note.txt"), b"note").unwrap();
    let mut requester = Requester::new(
        requester::Mode::Open,
        temp.path().into(),
        vec![],
        requester::std_fs(),
    );
    requester.update(requester::Event::Input("doc".into()));
    let strings = requester::Strings::english();
    let mut ui = simulation(requester.view::<requester::ViewMessage>(Tokens::dark(), &strings));
    ui.click(widget::Id::new(requester::PATH_INPUT)).unwrap();
    ui.tap_key(keyboard::key::Named::Tab);
    for message in ui.into_messages() {
        requester.update(message.0);
    }
    assert_eq!(requester.input(), "documents/");
    let mut ui = simulation(requester.view::<requester::ViewMessage>(Tokens::dark(), &strings));
    ui.click(widget::Id::new(requester::PATH_INPUT)).unwrap();
    ui.tap_key(keyboard::key::Named::Enter);
    for message in ui.into_messages() {
        requester.update(message.0);
    }
    assert_eq!(requester.dir(), temp.path().join("documents"));
    requester.update(requester::Event::Parent);
    let mut ui = simulation(requester.view::<requester::ViewMessage>(Tokens::light(), &strings));
    ui.tap_key(keyboard::key::Named::ArrowDown);
    ui.tap_key(keyboard::key::Named::ArrowDown);
    for message in ui.into_messages() {
        requester.update(message.0);
    }
    assert_eq!(requester.input(), "note.txt");
    assert!(matches!(
        requester.update(requester::Event::Submit),
        Some(requester::Outcome::Open(_))
    ));
    let mut save = Requester::new(
        requester::Mode::Save,
        temp.path().into(),
        vec![],
        requester::std_fs(),
    )
    .with_name("note.txt");
    let mut ui = simulation(save.view::<requester::ViewMessage>(Tokens::light(), &strings));
    ui.click(widget::Id::new(requester::PATH_INPUT)).unwrap();
    ui.tap_key(keyboard::key::Named::Enter);
    let outcomes: Vec<_> = ui
        .into_messages()
        .filter_map(|m| save.update(m.0))
        .collect();
    assert!(matches!(
        outcomes.as_slice(),
        [requester::Outcome::Save { exists: true, .. }]
    ));
}

#[test]
fn cancelled_drags_rearm_and_keep_stateful_children_working() {
    let shared: dnd::Shared<u8> = Arc::new(Mutex::new(dnd::State::default()));
    let source = dnd::DragArea::new(iced_widget::button("Source").on_press(0), 1u8)
        .start_directly(shared.clone());
    let view = dnd::Layer::new(source, shared.clone(), Tokens::dark(), |_| 2).into();
    let mut ui = simulation(view);
    ui.click("Source").unwrap();
    let start = Point::new(15.0, 15.0);
    for cancel in [
        Event::Mouse(mouse::Event::CursorLeft),
        Event::Window(window::Event::Unfocused),
        iced_test::simulator::press_key(keyboard::key::Named::Escape, None),
    ] {
        ui.point_at(start);
        ui.simulate([Event::Mouse(mouse::Event::ButtonPressed(
            mouse::Button::Left,
        ))]);
        let end = Point::new(25.0, 15.0);
        ui.point_at(end);
        ui.simulate([Event::Mouse(mouse::Event::CursorMoved { position: end })]);
        assert!(
            dnd::lock(&shared).active.is_some(),
            "rearmed after previous cancellation"
        );
        ui.simulate([cancel]);
        assert!(dnd::lock(&shared).active.is_none());
        // No release is delivered after leaving the window.
    }
    ui.click("Source").unwrap();
    assert_eq!(ui.into_messages().filter(|m| *m == 0).count(), 2);
}

#[test]
fn selection_arithmetic_accepts_stale_and_extreme_indices() {
    assert_eq!(
        command_palette::move_selection(Some(usize::MAX), i32::MAX, 3),
        Some(0)
    );
    assert_eq!(
        command_palette::move_selection(Some(0), i32::MIN, usize::MAX),
        Some(usize::MAX - 2_147_483_648)
    );
}

#[test]
fn column_resize_reports_absolute_preview_commit_cancel_and_sort_separately() {
    use toolkit::virtual_list::{Columns, Resize};
    #[derive(Debug, Clone, PartialEq)]
    enum Message {
        Sort(usize),
        Resize(Resize),
    }
    let columns = Columns::new()
        .column("Name", toolkit::core::Length::Fill)
        .column("Size", 80)
        .spacing(0.0);
    let header = columns.resizable_header(None, Message::Sort, Message::Resize, 90.0, 3.0);
    let view = iced_widget::container(header)
        .height(32)
        .width(toolkit::core::Length::Fill)
        .into();
    let mut ui = simulation(view);
    ui.click("Name").unwrap();
    ui.point_at(Point::new(719.0, 16.0));
    ui.simulate([Event::Mouse(mouse::Event::ButtonPressed(
        mouse::Button::Left,
    ))]);
    ui.point_at(Point::new(749.0, 16.0));
    ui.simulate([Event::Mouse(mouse::Event::CursorMoved {
        position: Point::new(749.0, 16.0),
    })]);
    ui.point_at(Point::new(759.0, 16.0));
    ui.simulate([
        Event::Mouse(mouse::Event::CursorMoved {
            position: Point::new(759.0, 16.0),
        }),
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
    ]);
    ui.point_at(Point::new(719.0, 16.0));
    ui.simulate([
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
        Event::Window(window::Event::Unfocused),
        Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
    ]);
    assert_eq!(
        ui.into_messages().collect::<Vec<_>>(),
        vec![
            Message::Sort(0),
            Message::Resize(Resize::Preview {
                column: 0,
                width: 750.0
            }),
            Message::Resize(Resize::Preview {
                column: 0,
                width: 760.0
            }),
            Message::Resize(Resize::Commit { column: 0 }),
            Message::Resize(Resize::Cancel { column: 0 }),
        ]
    );
}

#[test]
fn header_controls_do_not_start_a_window_drag_and_input_clear_is_disabled_when_empty() {
    use toolkit::patterns::{HeaderBar, InputField};
    #[derive(Debug, Clone, PartialEq)]
    enum Message {
        Control,
        Drag,
        Clear,
        Input(String),
        Submit,
    }
    let header = HeaderBar::new(iced_widget::text("Title"))
        .end(iced_widget::button("Control").on_press(Message::Control))
        .on_drag(Message::Drag)
        .view(Tokens::dark());
    let mut ui = simulation(header);
    ui.click("Control").unwrap();
    ui.click("Title").unwrap();
    assert_eq!(
        ui.into_messages().collect::<Vec<_>>(),
        vec![Message::Control, Message::Drag]
    );
    for value in ["", "query"] {
        let view = InputField::new("Search", value)
            .on_input(Message::Input)
            .on_submit(Message::Submit)
            .clear("Clear", Message::Clear)
            .view(Tokens::dark());
        let mut ui = simulation(view);
        ui.click("Clear").unwrap();
        assert_eq!(
            ui.into_messages().collect::<Vec<_>>(),
            if value.is_empty() {
                vec![]
            } else {
                vec![Message::Clear]
            }
        );
    }
}

#[test]
fn tab_strip_scrolls_to_later_tabs_and_middle_click_closes_the_target() {
    use toolkit::tab_bar::{TabBar, TabLabel};
    #[derive(Debug, Clone, PartialEq)]
    enum Message {
        Select(usize),
        Close(usize),
    }
    let mut bar = TabBar::new(Message::Select)
        .on_close(Message::Close)
        .tab_width(120.into());
    for index in 0..40 {
        bar = bar.push(index, TabLabel::Text(format!("Document {index}")));
    }
    let mut ui = simulation(bar.scrollable().into());
    assert!(ui.find("Document 39").unwrap().visible_bounds().is_none());
    ui.point_at(Point::new(400.0, 16.0));
    ui.simulate([Event::Mouse(mouse::Event::WheelScrolled {
        delta: mouse::ScrollDelta::Pixels {
            x: -10000.0,
            y: 0.0,
        },
    })]);
    let bounds = ui.find("Document 39").unwrap().visible_bounds().unwrap();
    ui.point_at(bounds.center());
    ui.simulate([Event::Mouse(mouse::Event::ButtonPressed(
        mouse::Button::Middle,
    ))]);
    assert_eq!(
        ui.into_messages().collect::<Vec<_>>(),
        vec![Message::Close(39)]
    );
    let bar = TabBar::new(Message::Select).push(0, TabLabel::Text("Only".into()));
    let mut ui = simulation(bar.into());
    ui.point_at(Point::new(20.0, 15.0));
    ui.simulate([Event::Mouse(mouse::Event::ButtonPressed(
        mouse::Button::Middle,
    ))]);
    assert!(ui.into_messages().next().is_none());
}

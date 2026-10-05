// SPDX-License-Identifier: MIT OR Apache-2.0
//! The ordinary shell example, rendered and driven through the real
//! widget tree. No window, GPU, compositor or host font is required.
#![cfg(all(feature = "gallery-tiny-skia", not(feature = "wgpu")))]

#[allow(dead_code)]
#[path = "../examples/shell/app.rs"]
mod app;

use app::{App, Message, Mode};
use iced_test::Simulator;
use iced_test::core::{Event, Point, Settings, Size, keyboard, mouse};
use toolkit::shell::{self, Side, Toolbar};
use toolkit::{Theme, Tokens, fonts};

const VIEWPORT: Size = Size::new(1000.0, 600.0);

#[test]
fn toggling_shell_chrome_preserves_editor_focus_selection_and_undo() {
    use toolkit::core::shell::{Bus, Waker};
    use toolkit::core::{
        Element, Layout, layout,
        renderer::Headless,
        widget::{Id, Tree, operation},
    };
    fn view(value: &str, visible: bool) -> Element<'_, String, Theme, iced_widget::Renderer> {
        let mut shell = shell::Shell::new(
            toolkit::TextField::new("", value)
                .id(Id::new("editor"))
                .on_input(|s| s),
        );
        if visible {
            shell = shell
                .sidebar(Side::Left, 180.0, iced_widget::text("Left"))
                .sidebar(Side::Right, 180.0, iced_widget::text("Right"))
                .menu(Some(vec![]))
                .toolbar(Toolbar::new().push(shell::tool("Tool", String::new())))
                .status(shell::StatusBar::new().left(vec![shell::field("Ready")]));
        }
        shell.into()
    }
    let renderer = iced_futures::futures::executor::block_on(
        <iced_widget::Renderer as Headless>::new(Default::default(), None),
    )
    .unwrap();
    let mut element = view("draft", false);
    let mut tree = Tree::new(element.as_widget());
    let build = |element: &mut Element<'_, String, Theme, iced_widget::Renderer>,
                 tree: &mut Tree,
                 renderer: &iced_widget::Renderer| {
        tree.diff(element.as_widget_mut());
        element
            .as_widget_mut()
            .layout(tree, renderer, &layout::Limits::new(Size::ZERO, VIEWPORT))
    };
    let node = build(&mut element, &mut tree, &renderer);
    element.as_widget_mut().operate(
        &mut tree,
        Layout::new(&node),
        &renderer,
        &mut operation::focusable::focus::<()>(Id::new("editor")),
    );
    let send = |element: &mut Element<'_, String, Theme, iced_widget::Renderer>,
                tree: &mut Tree,
                renderer: &iced_widget::Renderer,
                node: &layout::Node,
                events: Vec<Event>| {
        let mut bus = Bus::new();
        let mut shell =
            toolkit::core::Shell::new(&toolkit::core::window::Headless, Waker::noop(), &mut bus);
        for event in events {
            element.as_widget_mut().update(
                tree,
                &event,
                Layout::new(node),
                mouse::Cursor::Unavailable,
                renderer,
                &mut shell,
                &toolkit::core::Rectangle::with_size(VIEWPORT),
            );
        }
        bus.drain().collect::<Vec<_>>()
    };
    let key = |letter: &str, modifiers| {
        Event::Keyboard(keyboard::Event::KeyPressed {
            key: keyboard::Key::Character(letter.into()),
            modified_key: keyboard::Key::Character(letter.into()),
            physical_key: keyboard::key::Physical::Unidentified(
                keyboard::key::NativeCode::Unidentified,
            ),
            location: keyboard::Location::Standard,
            modifiers,
            text: (modifiers == keyboard::Modifiers::empty()).then(|| letter.into()),
            repeat: false,
        })
    };
    send(
        &mut element,
        &mut tree,
        &renderer,
        &node,
        vec![key("a", keyboard::Modifiers::CTRL)],
    );
    for visible in [true, false, true] {
        element = view("draft", visible);
        let node = build(&mut element, &mut tree, &renderer);
        let mut focused = operation::focusable::is_focused(Id::new("editor"));
        element.as_widget_mut().operate(
            &mut tree,
            Layout::new(&node),
            &renderer,
            &mut operation::black_box(&mut focused),
        );
        assert!(matches!(
            operation::Operation::finish(&focused),
            operation::Outcome::Some(true)
        ));
    }
    let node = build(&mut element, &mut tree, &renderer);
    assert_eq!(
        send(
            &mut element,
            &mut tree,
            &renderer,
            &node,
            vec![key("x", keyboard::Modifiers::empty())]
        ),
        ["x"]
    );
    element = view("x", false);
    let node = build(&mut element, &mut tree, &renderer);
    assert_eq!(
        send(
            &mut element,
            &mut tree,
            &renderer,
            &node,
            vec![key("z", keyboard::Modifiers::CTRL)]
        ),
        ["draft"]
    );
}

fn settings() -> Settings {
    Settings {
        default_font: fonts::default_ui_font(),
        ..Settings::default()
    }
}

fn click(app: &mut App, label: &str) -> Vec<Message> {
    let mut ui = Simulator::with_size(settings(), VIEWPORT, app.view());
    ui.click(label).expect("visible control");
    let messages: Vec<_> = ui.into_messages().collect();
    for message in &messages {
        app.update(*message);
    }
    messages
}

#[test]
fn shell_example_renders_every_token_set() {
    let directory = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("toolkit-snapshots");
    std::fs::create_dir_all(&directory).unwrap();
    let mut frames = Vec::new();
    for mode in Mode::ALL {
        let mut app = App::new();
        app.mode = mode;
        let mut ui = Simulator::with_size(settings(), VIEWPORT, app.view());
        for label in [
            "New",
            "Save",
            "Theme",
            "Places",
            "Inspector",
            "Ready",
            "UTF-8",
        ] {
            let bounds = ui
                .find(label)
                .expect("chrome visible")
                .visible_bounds()
                .unwrap();
            assert!(bounds.x >= 0.0 && bounds.y >= 0.0, "{mode:?}: {label}");
            assert!(
                bounds.x + bounds.width <= VIEWPORT.width + 1.0,
                "{mode:?}: {label}"
            );
            assert!(
                bounds.y + bounds.height <= VIEWPORT.height + 1.0,
                "{mode:?}: {label}"
            );
        }
        let path = directory.join(format!("shell-{}.png", mode.name()));
        let written = directory.join(format!("shell-{}-tiny-skia.png", mode.name()));
        let _ = std::fs::remove_file(&written);
        assert!(
            ui.snapshot(&app.theme())
                .unwrap()
                .matches_image(&path)
                .unwrap()
        );
        eprintln!("wrote {}", written.display());
        let decoder = png::Decoder::new(std::io::BufReader::new(
            std::fs::File::open(written).unwrap(),
        ));
        let mut reader = decoder.read_info().unwrap();
        let mut bytes = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut bytes).unwrap();
        assert_eq!((info.width, info.height), (2000, 1200));
        assert_eq!(info.color_type, png::ColorType::Rgba);
        bytes.truncate(info.buffer_size());
        let palette = mode.tokens().palette;
        for colour in [palette.surface, palette.muted_surface, palette.selection] {
            let wanted = colour.into_rgba8();
            assert!(
                bytes.chunks_exact(4).any(|pixel| {
                    pixel
                        .iter()
                        .zip(wanted)
                        .all(|(actual, expected)| actual.abs_diff(expected) <= 3)
                }),
                "{mode:?}: shell colour missing"
            );
        }
        frames.push(bytes);
    }
    assert_ne!(frames[0], frames[1], "dark and light differ");
    assert_ne!(frames[1], frames[2], "custom metrics alter the frame");
}

#[test]
fn toolbar_disabled_tools_and_navigation_publish_application_messages() {
    let mut app = App::new();
    assert!(click(&mut app, "Save").is_empty(), "Save starts disabled");
    assert_eq!(click(&mut app, "New"), vec![Message::New]);
    assert!(app.dirty);
    assert_eq!(click(&mut app, "Save"), vec![Message::Save]);
    assert!(!app.dirty);
    assert_eq!(click(&mut app, "Documents"), vec![Message::Select(1)]);
    assert_eq!(app.selected, 1);
    assert_eq!(click(&mut app, "Places"), vec![Message::Places]);
    assert!(!app.show_places);
    assert_eq!(click(&mut app, "Inspector"), vec![Message::Inspector]);
    assert!(!app.show_inspector);
    assert_eq!(click(&mut app, "Theme"), vec![Message::Theme]);
    assert_eq!(app.mode, Mode::Light);
    let mut ui = Simulator::with_size(settings(), VIEWPORT, app.view());
    assert!(ui.find("Documents").is_err(), "hidden sidebar is absent");
    assert!(ui.find("Details").is_err(), "hidden inspector is absent");
}

#[test]
fn the_existing_menu_remains_keyboard_operable_inside_the_shell() {
    let app = App::new();
    let mut ui = Simulator::with_size(settings(), VIEWPORT, app.view());
    ui.tap_key(keyboard::key::Named::F10);
    ui.tap_key(keyboard::key::Named::ArrowDown);
    ui.tap_key(keyboard::key::Named::Enter);
    assert_eq!(ui.into_messages().collect::<Vec<_>>(), vec![Message::New]);
}

#[test]
fn each_sidebar_grip_reports_its_own_side() {
    for side in [Side::Left, Side::Right] {
        let app = App::new();
        let mut ui = Simulator::with_size(settings(), VIEWPORT, app.view());
        let start_x = match side {
            Side::Left => app.left_width + 2.0,
            Side::Right => VIEWPORT.width - app.right_width - 2.0,
        };
        let start = Point::new(start_x, VIEWPORT.height / 2.0);
        ui.point_at(start);
        ui.simulate([Event::Mouse(mouse::Event::ButtonPressed(
            mouse::Button::Left,
        ))]);
        let end = Point::new(start.x + 40.0, start.y);
        ui.point_at(end);
        ui.simulate([
            Event::Mouse(mouse::Event::CursorMoved { position: end }),
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
        ]);
        let messages: Vec<_> = ui.into_messages().collect();
        assert_eq!(messages.len(), 1, "{side:?}: {messages:?}");
        let Message::Resize(actual_side, width) = messages[0] else {
            panic!("grip did not resize: {messages:?}")
        };
        assert_eq!(actual_side, side);
        let expected = match side {
            Side::Left => 220.0,
            Side::Right => 140.0,
        };
        assert!((width - expected).abs() < 1.0, "{side:?}: {width}");
    }
}

#[test]
fn toolbar_keeps_the_middle_centred_with_unequal_edges() {
    let toolbar = Toolbar::new()
        .leading(shell::tool("A long leading label", ()))
        .push(shell::tool("Middle", ()))
        .trailing(shell::tool("R", ()));
    let mut ui = Simulator::with_size(settings(), VIEWPORT, toolbar.view(Tokens::dark()));
    let middle = ui.find("Middle").unwrap().bounds();
    assert!(
        (middle.center_x() - VIEWPORT.width / 2.0).abs() < 1.0,
        "{middle:?}"
    );
}

#[test]
fn rebuilding_with_custom_metrics_changes_toolbar_layout() {
    let height = |tokens| {
        let mut ui = Simulator::with_size(
            settings(),
            VIEWPORT,
            Toolbar::new().push(shell::tool("Size", ())).view(tokens),
        );
        ui.find("Size").unwrap().bounds().height
    };
    assert!(height(Mode::Custom.tokens()) > height(Mode::Light.tokens()));
}

#[test]
fn a_tool_can_be_enabled_after_being_disabled() {
    let toolbar = Toolbar::new().push(shell::tool("Enable", 7).enabled(false).enabled(true));
    let mut ui = Simulator::with_size(settings(), VIEWPORT, toolbar.view(Tokens::dark()));
    ui.click("Enable").unwrap();
    assert_eq!(ui.into_messages().collect::<Vec<_>>(), vec![7]);
}

#[test]
fn existing_shell_restyles_without_rebuilding_its_elements() {
    let app = App::new();
    let mut ui = Simulator::with_size(settings(), VIEWPORT, app.view());
    let toolbar_y = ui.find("New").unwrap().bounds().center_y();
    let selected_y = ui.find("Overview").unwrap().bounds().center_y();
    // A theme swap at draw time must resolve the new chrome colours.
    let directory = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("toolkit-snapshots");
    std::fs::create_dir_all(&directory).unwrap();
    let mut frames = Vec::new();
    for (name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
        let path = directory.join(format!("shell-swap-{name}.png"));
        let written = directory.join(format!("shell-swap-{name}-tiny-skia.png"));
        let _ = std::fs::remove_file(&written);
        assert!(ui.snapshot(&theme).unwrap().matches_image(&path).unwrap());
        let decoder = png::Decoder::new(std::io::BufReader::new(
            std::fs::File::open(&written).unwrap(),
        ));
        let mut reader = decoder.read_info().unwrap();
        let mut bytes = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut bytes).unwrap();
        for (y, colour) in [
            (toolbar_y, theme.tokens().palette.muted_surface),
            (selected_y, theme.tokens().palette.selection),
        ] {
            let at = ((y * 2.0) as usize * info.width as usize + 2) * 4;
            assert!(
                bytes[at..at + 4]
                    .iter()
                    .zip(colour.into_rgba8())
                    .all(|(actual, expected)| actual.abs_diff(expected) <= 3),
                "{name}: chrome did not resolve its colour from the drawing theme"
            );
        }
        frames.push(std::fs::read(written).unwrap());
    }
    assert_ne!(frames[0], frames[1]);
}

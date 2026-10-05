// SPDX-License-Identifier: MIT OR Apache-2.0
//! Date/time widgets rendered and driven without a host clock or window.
#![cfg(all(feature = "gallery-tiny-skia", not(feature = "wgpu")))]

#[allow(dead_code)]
#[path = "../examples/gallery/app.rs"]
mod app;
use app::{Gallery, Message, Mode};
use iced_test::Simulator;
use toolkit::core::{Settings, Size, keyboard::key::Named};
use toolkit::date_picker::Date;
use toolkit::time_picker::Time;

#[test]
fn pickers_render_dark_light_and_custom_tokens() {
    let directory = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("toolkit-snapshots");
    std::fs::create_dir_all(&directory).unwrap();
    for mode in Mode::ALL {
        let mut gallery = Gallery::new();
        gallery.update(Message::Mode(mode));
        let mut ui = Simulator::with_size(
            Settings::default(),
            Size::new(1000.0, 650.0),
            gallery.pickers_page(),
        );
        for label in [
            "February 2024",
            "Mon",
            "Sun",
            "Hour",
            "Minute",
            "Second",
            "PM",
        ] {
            let bounds = ui.find(label).unwrap().visible_bounds().unwrap();
            assert!(bounds.width > 0.0 && bounds.x + bounds.width <= 1000.0);
        }
        let path = directory.join(format!("pickers-{}.png", mode.name()));
        let written = directory.join(format!("pickers-{}-tiny-skia.png", mode.name()));
        let _ = std::fs::remove_file(written);
        assert!(
            ui.snapshot(&gallery.theme())
                .unwrap()
                .matches_image(path)
                .unwrap()
        );
    }
}

#[test]
fn calendar_keys_and_clock_keys_do_not_steal_each_others_focus() {
    let mut gallery = Gallery::new();
    let mut ui = Simulator::with_size(
        Settings::default(),
        Size::new(1000.0, 650.0),
        gallery.pickers_page(),
    );
    ui.click("29").unwrap();
    ui.tap_key(Named::ArrowRight);
    let messages: Vec<_> = ui.into_messages().collect();
    for message in messages {
        gallery.update(message);
    }
    assert_eq!(gallery.pickers().date(), Date::new(2024, 3, 1).unwrap());
    let mut ui = Simulator::with_size(
        Settings::default(),
        Size::new(1000.0, 650.0),
        gallery.pickers_page(),
    );
    ui.click(
        gallery
            .pickers()
            .time_field_id(toolkit::time_picker::Part::Hour),
    )
    .unwrap();
    ui.tap_key(Named::ArrowUp);
    let messages: Vec<_> = ui.into_messages().collect();
    for message in messages {
        gallery.update(message);
    }
    assert_eq!(gallery.pickers().time(), Time::new(13, 30, 45));
    assert_eq!(gallery.pickers().date(), Date::new(2024, 3, 1).unwrap());
}

#[test]
fn tab_between_calendar_and_clock_has_one_focus_owner_and_wraps() {
    let gallery = Gallery::new();
    let mut ui = Simulator::with_size(
        Settings::default(),
        Size::new(1000.0, 650.0),
        gallery.pickers_page(),
    );
    ui.click("29").unwrap();
    ui.tap_key(Named::Tab);
    ui.tap_key(Named::ArrowUp);
    let messages: Vec<_> = ui.into_messages().collect();
    assert_eq!(
        messages
            .iter()
            .filter(|m| matches!(m, Message::Pickers(app::pickers::Message::Time(_))))
            .count(),
        1,
        "{messages:?}"
    );
    assert!(
        !messages.iter().any(
            |m| matches!(m, Message::Pickers(app::pickers::Message::Date(event))
                if !matches!(event, toolkit::date_picker::Event::Select(_)))
        ),
        "{messages:?}"
    );
}

#[test]
fn application_patterns_render_and_remain_reachable_in_a_narrow_window() {
    let directory = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("toolkit-snapshots");
    std::fs::create_dir_all(&directory).unwrap();
    for mode in Mode::ALL {
        let mut gallery = Gallery::new();
        gallery.update(Message::Mode(mode));
        for width in [900.0, 240.0] {
            let mut ui = Simulator::with_size(
                Settings::default(),
                Size::new(width, 1200.0),
                gallery.patterns_page(),
            );
            let path = directory.join(format!("patterns-{}-{width}.png", mode.name()));
            let written = directory.join(format!("patterns-{}-{width}-tiny-skia.png", mode.name()));
            let _ = std::fs::remove_file(written);
            assert!(
                ui.snapshot(&gallery.theme())
                    .unwrap()
                    .matches_image(path)
                    .unwrap()
            );
            for label in ["Back", "Help", "Enabled", "Website", "Retry", "Dismiss"] {
                if label == "Website" && ui.find(label).unwrap().visible_bounds().is_none() {
                    let about = ui
                        .find(toolkit::core::widget::Id::new("patterns-about"))
                        .unwrap()
                        .visible_bounds()
                        .unwrap();
                    ui.point_at(about.center());
                    ui.simulate([toolkit::core::Event::Mouse(
                        toolkit::core::mouse::Event::WheelScrolled {
                            delta: toolkit::core::mouse::ScrollDelta::Lines { x: 0.0, y: -100.0 },
                        },
                    )]);
                }
                let target = ui.find(label).unwrap();
                let bounds = target.visible_bounds().unwrap_or_else(|| {
                    panic!("{mode:?}/{width}: {label} is offscreen: {target:?}")
                });
                assert!(
                    bounds.width > 0.0 && bounds.x + bounds.width <= width + 1.0,
                    "{mode:?}/{width}: {label} {bounds:?}"
                );
            }
        }
    }
}

#[test]
fn composed_widgets_render_all_tokens_and_palette_activates_by_keyboard() {
    let directory = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("toolkit-snapshots");
    std::fs::create_dir_all(&directory).unwrap();
    for mode in Mode::ALL {
        let mut gallery = Gallery::new();
        gallery.update(Message::Mode(mode));
        let mut ui = Simulator::with_size(
            Settings::default(),
            Size::new(1100.0, 1400.0),
            gallery.flows_window(),
        );
        for label in [
            "Typed number",
            "Command palette",
            "Show popover",
            "File requester",
            "Drag this item",
            "Drop here",
        ] {
            assert!(
                ui.find(label).unwrap().visible_bounds().is_some(),
                "{mode:?}: {label}"
            );
        }
        let path = directory.join(format!("flows-{}.png", mode.name()));
        let written = directory.join(format!("flows-{}-tiny-skia.png", mode.name()));
        let _ = std::fs::remove_file(written);
        assert!(
            ui.snapshot(&gallery.theme())
                .unwrap()
                .matches_image(path)
                .unwrap()
        );
        ui.click("Command palette").unwrap();
        for message in ui.into_messages() {
            gallery.update(message);
        }
        let mut ui = Simulator::with_size(
            Settings::default(),
            Size::new(1100.0, 1400.0),
            gallery.flows_window(),
        );
        ui.tap_key(Named::ArrowDown);
        for message in ui.into_messages() {
            gallery.update(message);
        }
        let mut ui = Simulator::with_size(
            Settings::default(),
            Size::new(1100.0, 1400.0),
            gallery.flows_window(),
        );
        ui.tap_key(Named::Enter);
        let messages: Vec<_> = ui.into_messages().collect();
        assert!(
            messages.iter().any(|message| matches!(
                message,
                Message::Flows(app::flows::Message::Palette(
                    toolkit::command_palette::Event::Activated(1)
                ))
            )),
            "{mode:?}: {messages:?}"
        );
    }
}

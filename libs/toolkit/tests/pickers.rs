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
            for label in ["Back", "Help", "Enabled", "Website", "Retry", "Dismiss"] {
                let bounds = ui.find(label).unwrap().visible_bounds().unwrap();
                assert!(
                    bounds.width > 0.0 && bounds.x + bounds.width <= width + 1.0,
                    "{mode:?}/{width}: {label} {bounds:?}"
                );
            }
            let path = directory.join(format!("patterns-{}-{width}.png", mode.name()));
            let written = directory.join(format!("patterns-{}-{width}-tiny-skia.png", mode.name()));
            let _ = std::fs::remove_file(written);
            assert!(
                ui.snapshot(&gallery.theme())
                    .unwrap()
                    .matches_image(path)
                    .unwrap()
            );
        }
    }
}

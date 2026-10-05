// SPDX-License-Identifier: MIT OR Apache-2.0
//! The gallery's "Dialogs & toasts" page, offscreen: every dialog kind and
//! the toast stack are rendered under the dark and light token sets and
//! written as PNGs to `$CARGO_TARGET_TMPDIR/toolkit-snapshots/` for a
//! person to look at, and the page is driven through iced's headless
//! simulator with keyboard events alone, so a dialog that needed a mouse
//! would fail here.
//!
//! `cargo test -p toolkit --features gallery-tiny-skia --test services`.
//! No window, GPU or host font is used.
#![cfg(all(feature = "gallery-tiny-skia", not(feature = "wgpu")))]

#[allow(dead_code)]
#[path = "../examples/gallery/app.rs"]
mod app;

use std::path::{Path, PathBuf};

use app::services::{self, Action};
use app::{Gallery, Message, Mode, Page};
use iced_test::Simulator;
use iced_test::core::keyboard::key::{Named, NativeCode, Physical};
use iced_test::core::keyboard::{self, Key, Location, Modifiers};
use iced_test::core::{Event, Settings, Size};
use iced_test::simulator::typewrite;
use toolkit::core::Color;
use toolkit::dialog::{Dialog, Kind, Severity};
use toolkit::fonts;

/// Logical size of the simulated window; the snapshot is taken at 2x.
const VIEWPORT: Size = Size::new(1280.0, 800.0);

/// `Snapshot::matches_image` writes `<stem>-<renderer>.png`.
const RENDERER: &str = "tiny-skia";

fn output_dir() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("toolkit-snapshots")
}

fn settings() -> Settings {
    Settings {
        default_font: fonts::default_ui_font(),
        ..Settings::default()
    }
}

/// Decoded RGBA pixels of a PNG.
fn read_png(path: &Path) -> (u32, u32, Vec<u8>) {
    let decoder = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path).unwrap()));
    let mut reader = decoder.read_info().unwrap();
    let mut bytes = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut bytes).unwrap();
    assert_eq!(info.color_type, png::ColorType::Rgba);
    bytes.truncate(info.buffer_size());
    (info.width, info.height, bytes)
}

fn rgb8(colour: Color) -> [u8; 3] {
    let [r, g, b, _] = colour.into_rgba8();
    [r, g, b]
}

fn close(a: [u8; 3], b: [u8; 3]) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 3)
}

fn luminance(rgb: [u8; 3]) -> f32 {
    let [r, g, b] = rgb.map(f32::from);
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

struct Frame {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl Frame {
    fn pixel(&self, x: u32, y: u32) -> [u8; 3] {
        let at = ((y * self.width + x) * 4) as usize;
        [self.rgba[at], self.rgba[at + 1], self.rgba[at + 2]]
    }

    fn has(&self, colour: Color) -> bool {
        let wanted = rgb8(colour);
        (0..self.height).step_by(2).any(|y| {
            (0..self.width)
                .step_by(2)
                .any(|x| close(self.pixel(x, y), wanted))
        })
    }
}

fn render(gallery: &Gallery, path: &Path) -> Frame {
    let mut ui = Simulator::with_size(settings(), VIEWPORT, gallery.view());
    let snapshot = ui.snapshot(&gallery.theme()).expect("render the page");
    let stem = path.file_stem().unwrap().to_string_lossy();
    let written = path.with_file_name(format!("{stem}-{RENDERER}.png"));
    let _ = std::fs::remove_file(&written);
    assert!(
        snapshot.matches_image(path).unwrap(),
        "{} written",
        written.display()
    );
    eprintln!("wrote {}", written.display());
    let (width, height, rgba) = read_png(&written);
    assert_eq!((width, height), (2560, 1600), "2x the viewport");
    Frame {
        width,
        height,
        rgba,
    }
}

/// The gallery on its services page under `mode`.
fn services(mode: Mode) -> Gallery {
    let mut gallery = Gallery::new();
    gallery.update(Message::Mode(mode));
    gallery.update(Message::Page(Page::Services));
    assert_eq!(gallery.page(), Page::Services);
    gallery
}

fn open(gallery: &mut Gallery, action: Action) {
    gallery.update(Message::Services(services::Message::Open(action)));
}

/// Runs `events` through a fresh simulation of the current view and
/// applies every message that came out, as the runtime would; returns how
/// many there were.
fn simulate(gallery: &mut Gallery, events: Vec<Event>) -> usize {
    let mut ui = Simulator::with_size(settings(), VIEWPORT, gallery.view());
    let _ = ui.simulate(events);
    let messages: Vec<Message> = ui.into_messages().collect();
    let count = messages.len();
    for message in messages {
        gallery.update(message);
    }
    count
}

/// A press and release of `key` with `modifiers`.
fn key(key: Key, modifiers: Modifiers) -> Vec<Event> {
    let physical = Physical::Unidentified(NativeCode::Unidentified);
    vec![
        Event::Keyboard(keyboard::Event::KeyPressed {
            key: key.clone(),
            modified_key: key.clone(),
            physical_key: physical,
            location: Location::Standard,
            modifiers,
            text: None,
            repeat: false,
        }),
        Event::Keyboard(keyboard::Event::KeyReleased {
            key: key.clone(),
            modified_key: key,
            physical_key: physical,
            location: Location::Standard,
            modifiers,
        }),
    ]
}

fn tap(named: Named) -> Vec<Event> {
    key(Key::Named(named), Modifiers::empty())
}

fn ctrl(character: &str) -> Vec<Event> {
    key(Key::Character(character.into()), Modifiers::CTRL)
}

fn typed(text: &str) -> Vec<Event> {
    typewrite(text).collect()
}

fn dialog(gallery: &Gallery) -> &Dialog {
    gallery.services().dialog().expect("a dialog is open")
}

fn kind(gallery: &Gallery) -> Option<Kind> {
    gallery.services().dialog().map(Dialog::kind)
}

fn last_outcome(gallery: &Gallery) -> Option<&str> {
    gallery.services().outcomes().last().map(String::as_str)
}

#[test]
fn dialogs_are_driven_by_the_keyboard_alone() {
    let mut gallery = services(Mode::Dark);
    assert_eq!(kind(&gallery), None);

    // A bound key opens its dialog; while it is up, bound keys are not
    // routed and the key never reaches the page.
    simulate(&mut gallery, tap(Named::F3));
    assert_eq!(kind(&gallery), Some(Kind::Confirm), "F3 opens the confirm");
    simulate(&mut gallery, tap(Named::F2));
    assert_eq!(
        kind(&gallery),
        Some(Kind::Confirm),
        "F2 is swallowed by the dialog"
    );
    assert_eq!(gallery.services().queued(), 0);

    // Tab, Enter.
    assert_eq!(
        dialog(&gallery).focused_button(),
        Some(0),
        "Delete has focus"
    );
    simulate(&mut gallery, tap(Named::Tab));
    assert_eq!(
        dialog(&gallery).focused_button(),
        Some(1),
        "Tab moves to Keep"
    );
    simulate(&mut gallery, tap(Named::Enter));
    assert_eq!(kind(&gallery), None, "Enter on Keep closes it");
    assert_eq!(last_outcome(&gallery), Some("cancelled"));

    // Shift+Tab wraps; Enter on the destructive button reports its index.
    simulate(&mut gallery, tap(Named::F3));
    simulate(&mut gallery, key(Key::Named(Named::Tab), Modifiers::SHIFT));
    assert_eq!(
        dialog(&gallery).focused_button(),
        Some(1),
        "Shift+Tab wraps to Keep"
    );
    simulate(&mut gallery, key(Key::Named(Named::Tab), Modifiers::SHIFT));
    assert_eq!(dialog(&gallery).focused_button(), Some(0));
    simulate(&mut gallery, tap(Named::Enter));
    assert_eq!(last_outcome(&gallery), Some("pressed button 0"));

    // Escape cancels.
    simulate(&mut gallery, tap(Named::F3));
    simulate(&mut gallery, tap(Named::Escape));
    assert_eq!(kind(&gallery), None);
    assert_eq!(last_outcome(&gallery), Some("cancelled"));

    // The prompt's field is focused on open: typing lands in it and Enter
    // returns the text.
    simulate(&mut gallery, tap(Named::F4));
    assert_eq!(kind(&gallery), Some(Kind::Prompt));
    assert_eq!(dialog(&gallery).text(), "Untitled");
    assert!(dialog(&gallery).is_leading_focused());
    simulate(&mut gallery, typed("-2"));
    assert_eq!(
        dialog(&gallery).text(),
        "Untitled-2",
        "typed into the focused field"
    );
    simulate(&mut gallery, tap(Named::Enter));
    assert_eq!(kind(&gallery), None);
    assert_eq!(last_outcome(&gallery), Some("text: Untitled-2"));

    // Tab leaves the field: typing on a button types nothing; Shift+Tab
    // returns to the field.
    simulate(&mut gallery, tap(Named::F5));
    assert_eq!(kind(&gallery), Some(Kind::Secret));
    simulate(&mut gallery, tap(Named::Tab));
    assert_eq!(dialog(&gallery).focused_button(), Some(0));
    simulate(&mut gallery, typed("x"));
    assert_eq!(dialog(&gallery).text(), "", "a button does not take text");
    simulate(&mut gallery, key(Key::Named(Named::Tab), Modifiers::SHIFT));
    assert!(dialog(&gallery).is_leading_focused());
    simulate(&mut gallery, typed("hunter2"));
    assert_eq!(dialog(&gallery).text(), "hunter2");
    simulate(&mut gallery, tap(Named::Enter));
    assert_eq!(last_outcome(&gallery), Some("text: hunter2"));

    // The choice moves with the arrows.
    simulate(&mut gallery, tap(Named::F6));
    assert_eq!(kind(&gallery), Some(Kind::Choice));
    simulate(&mut gallery, tap(Named::ArrowDown));
    simulate(&mut gallery, tap(Named::ArrowDown));
    assert_eq!(dialog(&gallery).selection(), 2);
    simulate(&mut gallery, tap(Named::ArrowUp));
    assert_eq!(dialog(&gallery).selection(), 1);
    simulate(&mut gallery, tap(Named::Enter));
    assert_eq!(last_outcome(&gallery), Some("chose option 1"));

    // A cancellable progress dialog goes on Escape.
    simulate(&mut gallery, tap(Named::F7));
    assert_eq!(kind(&gallery), Some(Kind::Progress));
    simulate(&mut gallery, tap(Named::Escape));
    assert_eq!(kind(&gallery), None);

    // Three dialogs offered at once show one at a time.
    simulate(&mut gallery, tap(Named::F9));
    assert_eq!(kind(&gallery), Some(Kind::Message(Severity::Info)));
    assert_eq!(gallery.services().queued(), 2);
    simulate(&mut gallery, tap(Named::Enter));
    assert_eq!(gallery.services().queued(), 1);
    assert_eq!(last_outcome(&gallery), Some("accepted"));
    simulate(&mut gallery, tap(Named::Enter));
    simulate(&mut gallery, tap(Named::Enter));
    assert_eq!(kind(&gallery), None);
    assert_eq!(gallery.services().queued(), 0);
}

#[test]
fn toasts_open_from_keys_and_escape_dismisses_the_newest() {
    let mut gallery = services(Mode::Light);
    simulate(&mut gallery, ctrl("1"));
    assert_eq!(
        gallery.services().toaster().len(),
        1,
        "Ctrl+1 pushes a toast"
    );
    simulate(&mut gallery, ctrl("4"));
    assert_eq!(gallery.services().toaster().len(), 2);
    let newest = gallery.services().toaster().iter().last().unwrap().0;
    simulate(&mut gallery, tap(Named::Escape));
    assert_eq!(
        gallery.services().toaster().len(),
        1,
        "Escape dismisses one"
    );
    assert!(!gallery.services().toaster().contains(newest), "the newest");
    // Escape with a dialog up goes to the dialog, not the toasts.
    simulate(&mut gallery, tap(Named::F3));
    simulate(&mut gallery, tap(Named::Escape));
    assert_eq!(kind(&gallery), None);
    assert_eq!(gallery.services().toaster().len(), 1);
}

#[test]
fn dialogs_and_toasts_render_under_dark_and_light() {
    let dir = output_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let cases = [
        (Mode::Dark, Action::Confirm, "confirm"),
        (Mode::Light, Action::Prompt, "prompt"),
        (Mode::Dark, Action::Choice, "choice"),
        (Mode::Light, Action::Progress, "progress"),
        (Mode::Dark, Action::Message, "message"),
        (Mode::Light, Action::Busy, "busy"),
    ];
    for (mode, action, name) in cases {
        let mut gallery = services(mode);
        open(&mut gallery, action);
        assert!(gallery.services().dialog().is_some());
        let frame = render(
            &gallery,
            &dir.join(format!("services-{name}-{}.png", mode.name())),
        );
        let palette = mode.tokens().palette;
        let surface = rgb8(palette.surface);
        let corner = frame.pixel(4, frame.height - 4);
        // The scrim: the darker of surface and text at 55%. Over a dark
        // surface that is the surface itself; over a light one it dims.
        if palette.surface.relative_luminance() <= palette.text.relative_luminance() {
            assert!(
                close(corner, surface),
                "{name} {mode:?}: corner {corner:?} is not the surface"
            );
        } else {
            assert!(
                luminance(corner) < luminance(surface) - 40.0,
                "{name} {mode:?}: corner {corner:?} is not dimmed from {surface:?}"
            );
        }
        // The card's controls are on screen: a primary or destructive fill
        // (buttons, the progress bar) or the sliding bar's track.
        assert!(
            frame.has(palette.primary)
                || frame.has(palette.destructive)
                || frame.has(palette.muted_surface),
            "{name} {mode:?}: no dialog control colour found"
        );
    }
    for mode in [Mode::Dark, Mode::Light] {
        let mut gallery = services(mode);
        for action in [
            Action::ToastInfo,
            Action::ToastSuccess,
            Action::ToastWarning,
            Action::ToastError,
        ] {
            open(&mut gallery, action);
        }
        assert_eq!(gallery.services().toaster().len(), 4);
        let frame = render(
            &gallery,
            &dir.join(format!("services-toasts-{}.png", mode.name())),
        );
        let palette = mode.tokens().palette;
        assert!(
            close(frame.pixel(4, frame.height - 4), rgb8(palette.surface)),
            "{mode:?}: no scrim without a dialog"
        );
        assert!(
            frame.has(palette.destructive),
            "{mode:?}: the error toast's outline"
        );
        assert!(
            frame.has(gallery.theme().semantic().success),
            "{mode:?}: the success toast's outline"
        );
        assert!(frame.has(palette.elevated), "{mode:?}: the toast cards");
    }
}

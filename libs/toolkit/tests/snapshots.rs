// SPDX-License-Identifier: MIT OR Apache-2.0
//! Offscreen gallery snapshots: the gallery page under each token set,
//! rendered by iced's headless simulator with the software renderer and
//! written as PNGs to `$CARGO_TARGET_TMPDIR/toolkit-snapshots/` for a
//! person to look at. The checks here are the ones a machine can make: the
//! frame is cleared to the tokens' surface colour, a `primary`-filled
//! control is on screen, and the sets differ from one another.
//!
//! `cargo test -p toolkit --features gallery-tiny-skia --test snapshots`
//! (the `[[test]]` entry requires the feature, so a plain `cargo test`
//! skips it). No window, GPU or host font is used. With the `wgpu` feature
//! on as well (`--all-features`) iced's fallback renderer would try a GPU
//! first, so the tests are compiled out.
#![cfg(all(feature = "gallery-tiny-skia", not(feature = "wgpu")))]

// The gallery program as a whole; its `run` is for the examples.
#[allow(dead_code)]
#[path = "../examples/gallery/app.rs"]
mod app;

use std::path::{Path, PathBuf};

use app::{Gallery, Message, Mode};
use iced_test::Simulator;
use iced_test::core::{Settings, Size};
use toolkit::core::Color;
use toolkit::fonts;

/// Logical size of the simulated window; the snapshot is taken at 2x.
const VIEWPORT: Size = Size::new(1280.0, 1900.0);

/// `Snapshot::matches_image` writes `<stem>-<renderer>.png`.
const RENDERER: &str = "tiny-skia";

fn output_dir() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR")).join("toolkit-snapshots")
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

fn render(gallery: &Gallery, path: &Path) -> (u32, u32, Vec<u8>) {
    render_element(gallery.view(), &gallery.theme(), VIEWPORT, path)
}

fn render_element<M>(
    element: toolkit::core::Element<'_, M, toolkit::Theme>,
    theme: &toolkit::Theme,
    viewport: Size,
    path: &Path,
) -> (u32, u32, Vec<u8>) {
    let settings = Settings {
        default_font: fonts::default_ui_font(),
        ..Settings::default()
    };
    let mut ui = Simulator::with_size(settings, viewport, element);
    let snapshot = ui.snapshot(theme).expect("render the gallery");
    let stem = path.file_stem().unwrap().to_string_lossy();
    let written = path.with_file_name(format!("{stem}-{RENDERER}.png"));
    let _ = std::fs::remove_file(&written);
    assert!(
        snapshot.matches_image(path).unwrap(),
        "{} written",
        written.display()
    );
    eprintln!("wrote {}", written.display());
    read_png(&written)
}

#[test]
fn gallery_renders_under_every_token_set() {
    let dir = output_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let mut gallery = Gallery::new();
    let mut frames = Vec::new();
    for mode in Mode::ALL {
        gallery.update(Message::Mode(mode));
        assert_eq!(gallery.mode(), mode);
        let tokens = mode.tokens();
        let path = dir.join(format!("gallery-{}.png", mode.name()));
        let (width, height, rgba) = render(&gallery, &path);
        assert_eq!((width, height), (2560, 3800), "{mode:?}: 2x the viewport");
        let pixel = |x: u32, y: u32| {
            let at = ((y * width + x) * 4) as usize;
            [rgba[at], rgba[at + 1], rgba[at + 2]]
        };
        // The frame is cleared to the surface colour: the page's padding
        // corner shows it.
        assert!(
            close(pixel(4, height - 4), rgb8(tokens.palette.surface)),
            "{mode:?}: corner {:?} is not the surface {:?}",
            pixel(4, height - 4),
            rgb8(tokens.palette.surface)
        );
        // A primary-filled control (the selected theme button, the primary
        // button, the slider fill) is on screen.
        let primary = rgb8(tokens.palette.primary);
        assert!(
            (0..height)
                .step_by(2)
                .any(|y| (0..width).step_by(2).any(|x| close(pixel(x, y), primary))),
            "{mode:?}: no pixel in the primary colour {primary:?}"
        );
        frames.push(rgba);
    }
    assert_ne!(frames[0], frames[1], "dark and light differ");
    assert_ne!(frames[1], frames[2], "light and custom differ");
}

/// The "Lists & trees" page on its own, dark and light: the virtual list's
/// first row is selected, so the selection colour is on screen, and the
/// two token sets differ.
#[test]
fn lists_page_renders_dark_and_light() {
    const PAGE: Size = Size::new(1280.0, 520.0);
    let dir = output_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let mut gallery = Gallery::new();
    let mut frames = Vec::new();
    for mode in [Mode::Dark, Mode::Light] {
        gallery.update(Message::Mode(mode));
        let tokens = mode.tokens();
        let path = dir.join(format!("lists-{}.png", mode.name()));
        let (width, height, rgba) =
            render_element(gallery.lists_page(), &gallery.theme(), PAGE, &path);
        assert_eq!((width, height), (2560, 1040), "{mode:?}: 2x the page");
        let pixel = |x: u32, y: u32| {
            let at = ((y * width + x) * 4) as usize;
            [rgba[at], rgba[at + 1], rgba[at + 2]]
        };
        let selection = rgb8(tokens.palette.selection);
        assert!(
            (0..height)
                .step_by(2)
                .any(|y| (0..width).step_by(2).any(|x| close(pixel(x, y), selection))),
            "{mode:?}: no pixel in the selection colour {selection:?}"
        );
        frames.push(rgba);
    }
    assert_ne!(frames[0], frames[1], "dark and light differ");
}

/// The "Text" page on its own, dark and light: an elided label and a
/// fitted headline are on screen (text drawn, not just boxes), and the
/// two token sets differ.
#[test]
fn text_page_renders_dark_and_light() {
    const PAGE: Size = Size::new(1280.0, 520.0);
    let dir = output_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let mut gallery = Gallery::new();
    let mut frames = Vec::new();
    for mode in [Mode::Dark, Mode::Light] {
        gallery.update(Message::Mode(mode));
        let path = dir.join(format!("text-{}.png", mode.name()));
        let (width, height, rgba) =
            render_element(gallery.text_page(), &gallery.theme(), PAGE, &path);
        assert_eq!((width, height), (2560, 1040), "{mode:?}: 2x the page");
        let pixel = |x: u32, y: u32| {
            let at = ((y * width + x) * 4) as usize;
            [rgba[at], rgba[at + 1], rgba[at + 2]]
        };
        let text = rgb8(mode.tokens().palette.text);
        assert!(
            (0..height)
                .step_by(2)
                .any(|y| (0..width).step_by(2).any(|x| close(pixel(x, y), text))),
            "{mode:?}: no pixel in the text colour {text:?}"
        );
        frames.push(rgba);
    }
    assert_ne!(frames[0], frames[1], "dark and light differ");
}

/// Swapping tokens in a running program restyles the next frame: the same
/// gallery state renders two different frames with no rebuild.
#[test]
fn theme_swap_restyles_the_next_frame() {
    let dir = output_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let mut gallery = Gallery::new();
    let first = render(&gallery, &dir.join("swap-before.png"));
    gallery.update(Message::Mode(Mode::Light));
    let second = render(&gallery, &dir.join("swap-after.png"));
    assert_eq!(first.0, second.0);
    assert_ne!(first.2, second.2);
    let light = rgb8(Mode::Light.tokens().palette.surface);
    let at = ((second.1 - 4) * second.0 + 4) as usize * 4;
    assert!(close(
        [second.2[at], second.2[at + 1], second.2[at + 2]],
        light
    ));
}

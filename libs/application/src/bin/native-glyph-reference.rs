// SPDX-License-Identifier: MIT OR Apache-2.0
//! Offscreen oracle only. No Wayland, Bus, app icon mapping or cached raster.
use application::iced::advanced::graphics::text::{cosmic_text::fontdb, font_system};
use application::iced::advanced::{
    renderer::{Headless, Settings},
    text::{self, Renderer as _},
};
use application::iced::{Color, Font, Pixels, Rectangle, Size, alignment, font};
use serde::Deserialize;
use std::{borrow::Cow, io::Write};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    font_path: String,
    family: String,
    glyph: char,
    size: [u32; 2],
    scale: f32,
    bounds: [f32; 4],
    foreground: [u8; 4],
    background: [u8; 3],
    #[serde(default)]
    neighbours: Vec<Neighbour>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Neighbour {
    glyph: char,
    bounds: [f32; 4],
    foreground: [u8; 4],
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 2 {
        return Err("REQUEST_JSON OUTPUT_PPM".into());
    }
    let request: Request = serde_json::from_slice(&std::fs::read(&args[0])?)?;
    if request.size.iter().any(|n| *n == 0 || *n > 4096)
        || ![1.0, 2.0].contains(&request.scale)
        || request
            .bounds
            .iter()
            .any(|n| !n.is_finite() || n.abs() > 8192.0)
        || request.bounds[2] <= 0.0
        || request.bounds[3] <= 0.0
        || request.neighbours.len() > 64
        || request.neighbours.iter().any(|glyph| {
            glyph
                .bounds
                .iter()
                .any(|n| !n.is_finite() || n.abs() > 8192.0)
                || glyph.bounds[2] <= 0.0
                || glyph.bounds[3] <= 0.0
        })
    {
        return Err("invalid bounded raster geometry".into());
    }
    let bytes = std::fs::read(&request.font_path)?;
    {
        let mut system = font_system().write().map_err(|_| "font lock poisoned")?;
        system.load_font(Cow::Owned(bytes.clone()));
        let raw = system.raw();
        let id = raw
            .db()
            .query(&fontdb::Query {
                families: &[fontdb::Family::Name(&request.family)],
                weight: fontdb::Weight::NORMAL,
                ..Default::default()
            })
            .ok_or("locked named font not selected")?;
        // An identically named ambient font must never become the oracle.
        let exact = raw
            .db()
            .with_face_data(id, |selected, face| selected == bytes && face == 0)
            .unwrap_or(false);
        if !exact {
            return Err("oracle selected another font source or face".into());
        }
        let selected = raw
            .get_font(id, fontdb::Weight::NORMAL)
            .ok_or("font unavailable")?;
        if std::iter::once(request.glyph)
            .chain(request.neighbours.iter().map(|glyph| glyph.glyph))
            .any(|glyph| selected.as_swash().charmap().map(glyph) == 0)
        {
            return Err("locked glyph missing from actual selected face".into());
        }
    }
    // Keep the family alive for the renderer's static descriptor.
    let family: &'static str = Box::leak(request.family.into_boxed_str());
    let mut renderer = application::cpu::Renderer::new(Settings::default());
    let target = Neighbour {
        glyph: request.glyph,
        bounds: request.bounds,
        foreground: request.foreground,
    };
    for glyph in std::iter::once(&target).chain(request.neighbours.iter()) {
        let bounds = Rectangle {
            x: glyph.bounds[0],
            y: glyph.bounds[1],
            width: glyph.bounds[2],
            height: glyph.bounds[3],
        };
        let [r, g, b, a] = glyph.foreground;
        renderer.fill_text(
            text::Text {
                content: glyph.glyph.to_string(),
                bounds: bounds.size(),
                size: Pixels(bounds.height),
                line_height: text::LineHeight::Absolute(Pixels(bounds.height)),
                font: Font {
                    family: font::Family::Name(family),
                    weight: font::Weight::Normal,
                    ..Font::DEFAULT
                },
                align_x: text::Alignment::Center,
                align_y: alignment::Vertical::Center,
                shaping: text::Shaping::Advanced,
                wrapping: text::Wrapping::None,
                ellipsis: text::Ellipsis::None,
                hint_factor: None,
            },
            bounds.center(),
            Color::from_rgba8(r, g, b, f32::from(a) / 255.0),
            Rectangle::with_size(Size::new(request.size[0] as f32, request.size[1] as f32)),
        );
    }
    let [r, g, b] = request.background;
    let pixels = renderer.screenshot(
        Size::new(request.size[0], request.size[1]),
        request.scale,
        Color::from_rgb8(r, g, b),
    );
    let mut output = std::fs::File::create(&args[1])?;
    write!(output, "P6\n{} {}\n255\n", request.size[0], request.size[1])?;
    for pixel in pixels.chunks_exact(4) {
        output.write_all(&pixel[..3])?;
    }
    Ok(())
}

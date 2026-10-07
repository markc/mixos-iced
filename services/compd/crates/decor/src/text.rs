//! The title, shaped and rasterised with cosmic-text into a coverage mask,
//! elided at the end to the width it may take.
//!
//! The font system is iced's process-wide one (`iced_graphics::text::
//! font_system`): the faces compd already loaded, including the asset set's
//! sans (or, without a set, the embedded Inter) that `main` installs as the
//! sans-serif default (ui `font::default`), so a machine with no fonts
//! installed still gets a title. A missing face
//! falls back the way cosmic-text always does; no usable face gives an empty
//! mask, never a failure.

use std::cell::RefCell;

use crate::layout::DecoFontFamily;
use iced_graphics::text::cosmic_text::{
    Attrs, Buffer, Color, Ellipsize, EllipsizeHeightLimit, Family, Metrics, Shaping, SwashCache,
    Weight, Wrap,
};

thread_local! {
    static GLYPHS: RefCell<Option<SwashCache>> = const { RefCell::new(None) };
}

/// An 8-bit coverage mask, row-major from the top-left.
#[derive(Clone, Debug, PartialEq)]
pub struct TextMask {
    pub width: u32,
    pub height: u32,
    pub alpha: Vec<u8>,
}

impl TextMask {
    pub fn at(&self, x: u32, y: u32) -> u8 {
        self.alpha[(y * self.width + x) as usize]
    }
}

/// `title` in `family` at `size_px` (logical) and `weight`, at `scale`
/// physical px per logical px, no wider than `max_width` physical px (elided
/// with an ellipsis past it). `None` for an empty title or no room.
pub fn title_mask(
    title: &str,
    family: &DecoFontFamily,
    size_px: f32,
    weight: u16,
    max_width: f32,
    scale: f32,
) -> Option<TextMask> {
    let title = title.trim();
    if title.is_empty() || max_width < 1.0 || size_px <= 0.0 || scale <= 0.0 {
        return None;
    }
    let Ok(mut fonts) = iced_graphics::text::font_system().write() else {
        return None;
    };
    let font_system = fonts.raw();
    GLYPHS.with_borrow_mut(|cache| {
        let cache = cache.get_or_insert_with(SwashCache::new);
        let font_size = size_px * scale;
        let line_height = (font_size * 1.3).ceil();
        let mut buffer = Buffer::new(font_system, Metrics::new(font_size, line_height));
        let family = match family {
            DecoFontFamily::Named(name) => Family::Name(name.as_str()),
            DecoFontFamily::SystemUi => Family::SansSerif,
            DecoFontFamily::Monospace => Family::Monospace,
        };
        let attrs = Attrs::new().family(family).weight(Weight(weight));
        {
            let mut buffer = buffer.borrow_with(font_system);
            buffer.set_wrap(Wrap::None);
            buffer.set_ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1)));
            buffer.set_size(Some(max_width), Some(line_height));
            buffer.set_text(title, &attrs, Shaping::Advanced, None);
            buffer.shape_until_scroll(false);
        }
        let width = buffer
            .layout_runs()
            .map(|run| run.line_w)
            .fold(0.0f32, f32::max)
            .ceil()
            .min(max_width.floor()) as u32;
        let height = line_height as u32;
        if width == 0 || height == 0 {
            return None;
        }
        let mut mask = TextMask {
            width,
            height,
            alpha: vec![0; (width * height) as usize],
        };
        buffer.draw(
            font_system,
            cache,
            Color::rgba(255, 255, 255, 255),
            |x, y, w, h, colour| {
                let a = colour.a();
                if a == 0 {
                    return;
                }
                for yy in y.max(0)..(y + h as i32).min(height as i32) {
                    for xx in x.max(0)..(x + w as i32).min(width as i32) {
                        let at = (yy as u32 * width + xx as u32) as usize;
                        // Coverage accumulates as alpha-over (glyph boxes overlap).
                        let dst = mask.alpha[at] as u32;
                        mask.alpha[at] = (a as u32 + dst * (255 - a as u32) / 255).min(255) as u8;
                    }
                }
            },
        );
        Some(mask)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_title_or_no_room_draws_nothing() {
        let family = DecoFontFamily::SystemUi;
        assert_eq!(title_mask("   ", &family, 13.0, 400, 200.0, 1.0), None);
        assert_eq!(title_mask("Files", &family, 13.0, 400, 0.0, 1.0), None);
    }

    /// Whatever fonts the machine has, the mask never exceeds its width.
    #[test]
    fn a_long_title_is_held_to_its_width() {
        let family = DecoFontFamily::SystemUi;
        let long = "a very long window title that cannot possibly fit ".repeat(10);
        if let Some(mask) = title_mask(&long, &family, 13.0, 400, 120.0, 1.0) {
            assert!(mask.width <= 120, "{}", mask.width);
            assert_eq!(mask.alpha.len(), (mask.width * mask.height) as usize);
        }
    }
}

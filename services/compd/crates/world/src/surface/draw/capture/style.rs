//! Shared colors/metrics for the capture overlays.

use iced_core::Color;

/// Dim (not opaque) mask used by the setup overlay outside the hole — the user
/// must still see the screen to aim the region.
pub const SETUP_MASK: Color = Color::from_rgba(0.0, 0.0, 0.0, 0.5);
/// Semi-dark backdrop drawn below windows during capture (outside the region).
pub const DIM_MASK: Color = Color::from_rgba(0.0, 0.0, 0.0, 0.45);
/// Bright accent for the region border + selected chooser items.
pub const ACCENT: Color = Color::from_rgb(0.20, 0.62, 1.0);
/// Panel background for the chooser / dialog.
pub const PANEL_BG: Color = Color::from_rgba(0.08, 0.09, 0.11, 0.95);
/// Default button background.
pub const BUTTON_BG: Color = Color::from_rgba(0.16, 0.17, 0.20, 1.0);
/// Stop button (destructive) background.
pub const STOP_BG: Color = Color::from_rgb(0.78, 0.20, 0.22);
pub const TEXT: Color = Color::from_rgb(0.92, 0.93, 0.95);
pub const TEXT_DIM: Color = Color::from_rgb(0.66, 0.68, 0.72);

pub const BORDER_WIDTH: f32 = 2.0;
pub const RADIUS: f32 = 6.0;

use iced_core::{Background, Border, Shadow};
use iced_widget::button;

fn lighten(c: Color, amount: f32) -> Color {
    Color {
        r: (c.r + amount).min(1.0),
        g: (c.g + amount).min(1.0),
        b: (c.b + amount).min(1.0),
        a: c.a,
    }
}

/// A button style with the given base background, lightened on hover/press.
pub fn button_with(base: Color) -> impl Fn(&iced_core::Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let bg = match status {
            button::Status::Hovered => lighten(base, 0.08),
            button::Status::Pressed => lighten(base, 0.16),
            _ => base,
        };
        button::Style {
            background: Some(Background::Color(bg)),
            text_color: TEXT,
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: RADIUS.into(),
            },
            shadow: Shadow::default(),
            snap: true,
        }
    }
}

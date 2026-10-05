// SPDX-License-Identifier: MIT OR Apache-2.0
//! Focus conventions for widget authors: [`Source`] records *how* a
//! widget most recently gained focus, and [`ring`] draws the
//! `:focus-visible` halo that follows from it.
//!
//! A focus ring that appears on every click teaches users to ignore it.
//! The rule the web settled on — and we follow — is that the ring is a
//! *keyboard navigation* affordance: draw it when focus arrived by
//! keyboard, not by mouse. A widget stores `Option<Source>` in its state
//! (`None` = not focused), paints the ring only for
//! [`Source::Keyboard`], and re-arms it on the next keyboard interaction.

use iced_core::border::Border;
use iced_core::{Background, Color, Rectangle, Renderer, renderer};

/// How a focusable widget most recently gained focus.
///
/// Widgets that draw a focus ring only under keyboard navigation store
/// this behind an [`Option`] — `None` meaning "not focused" — and paint
/// the ring only for [`Keyboard`](Self::Keyboard), the analog of CSS
/// `:focus-visible`. Clicking a widget focuses it for subsequent keyboard
/// use but records [`Mouse`](Self::Mouse), so no ring is shown until the
/// next keyboard interaction re-arms [`Keyboard`](Self::Keyboard).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Focus arrived via Tab, a programmatic focus operation, or keyboard
    /// navigation / activation.
    Keyboard,
    /// Focus arrived by clicking or tapping the widget.
    Mouse,
}

/// Gap between the control's edge and the ring band.
const GAP: f32 = 3.0;

/// Draws a soft `:focus-visible` halo hugging a control.
///
/// `bounds` and `radius` are the control's own bounds and corner radius
/// (pass `height / 2.0` for a circle like a radio dot, or the box radius
/// for a checkbox). The band is expanded outward by a small gap and kept
/// concentric with the control — `radius + GAP` — so its corners parallel
/// the control's. It is thin and drawn at reduced alpha so it reads as a
/// glow rather than a hard outline; `color` is the theme's focus colour
/// (for toolkit tokens, `Palette::ring`).
pub fn ring<R: Renderer>(renderer: &mut R, bounds: Rectangle, radius: f32, color: Color) {
    renderer.fill_quad(
        renderer::Quad {
            bounds: bounds.expand(GAP),
            border: Border {
                radius: (radius + GAP).into(),
                width: 2.0,
                color: color.scale_alpha(0.4),
            },
            ..renderer::Quad::default()
        },
        Background::Color(Color::TRANSPARENT),
    );
}

// SPDX-License-Identifier: MIT OR Apache-2.0
//! MixOS glue kept outside the generic toolkit (source-layout decision).
pub mod fonts;
pub mod tokens;

/// Apply a caller's resolved border width to the generic tooltip style.
pub fn tooltip_style(mut tokens: toolkit::Tokens, width: f32) -> iced_widget::container::Style {
    tokens.metrics.border.width = width;
    tokens.tooltip_style()
}

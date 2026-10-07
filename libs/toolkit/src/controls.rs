// SPDX-License-Identifier: MIT OR Apache-2.0
//! Shared control metrics: the spacing arithmetic every standard control
//! repeats, derived once from [`Tokens`] so density affects real layout.
//! These are shared defaults, not compulsory sizes: the explicit widget
//! size, padding and list builders remain available.
use iced_core::Padding;

use crate::tokens::{Spacing, Tokens};
use crate::typography::TextStyle;

/// Control padding, row padding, gaps and derived row geometry from the
/// tokens' spacing scale.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    spacing: Spacing,
}

impl Metrics {
    /// The shared metrics of `tokens`. The spacing fields already carry the
    /// projected density; they are never multiplied again here.
    pub fn from_tokens(tokens: Tokens) -> Self {
        Self {
            spacing: tokens.metrics.spacing,
        }
    }

    /// Padding of an input-like control: vertical `sm + xs`, horizontal
    /// `md + xs` (6 and 10 at the default scale).
    pub fn padding(self) -> Padding {
        Padding::from([
            self.spacing.sm + self.spacing.xs,
            self.spacing.md + self.spacing.xs,
        ])
    }

    /// Padding of a list or menu row: vertical `xs`, horizontal `md`
    /// (2 and 8 at the default scale).
    pub fn row_padding(self) -> Padding {
        Padding::from([self.spacing.xs, self.spacing.md])
    }

    /// The gap between sibling controls and rows.
    pub fn gap(self) -> f32 {
        self.spacing.sm
    }

    /// The inset of a framed group (list viewport, panel content).
    pub fn inset(self) -> f32 {
        self.spacing.sm
    }

    /// The height of one control row with `text` plus its vertical padding:
    /// the text's minimum height grown by `2 * xs`.
    pub fn row_height<F>(self, text: &TextStyle<F>) -> f32 {
        text.minimum_height() + 2.0 * self.spacing.xs
    }

    /// The per-depth indent of a tree row: at least `lg + sm`, and at least
    /// the row height so an expander and a guide always fit.
    /// [`crate::tree::TreeView::metrics`] consumes this for its prepared
    /// rows.
    pub fn indent<F>(self, text: &TextStyle<F>) -> f32 {
        self.row_height(text).max(self.spacing.lg + self.spacing.sm)
    }
}

impl Default for Metrics {
    /// The shared metrics of the default tokens: the exact frozen geometry
    /// of the legacy views, which never follows a host's prepared tokens.
    fn default() -> Self {
        Self::from_tokens(Tokens::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typography::TextStyle;

    #[test]
    fn the_default_spacing_reproduces_the_familiar_padding() {
        let metrics = Metrics::from_tokens(Tokens::default());
        // The axes are named explicitly: `Padding::from([f32; 2])` reads
        // `[vertical, horizontal]`, so only the fields can catch a swap.
        let padding = metrics.padding();
        assert_eq!(padding.top, 6.0);
        assert_eq!(padding.bottom, 6.0);
        assert_eq!(padding.left, 10.0);
        assert_eq!(padding.right, 10.0);
        let row = metrics.row_padding();
        assert_eq!(row.top, 2.0);
        assert_eq!(row.bottom, 2.0);
        assert_eq!(row.left, 8.0);
        assert_eq!(row.right, 8.0);
        assert_eq!(metrics.gap(), 4.0);
        assert_eq!(metrics.inset(), 4.0);
    }

    #[test]
    fn density_scales_layout_once_through_the_prepared_spacing() {
        let mut tokens = Tokens::dark();
        tokens.metrics.spacing.xs *= 1.5;
        tokens.metrics.spacing.sm *= 1.5;
        tokens.metrics.spacing.md *= 1.5;
        tokens.metrics.spacing.lg *= 1.5;
        let metrics = Metrics::from_tokens(tokens);
        let padding = metrics.padding();
        assert_eq!(padding.top, 9.0);
        assert_eq!(padding.bottom, 9.0);
        assert_eq!(padding.left, 15.0);
        assert_eq!(padding.right, 15.0);
        let row = metrics.row_padding();
        assert_eq!(row.top, 3.0);
        assert_eq!(row.bottom, 3.0);
        assert_eq!(row.left, 12.0);
        assert_eq!(row.right, 12.0);
        assert_eq!(metrics.gap(), 6.0);
    }

    #[test]
    fn the_default_metrics_freeze_the_legacy_geometry() {
        let frozen = Metrics::default();
        assert_eq!(frozen, Metrics::from_tokens(Tokens::default()));
        assert_eq!(frozen.gap(), 4.0);
        assert_eq!(frozen.inset(), 4.0);
    }

    #[test]
    fn row_height_and_indent_follow_the_text_floor() {
        let metrics = Metrics::from_tokens(Tokens::default());
        let text = TextStyle {
            font: iced_core::Font::DEFAULT,
            size: 20.0,
            line_height: Some(12.0),
        };
        // The floor is the text size (20), plus 2 * xs (4).
        assert_eq!(metrics.row_height(&text), 24.0);
        // The indent is at least lg + sm (20) and at least the row height.
        assert_eq!(metrics.indent(&text), 24.0);
        let tall = TextStyle {
            font: iced_core::Font::DEFAULT,
            size: 14.0,
            line_height: Some(30.0),
        };
        assert_eq!(metrics.row_height(&tall), 34.0);
        assert_eq!(metrics.indent(&tall), 34.0);
    }
}

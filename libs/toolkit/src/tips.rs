// SPDX-License-Identifier: MIT OR Apache-2.0
//! Tooltips: [`tip`] styles one from the [`Tokens`], and [`regions`]
//! builds only the *visible* hit regions over custom-drawn content — the
//! pattern for widgets that paint their own pixels (a piano roll, a
//! meter strip, a map) and still want per-region hover tips.
//!
//! The hit-region children draw no pixels themselves: each is a `Space`
//! sized to its region inside a stock iced tooltip, which supplies the
//! hover timing and the viewport-clamped overlay. The parent lays them
//! out over its own drawing and returns the node, so the tips move and
//! scroll with the content for free.

use iced_core::layout;
use iced_core::widget::Tree;
use iced_core::widget::text::{self, Text};
use iced_core::{Element, Rectangle, Size};
use iced_widget::tooltip::{self, Tooltip};
use iced_widget::{Space, container};

use crate::tokens::Tokens;

/// A themed tooltip over `content`: the label is set in the tokens' small
/// text size with advanced shaping, and the card takes
/// [`Tokens::tooltip_style`].
///
/// The compiled design dictionary has no timing tokens, so the tooltip
/// keeps iced's default delay until a design supplies a duration role.
pub fn tip<'a, M, Theme, Renderer>(
    tokens: &Tokens,
    content: impl Into<Element<'a, M, Theme, Renderer>>,
    label: impl Into<String>,
) -> Element<'a, M, Theme, Renderer>
where
    Theme: container::Catalog + text::Catalog + 'a,
    <Theme as container::Catalog>::Class<'a>: From<container::StyleFn<'a, Theme>>,
    Renderer: iced_core::text::Renderer + 'a,
    M: 'a,
{
    let label = label.into();
    let tooltip_view = Text::<Theme, Renderer>::new(label)
        .size(tokens.metrics.text.sm)
        .shaping(iced_core::text::Shaping::Advanced)
        .wrapping(iced_core::text::Wrapping::WordOrGlyph);
    let pad = tokens.metrics.spacing.sm;
    let gap = tokens.metrics.spacing.xs;
    // `Tokens` is plain data: the clone frees the tooltip's style closure
    // from this borrow, so hit regions can be built as `'static` children.
    let tokens = tokens.clone();
    Tooltip::new(content, tooltip_view, tooltip::Position::Bottom)
        .padding(pad)
        .gap(gap)
        .style(move |_| tokens.tooltip_style())
        .into()
}

/// Lay the given `(bounds, label)` tooltip regions out as invisible
/// children over a custom-drawn widget.
///
/// Call this from the parent's [`layout`](iced_core::Widget::layout):
/// `children` is the parent's child-element scratch vector (reused
/// between frames, so no reallocation), `tree` the parent's widget tree
/// (diffed here), and `size` the parent's own laid-out size. The returned
/// node positions each region at its bounds; drawing nothing, the
/// children exist purely so iced's tooltips can track hover over the
/// regions the parent painted.
///
/// Regions outside the viewport can be skipped by the caller before
/// calling this: only what is on screen needs a child.
pub fn regions<M, Theme, Renderer>(
    tokens: &Tokens,
    regions: Vec<(Rectangle, String)>,
    children: &mut Vec<Element<'static, M, Theme, Renderer>>,
    tree: &mut Tree,
    renderer: &Renderer,
    size: Size,
) -> layout::Node
where
    Theme: container::Catalog + text::Catalog + 'static,
    for<'a> <Theme as container::Catalog>::Class<'a>: From<container::StyleFn<'a, Theme>>,
    Renderer: iced_core::text::Renderer + 'static,
    M: 'static,
{
    *children = regions
        .iter()
        .map(|(bounds, label)| {
            tip(
                tokens,
                Space::new().width(bounds.width).height(bounds.height),
                label.clone(),
            )
        })
        .collect();
    tree.diff_children(children.as_mut_slice());
    let nodes = children
        .iter_mut()
        .zip(&mut tree.children)
        .zip(regions)
        .map(|((child, state), (bounds, _))| {
            child
                .as_widget_mut()
                .layout(
                    state,
                    renderer,
                    &layout::Limits::new(Size::ZERO, bounds.size()),
                )
                .move_to(bounds.position())
        })
        .collect();
    layout::Node::with_children(size, nodes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_renderer::LayoutRenderer;
    use iced_core::layout::Layout;

    #[test]
    fn regions_lay_out_at_their_bounds_in_parent_order() {
        let tokens = Tokens::dark();
        let mut children: Vec<Element<'static, (), iced_core::Theme, LayoutRenderer>> = Vec::new();
        let mut tree = Tree::empty();
        let renderer = LayoutRenderer::new();
        let hits = vec![
            (
                Rectangle { x: 8.0, y: 8.0, width: 40.0, height: 12.0 },
                "first".to_owned(),
            ),
            (
                Rectangle { x: 8.0, y: 24.0, width: 16.0, height: 12.0 },
                "second".to_owned(),
            ),
        ];
        let node = regions(
            &tokens,
            hits,
            &mut children,
            &mut tree,
            &renderer,
            Size::new(64.0, 64.0),
        );
        let laid: Vec<_> = Layout::new(&node).children().collect();
        let first = laid[0].bounds();
        let second = laid[1].bounds();
        assert_eq!(
            (first.x, first.y, first.width, first.height),
            (8.0, 8.0, 40.0, 12.0)
        );
        assert_eq!(
            (second.x, second.y, second.width, second.height),
            (8.0, 24.0, 16.0, 12.0)
        );
        assert_eq!(node.bounds().size(), Size::new(64.0, 64.0));
    }
}

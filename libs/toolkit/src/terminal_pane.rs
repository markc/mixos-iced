// SPDX-License-Identifier: MIT OR Apache-2.0
//! A terminal viewport with a clipped painter surface, invariant focus border
//! and wheel actions. The caller supplies its renderer element and terminal
//! engine; this module has no PTY, raster storage or service dependency.

use iced_core::{Element, Length, Point, Rectangle, Size, mouse};
use iced_widget::{container, mouse_area};

use crate::Tokens;

/// Logical cell geometry shared by pointer hit-testing and IME placement.
/// Dimensions describe the terminal grid, including cells clipped by a narrow
/// pane. Border and spare pixels clamp to the nearest cell.
#[derive(Debug, Clone, Copy)]
pub struct GridGeometry {
    pub cell: Size,
    pub columns: u16,
    pub rows: u16,
    pub border: f32,
}

impl GridGeometry {
    fn valid(self) -> bool {
        self.columns > 0
            && self.rows > 0
            && self.cell.width.is_finite()
            && self.cell.width > 0.0
            && self.cell.height.is_finite()
            && self.cell.height > 0.0
            && self.border.is_finite()
            && self.border >= 0.0
    }

    pub fn cell_at(self, point: Point) -> Option<(u16, u16)> {
        if !self.valid() || !point.x.is_finite() || !point.y.is_finite() {
            return None;
        }
        let index = |position: f32, size: f32, count: u16| {
            (((position - self.border).max(0.0) / size) as u16).min(count - 1)
        };
        Some((
            index(point.x, self.cell.width, self.columns),
            index(point.y, self.cell.height, self.rows),
        ))
    }

    /// Cursor rectangle in window coordinates for the runtime IME overlay.
    pub fn cursor_rect(self, origin: Point, cursor: (u16, u16)) -> Option<Rectangle> {
        if !self.valid() || !origin.x.is_finite() || !origin.y.is_finite() {
            return None;
        }
        Some(Rectangle::new(
            Point::new(
                origin.x
                    + self.border
                    + f32::from(cursor.0.min(self.columns - 1)) * self.cell.width,
                origin.y + self.border + f32::from(cursor.1.min(self.rows - 1)) * self.cell.height,
            ),
            self.cell,
        ))
    }
}

/// A renderer-independent terminal pane. The border is padding rather than an
/// overlay, so changing focus never changes the available grid dimensions.
pub struct TerminalPane<'a, Message, Theme, Renderer> {
    surface: Element<'a, Message, Theme, Renderer>,
    size: Size,
    border: f32,
    focus_ring: bool,
    tokens: Tokens,
    scroll: Option<Box<dyn Fn(mouse::ScrollDelta) -> Message + 'a>>,
}

impl<'a, Message, Theme, Renderer> TerminalPane<'a, Message, Theme, Renderer> {
    pub fn new(
        surface: impl Into<Element<'a, Message, Theme, Renderer>>,
        size: Size,
        border: f32,
        tokens: Tokens,
    ) -> Self {
        assert!(
            size.width.is_finite()
                && size.width >= 0.0
                && size.height.is_finite()
                && size.height >= 0.0
        );
        assert!(border.is_finite() && border >= 0.0);
        Self {
            surface: surface.into(),
            size,
            border,
            focus_ring: false,
            tokens,
            scroll: None,
        }
    }

    pub fn focus_ring(mut self, visible: bool) -> Self {
        self.focus_ring = visible;
        self
    }

    /// Emit a wheel action for this pane. Scrollback and alternate-screen
    /// policy belong to the engine that consumes the action.
    pub fn on_scroll(mut self, action: impl Fn(mouse::ScrollDelta) -> Message + 'a) -> Self {
        self.scroll = Some(Box::new(action));
        self
    }
}

impl<'a, Message, Theme, Renderer> From<TerminalPane<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: Clone + 'a,
    Theme: container::Catalog + 'a,
    <Theme as container::Catalog>::Class<'a>: From<container::StyleFn<'a, Theme>>,
    Renderer: iced_core::Renderer + 'a,
{
    fn from(pane: TerminalPane<'a, Message, Theme, Renderer>) -> Self {
        let palette = pane.tokens.palette;
        let frame = if pane.focus_ring {
            palette.ring
        } else {
            palette.border
        };
        let inner = container(pane.surface)
            .width(Length::Fill)
            .height(Length::Fill)
            .clip(true)
            .style(move |_| container::Style {
                background: Some(palette.surface.into()),
                ..Default::default()
            });
        let outer = container(inner)
            .padding(pane.border)
            .width(Length::Fixed(pane.size.width))
            .height(Length::Fixed(pane.size.height))
            .style(move |_| container::Style {
                background: Some(frame.into()),
                ..Default::default()
            });
        match pane.scroll {
            Some(action) => mouse_area(outer).on_scroll(action).into(),
            None => outer.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_surface_is_clipped_focus_keeps_layout_and_wheel_is_delivered() {
        use crate::test_renderer::LayoutRenderer;
        use iced_core::{Event, Layout, Shell, layout::Limits, renderer, widget::Tree};
        #[derive(Clone, Debug)]
        enum Action {
            Wheel(mouse::ScrollDelta),
        }
        let tokens = Tokens::default();
        let mut sizes = Vec::new();
        for focused in [false, true] {
            let mut renderer = LayoutRenderer::new();
            let surface = container(iced_widget::space()).width(400).height(300);
            let mut element: Element<'_, Action, crate::Theme, LayoutRenderer> =
                TerminalPane::new(surface, Size::new(100.0, 80.0), 1.2, tokens)
                    .focus_ring(focused)
                    .on_scroll(Action::Wheel)
                    .into();
            let mut tree = Tree::new(&element);
            let node = element.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &Limits::new(Size::ZERO, Size::new(800.0, 600.0)),
            );
            sizes.push(node.size());
            let viewport = Rectangle::with_size(Size::new(800.0, 600.0));
            let cursor = mouse::Cursor::Available(Point::new(20.0, 20.0));
            let mut messages = Vec::new();
            element.as_widget_mut().update(
                &mut tree,
                &Event::Mouse(mouse::Event::WheelScrolled {
                    delta: mouse::ScrollDelta::Lines { x: 0.0, y: -3.0 },
                }),
                Layout::new(&node),
                cursor,
                &renderer,
                &mut Shell::new(&mut messages),
                &viewport,
            );
            assert!(matches!(
                messages.as_slice(),
                [Action::Wheel(mouse::ScrollDelta::Lines { x: 0.0, y: -3.0 })]
            ));
            element.as_widget().draw(
                &tree,
                &mut renderer,
                &crate::Theme::new(tokens),
                &renderer::Style::default(),
                Layout::new(&node),
                cursor,
                &viewport,
            );
            let colour = if focused {
                tokens.palette.ring
            } else {
                tokens.palette.border
            };
            assert!(
                renderer
                    .quads
                    .iter()
                    .any(|(_, background)| *background == colour.into())
            );
            assert!(
                renderer
                    .layers
                    .iter()
                    .any(|clip| clip.width <= 97.6 && clip.height <= 77.6)
            );
        }
        assert_eq!(sizes, [Size::new(100.0, 80.0); 2]);
    }

    #[test]
    fn fractional_cells_clamp_border_and_spare_pixels() {
        let grid = GridGeometry {
            cell: Size::new(7.5, 15.2),
            columns: 80,
            rows: 24,
            border: 1.2,
        };
        assert_eq!(grid.cell_at(Point::ORIGIN), Some((0, 0)));
        assert_eq!(grid.cell_at(Point::new(25.3, 33.3)), Some((3, 2)));
        assert_eq!(grid.cell_at(Point::new(900.0, 500.0)), Some((79, 23)));
        let cursor = grid
            .cursor_rect(Point::new(40.0, 60.0), (3, 2))
            .unwrap()
            .position();
        assert!((cursor.x - 63.7).abs() < 0.001 && (cursor.y - 91.6).abs() < 0.001);
    }

    #[test]
    fn malformed_or_empty_grids_have_no_cell_or_cursor() {
        let mut grid = GridGeometry {
            cell: Size::new(8.0, 16.0),
            columns: 0,
            rows: 24,
            border: 1.0,
        };
        assert!(grid.cell_at(Point::ORIGIN).is_none());
        assert!(grid.cursor_rect(Point::ORIGIN, (0, 0)).is_none());
        grid.columns = 80;
        grid.cell.width = f32::NAN;
        assert!(grid.cell_at(Point::ORIGIN).is_none());
        grid.cell.width = 8.0;
        assert!(grid.cell_at(Point::new(f32::INFINITY, 0.0)).is_none());
    }
}

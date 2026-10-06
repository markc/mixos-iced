// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native CPU grid bands, retained without conversion to ordinary image handles.
use crate::cpu::grid::Handle;
use iced::advanced::{Layout, Widget, layout, mouse, renderer, widget::Tree};
use iced::{Length, Rectangle, Size};

pub struct Grid {
    images: Vec<(Handle, Rectangle)>,
    scale: f32,
    width: Length,
    height: Length,
}

impl Grid {
    pub fn new(images: Vec<(Handle, Rectangle)>, scale: f32) -> Self {
        assert!(scale.is_finite() && scale > 0.0);
        Self { images, scale, width: Length::Shrink, height: Length::Shrink }
    }

    pub fn width(mut self, width: Length) -> Self {
        self.width = width;
        self
    }
    pub fn height(mut self, height: Length) -> Self {
        self.height = height;
        self
    }
}

/// Shared by the widget and benchmark. Bounds are physical extents divided
/// by output scale, never by available layout space. Snap only the origin;
/// deriving every band's origin from integer pixels prevents fractional seams.
pub fn draw_images(
    renderer: &mut crate::cpu::Renderer,
    images: &[(Handle, Rectangle)],
    origin: iced::Point,
    scale: f32,
    clip: Rectangle,
) {
    let x = (origin.x * scale).round() / scale;
    let y = (origin.y * scale).round() / scale;
    for (handle, relative) in images {
        // Subtracting logical edges loses precision for lower bands. Use the
        // grid's integer pixel dimensions so every band stays native-sized.
        let bounds = Rectangle {
            x: relative.x + x,
            y: relative.y + y,
            width: handle.width() as f32 / scale,
            height: handle.height() as f32 / scale,
        };
        if bounds.intersects(&clip) {
            renderer.draw_grid(handle.clone(), bounds, clip);
        }
    }
}

impl<Message, Theme> Widget<Message, Theme, crate::cpu::Renderer> for Grid {
    fn size(&self) -> Size<Length> {
        Size::new(self.width, self.height)
    }
    fn layout(
        &mut self,
        _: &mut Tree,
        _: &crate::cpu::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        layout::Node::new(limits.resolve(self.width, self.height, Size::ZERO))
    }
    fn draw(
        &self,
        _: &Tree,
        renderer: &mut crate::cpu::Renderer,
        _: &Theme,
        _: &renderer::Style,
        layout: Layout<'_>,
        _: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        if let Some(clip) = layout.bounds().intersection(viewport) {
            draw_images(renderer, &self.images, layout.position(), self.scale, clip);
        }
    }
}

impl<'a, Message: 'a, Theme: 'a> From<Grid>
    for iced::Element<'a, Message, Theme, crate::cpu::Renderer>
{
    fn from(grid: Grid) -> Self {
        Self::new(grid)
    }
}

// SPDX-License-Identifier: MIT OR Apache-2.0
//! Raster and guides need separate renderer layers: tiny-skia batches primitive
//! geometry before images within one layer, even across multiple geometries.
use crate::{document::Point, viewport::Viewport};
use iced::advanced::{
    Layout, Shell, Widget, image, layout, renderer,
    widget::{Tree, tree},
};
use iced::advanced::{Renderer as _, image::Renderer as _};
use iced::{Element, Event, Length, Rectangle, Size, mouse};
use toolkit::Theme;
pub fn plane<'a, Message: 'a>(
    content: Element<'a, Message, Theme>,
    image: &'a image::Handle,
    dimensions: (u32, u32),
    zoom: f32,
    pan: Point,
) -> Element<'a, Message, Theme> {
    Element::new(Plane {
        content,
        image,
        dimensions,
        zoom,
        pan,
    })
}
struct Plane<'a, Message> {
    content: Element<'a, Message, Theme>,
    image: &'a image::Handle,
    dimensions: (u32, u32),
    zoom: f32,
    pan: Point,
}
impl<Message> Widget<Message, Theme, iced::Renderer> for Plane<'_, Message> {
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }
    fn tag(&self) -> tree::Tag {
        self.content.as_widget().tag()
    }
    fn state(&self) -> tree::State {
        self.content.as_widget().state()
    }
    fn diff(&mut self, tree: &mut Tree) {
        self.content.as_widget_mut().diff(tree)
    }
    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &iced::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content.as_widget_mut().layout(tree, renderer, limits)
    }
    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &iced::Renderer,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        self.content
            .as_widget_mut()
            .update(tree, event, layout, cursor, renderer, shell, viewport)
    }
    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut iced::Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let view = Viewport::fit(
            self.dimensions,
            (bounds.width, bounds.height),
            self.zoom,
            self.pan,
        );
        let Some(clip) = bounds.intersection(viewport) else {
            return;
        };
        renderer.with_layer(clip, |renderer| {
            renderer.draw_image(
                image::Image::new(self.image),
                Rectangle {
                    x: bounds.x + view.origin.x,
                    y: bounds.y + view.origin.y,
                    width: view.width,
                    height: view.height,
                },
                clip,
            )
        });
        renderer.with_layer(clip, |renderer| {
            self.content
                .as_widget()
                .draw(tree, renderer, theme, style, layout, cursor, viewport)
        });
    }
    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &iced::Renderer,
    ) -> mouse::Interaction {
        self.content
            .as_widget()
            .mouse_interaction(tree, layout, cursor, viewport, renderer)
    }
}

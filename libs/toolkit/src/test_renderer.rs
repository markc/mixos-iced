// SPDX-License-Identifier: MIT OR Apache-2.0
//! Measurement-only renderer: real iced text shaping, no device or painting.

use std::sync::Once;

use iced_core::{Background, Color, Font, Pixels, Point, Rectangle, Transformation};
use iced_core::{image, renderer, text};
use iced_core::text::Paragraph as _;

pub(crate) struct LayoutRenderer {
    pub(crate) paragraphs: Vec<Rectangle>,
}

impl LayoutRenderer {
    pub(crate) fn new() -> Self {
        static FONT: Once = Once::new();
        FONT.call_once(|| {
            // The test build enables iced_graphics `fira-sans`, which embeds
            // Fira Sans and binds the generic sans-serif family to it. Widget
            // text uses Font::DEFAULT or MONOSPACE, so bind monospace to the
            // same face: no host fonts are needed for text bounds, cursor
            // positioning or typing.
            let mut system = iced_graphics::text::font_system().write().unwrap();
            system.raw().db_mut().set_monospace_family("Fira Sans");
        });
        Self { paragraphs: Vec::new() }
    }
}

impl iced_core::Renderer for LayoutRenderer {
    fn start_layer(&mut self, _bounds: Rectangle) {}
    fn end_layer(&mut self) {}
    fn start_transformation(&mut self, _transformation: Transformation) {}
    fn end_transformation(&mut self) {}
    fn fill_quad(&mut self, _quad: renderer::Quad, _background: impl Into<Background>) {}

    fn allocate_image(
        &mut self,
        _handle: &image::Handle,
        callback: impl FnOnce(Result<image::Allocation, image::Error>) + Send + 'static,
    ) {
        callback(Err(image::Error::Unsupported));
    }

    fn hint(&mut self, _scale: renderer::Scale) {}
    fn scale(&self) -> Option<renderer::Scale> {
        None
    }
    fn reset(&mut self, _new_bounds: Rectangle) {}
    fn settings(&self) -> renderer::Settings {
        renderer::Settings::default()
    }
}

impl text::Renderer for LayoutRenderer {
    type Font = Font;
    type Paragraph = iced_graphics::text::Paragraph;
    type Editor = iced_graphics::text::Editor;

    const ICON_FONT: Font = Font::new("Iced-Icons");
    const CHECKMARK_ICON: char = '\u{f00c}';
    const ARROW_DOWN_ICON: char = '\u{e800}';
    const ICED_LOGO: char = '\u{e801}';
    const SCROLL_UP_ICON: char = '\u{e802}';
    const SCROLL_DOWN_ICON: char = '\u{e803}';
    const SCROLL_LEFT_ICON: char = '\u{e804}';
    const SCROLL_RIGHT_ICON: char = '\u{e805}';

    fn default_font(&self) -> Font {
        Font::DEFAULT
    }
    fn default_size(&self) -> Pixels {
        Pixels(16.0)
    }

    fn fill_paragraph(
        &mut self,
        paragraph: &Self::Paragraph,
        position: Point,
        _color: Color,
        _clip_bounds: Rectangle,
    ) {
        self.paragraphs.push(Rectangle::new(position, paragraph.min_bounds()));
    }

    fn fill_editor(
        &mut self,
        _editor: &Self::Editor,
        _position: Point,
        _color: Color,
        _clip_bounds: Rectangle,
    ) {
    }

    fn fill_text(
        &mut self,
        _text: text::Text,
        _position: Point,
        _color: Color,
        _clip_bounds: Rectangle,
    ) {
    }
}

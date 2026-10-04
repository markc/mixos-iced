// SPDX-License-Identifier: MIT OR Apache-2.0
//! Measurement-only renderer: real iced text shaping, no device or painting.

use std::borrow::Cow;
use std::sync::Once;

use iced_core::{Background, Color, Font, Pixels, Point, Rectangle, Transformation};
use iced_core::{image, renderer, text};

pub(crate) struct LayoutRenderer;

impl LayoutRenderer {
    pub(crate) fn new() -> Self {
        static FONT: Once = Once::new();
        FONT.call_once(|| {
            let mut system = iced_graphics::text::font_system().write().unwrap();
            system.load_font(Cow::Borrowed(include_bytes!(
                "../../../vendor/font/Inter-VariableFont_opsz,wght.ttf"
            )));
            // Scene text explicitly uses Font::DEFAULT (or MONOSPACE), so
            // bind both generic families to the bundled face. No host fonts
            // are needed for text bounds, cursor positioning or typing.
            system.raw().db_mut().set_sans_serif_family("Inter");
            system.raw().db_mut().set_monospace_family("Inter");
        });
        Self
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
        _paragraph: &Self::Paragraph,
        _position: Point,
        _color: Color,
        _clip_bounds: Rectangle,
    ) {
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

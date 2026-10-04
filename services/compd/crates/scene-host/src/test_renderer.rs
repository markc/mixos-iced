//! Measurement-only renderer: real iced text shaping, no device or painting.

use std::borrow::Cow;
use std::sync::Once;

use iced_core::{Background, Color, Font, Pixels, Point, Rectangle, Size, Transformation};
use iced_core::{image, renderer, svg, text};

pub(crate) struct LayoutRenderer;

impl LayoutRenderer {
    pub(crate) fn new() -> Self {
        static FONT: Once = Once::new();
        FONT.call_once(|| {
            let mut system = iced_graphics::text::font_system().write().unwrap();
            system.load_font(Cow::Borrowed(include_bytes!(
                "../../../../../vendor/font/Inter-VariableFont_opsz,wght.ttf"
            )));
            // Scene text uses the sans-serif family (ui::font::BODY) or
            // MONOSPACE, so bind both generic families to the bundled face.
            // No host fonts and no asset set are needed for text bounds,
            // cursor positioning or typing: this renderer stays hermetic.
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
        ui::font::BODY
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

impl image::Renderer for LayoutRenderer {
    type Handle = image::Handle;

    fn load_image(&self, _handle: &Self::Handle) -> Result<image::Allocation, image::Error> {
        Err(image::Error::Unsupported)
    }

    fn measure_image(&self, handle: &Self::Handle) -> Option<Size<u32>> {
        iced_graphics::image::load(handle)
            .ok()
            .map(|pixels| Size::new(pixels.width(), pixels.height()))
    }

    fn draw_image(&mut self, _image: image::Image, _bounds: Rectangle, _clip_bounds: Rectangle) {}
}

impl svg::Renderer for LayoutRenderer {
    fn measure_svg(&self, handle: &svg::Handle) -> Size<u32> {
        let bytes = match handle.data() {
            svg::Data::Path(path) => match std::fs::read(path) {
                Ok(bytes) => Cow::Owned(bytes),
                Err(_) => return Size::new(1, 1),
            },
            svg::Data::Bytes(bytes) => Cow::Borrowed(bytes.as_ref()),
        };
        resvg::usvg::Tree::from_data(&bytes, &resvg::usvg::Options::default())
            .map(|tree| {
                let size = tree.size();
                Size::new(size.width() as u32, size.height() as u32)
            })
            .unwrap_or(Size::new(1, 1))
    }

    fn draw_svg(&mut self, _svg: svg::Svg, _bounds: Rectangle, _clip_bounds: Rectangle) {}
}

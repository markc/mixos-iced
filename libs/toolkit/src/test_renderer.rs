// SPDX-License-Identifier: MIT OR Apache-2.0
//! Measurement-only renderer: real iced text shaping, no device or painting.

use std::sync::Once;

use iced_core::text::Paragraph as _;
use iced_core::{Background, Color, Font, Pixels, Point, Rectangle, Transformation};
use iced_core::{image, renderer, text};

pub(crate) struct LayoutRenderer {
    pub(crate) paragraphs: Vec<Rectangle>,
    pub(crate) paragraph_colours: Vec<Color>,
    pub(crate) quads: Vec<(Rectangle, Background)>,
    pub(crate) layers: Vec<Rectangle>,
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
        Self {
            paragraphs: Vec::new(),
            paragraph_colours: Vec::new(),
            quads: Vec::new(),
            layers: Vec::new(),
        }
    }
}

impl iced_core::Renderer for LayoutRenderer {
    fn start_layer(&mut self, bounds: Rectangle) {
        self.layers.push(bounds);
    }
    fn end_layer(&mut self) {}
    fn start_transformation(&mut self, _transformation: Transformation) {}
    fn end_transformation(&mut self) {}
    fn fill_quad(&mut self, quad: renderer::Quad, background: impl Into<Background>) {
        self.quads.push((quad.bounds, background.into()));
    }

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
        color: Color,
        _clip_bounds: Rectangle,
    ) {
        self.paragraphs
            .push(Rectangle::new(position, paragraph.min_bounds()));
        self.paragraph_colours.push(color);
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

/// A text renderer whose font is not iced's `Font`, with a real matching
/// paragraph and editor. `Editor = ()` cannot prove a custom-font
/// renderer, so both are supplied: widgets that edit text (like the
/// requester's field) lay out end to end with it. The measured width of a
/// label depends on the face, so assertions can tell the supplied face
/// from the renderer's default one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Face(pub(crate) u8);

pub(crate) struct FaceParagraph {
    font: Face,
    size: Pixels,
    line_height: text::LineHeight,
    content: String,
}

impl Default for FaceParagraph {
    fn default() -> Self {
        Self {
            font: Face(0),
            size: Pixels(0.0),
            line_height: text::LineHeight::default(),
            content: String::new(),
        }
    }
}

impl text::Paragraph for FaceParagraph {
    type Font = Face;

    fn with_text(text: text::Text<&str, Face>) -> Self {
        Self {
            font: text.font,
            size: text.size,
            line_height: text.line_height,
            content: text.content.to_owned(),
        }
    }

    fn with_spans<Link>(_text: text::Text<&[text::Span<'_, Link, Face>], Face>) -> Self {
        Self::default()
    }

    fn resize(&mut self, _new_bounds: Size) {}

    fn compare(&self, _text: text::Text<(), Face>) -> text::Difference {
        text::Difference::None
    }

    fn size(&self) -> Pixels {
        self.size
    }

    fn hint_factor(&self) -> Option<f32> {
        None
    }

    fn font(&self) -> Face {
        self.font
    }

    fn line_height(&self) -> text::LineHeight {
        self.line_height
    }

    fn align_x(&self) -> text::Alignment {
        text::Alignment::Left
    }

    fn align_y(&self) -> iced_core::alignment::Vertical {
        iced_core::alignment::Vertical::Top
    }

    fn wrapping(&self) -> text::Wrapping {
        text::Wrapping::None
    }

    fn ellipsis(&self) -> text::Ellipsis {
        text::Ellipsis::None
    }

    fn shaping(&self) -> text::Shaping {
        text::Shaping::Advanced
    }

    fn bounds(&self) -> Size {
        Size::INFINITY
    }

    fn min_bounds(&self) -> Size {
        // The face changes the measured width, so the measurement and the
        // drawing provably use the same supplied font.
        Size::new(
            self.content.len() as f32 * 6.0 + self.font.0 as f32 * 100.0,
            self.size.0,
        )
    }

    fn hit_test(&self, _point: Point) -> Option<text::Hit> {
        None
    }

    fn hit_span(&self, _point: Point) -> Option<usize> {
        None
    }

    fn span_bounds(&self, _index: usize) -> Vec<Rectangle> {
        Vec::new()
    }
}

/// A minimal `Face` editor, so widgets that edit text work with the
/// non-`Font` renderer.
#[derive(Default)]
pub(crate) struct FaceEditor {
    text: String,
    font: Face,
    size: Pixels,
    line_height: text::LineHeight,
}

impl text::Editor for FaceEditor {
    type Font = Face;

    fn with_text(text: &str) -> Self {
        Self {
            text: text.to_owned(),
            ..Self::default()
        }
    }

    fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    fn cursor(&self) -> text::editor::Cursor {
        text::editor::Cursor {
            position: text::Position { line: 0, index: 0 },
            selection: None,
        }
    }

    fn selection(&self) -> text::editor::Selection {
        text::editor::Selection::Caret(Point::ORIGIN)
    }

    fn copy(&self) -> Option<String> {
        None
    }

    fn line(&self, index: usize) -> Option<text::editor::Line<'_>> {
        (index == 0).then(|| text::editor::Line {
            text: self.text.as_str().into(),
            ending: text::editor::LineEnding::None,
        })
    }

    fn line_count(&self) -> usize {
        usize::from(!self.text.is_empty())
    }

    fn perform(&mut self, action: text::editor::Action) {
        let text::editor::Action::Edit(text::editor::Edit::Insert(c)) = action else {
            return;
        };
        self.text.push(c);
    }

    fn move_to(&mut self, _cursor: text::editor::Cursor) {}

    fn bounds(&self) -> Size {
        Size::ZERO
    }

    fn min_bounds(&self) -> Size {
        Size::ZERO
    }

    fn hint_factor(&self) -> Option<f32> {
        None
    }

    fn update(
        &mut self,
        _new_bounds: Size,
        new_font: Self::Font,
        new_size: Pixels,
        new_line_height: text::LineHeight,
        _new_wrapping: text::Wrapping,
        _new_alignment: text::Alignment,
        _new_hint_factor: Option<f32>,
        _new_highlighter: &mut impl text::highlighter::Highlighter,
    ) {
        self.font = new_font;
        self.size = new_size;
        self.line_height = new_line_height;
    }

    fn overwrite(&mut self, new_text: &str) {
        self.text.clear();
        self.text.push_str(new_text);
    }

    fn highlight<H: text::highlighter::Highlighter>(
        &mut self,
        _font: Self::Font,
        _highlighter: &mut H,
        _format_highlight: impl Fn(&H::Highlight) -> text::highlighter::Format<Self::Font>,
    ) {
    }

    fn font(&self) -> Self::Font {
        self.font
    }

    fn text_size(&self) -> Pixels {
        self.size
    }

    fn line_height(&self) -> text::LineHeight {
        self.line_height
    }
}

/// Records the resolved styles of drawn paragraphs and editors for the
/// non-`Font` renderer.
#[derive(Default)]
pub(crate) struct FaceRenderer {
    pub(crate) paragraphs: Vec<(Face, Pixels, text::LineHeight)>,
    pub(crate) editors: Vec<(Face, Pixels)>,
}

impl iced_core::Renderer for FaceRenderer {
    fn start_layer(&mut self, _bounds: Rectangle) {}
    fn end_layer(&mut self) {}
    fn start_transformation(&mut self, _transformation: Transformation) {}
    fn end_transformation(&mut self) {}
    fn hint(&mut self, _scale: renderer::Scale) {}
    fn scale(&self) -> Option<renderer::Scale> {
        None
    }
    fn reset(&mut self, _: Rectangle) {}
    fn settings(&self) -> renderer::Settings {
        renderer::Settings::default()
    }
    fn fill_quad(&mut self, _quad: renderer::Quad, _background: impl Into<Background>) {}
    fn allocate_image(
        &mut self,
        handle: &image::Handle,
        callback: impl FnOnce(Result<image::Allocation, image::Error>) + Send + 'static,
    ) {
        let _ = handle;
        callback(Err(iced_core::image::Error::Unsupported));
    }
}

impl text::Renderer for FaceRenderer {
    type Font = Face;
    type Paragraph = FaceParagraph;
    type Editor = FaceEditor;

    const ICON_FONT: Face = Face(0);
    const CHECKMARK_ICON: char = 'x';
    const ARROW_DOWN_ICON: char = 'v';
    const SCROLL_UP_ICON: char = '^';
    const SCROLL_DOWN_ICON: char = 'v';
    const SCROLL_LEFT_ICON: char = '<';
    const SCROLL_RIGHT_ICON: char = '>';
    const ICED_LOGO: char = 'i';

    fn default_font(&self) -> Face {
        Face(0)
    }
    fn default_size(&self) -> Pixels {
        Pixels(14.0)
    }
    fn fill_paragraph(&mut self, paragraph: &FaceParagraph, _: Point, _: Color, _: Rectangle) {
        self.paragraphs
            .push((paragraph.font(), paragraph.size(), paragraph.line_height()));
    }
    fn fill_editor(&mut self, editor: &FaceEditor, _: Point, _: Color, _: Rectangle) {
        self.editors.push((editor.font(), editor.text_size()));
    }
    fn fill_text(&mut self, _text: text::Text<String, Face>, _: Point, _: Color, _: Rectangle) {}
}

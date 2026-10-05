// SPDX-License-Identifier: MIT OR Apache-2.0
//! Filename middle elision: [`middle`] resolves the widest prefix a string
//! can keep, and [`Label`] is a fill-width, clipped label that elides in
//! the middle — keeping the extension — instead of the end.
//!
//! End elision (iced's `Ellipsis::End`, CSS `text-overflow: ellipsis`)
//! loses exactly the part of a filename that identifies it: the
//! extension. Middle elision keeps both ends, preferring the extension
//! when one exists (a dot in a parent directory is not an extension) and
//! re-solving during layout, so resizes and font changes can never show a
//! stale cut.
//!
//! Width is always measured by shaping the candidate with the actual
//! renderer's font — never character estimates — and the search is
//! bounded: at most 32 shape-and-measure calls per resolve, so a resize
//! storm stays cheap.

use iced_core::Length;
use iced_core::alignment;
use iced_core::layout::{self, Layout};
use iced_core::mouse;
use iced_core::renderer;
use iced_core::text::paragraph::Paragraph;
use iced_core::text::{self, Shaping, Wrapping};
use iced_core::widget::text::{Catalog, Style};
use iced_core::widget::tree::{self, Tree};
use iced_core::{Color, Element, Pixels, Rectangle, Size, Widget};
use unicode_segmentation::UnicodeSegmentation;

/// Shape `content` as one unwrapped line for measurement.
fn shape<P: Paragraph>(content: &str, font: P::Font, px: impl Into<Pixels>) -> P
where
    P::Font: Copy,
{
    P::with_text(text::Text {
        content,
        bounds: Size::INFINITE,
        size: px.into(),
        line_height: text::LineHeight::default(),
        font,
        align_x: text::Alignment::Left,
        align_y: alignment::Vertical::Top,
        shaping: Shaping::Advanced,
        wrapping: Wrapping::None,
        ellipsis: text::Ellipsis::None,
        hint_factor: None,
    })
}

/// The widest middle-elided form of `text` that fits `width`, measured by
/// `measure` (shape the candidate and report its width; see [`Label`],
/// which does this with the renderer's font). An extension survives when
/// one exists; grapheme boundaries are respected; the search stops after
/// 32 measurements however long the name.
pub fn middle(text: &str, width: f32, mut measure: impl FnMut(&str) -> f32) -> String {
    if text.is_empty() || !width.is_finite() || width <= 0.0 {
        return String::new();
    }
    if measure(text) <= width {
        return text.to_owned();
    }
    if measure("…") > width {
        return String::new();
    }
    let boundaries: Vec<_> = text
        .grapheme_indices(true)
        .map(|(i, _)| i)
        .chain(std::iter::once(text.len()))
        .collect();
    let count = boundaries.len() - 1;
    if count <= 1 {
        return "…".into();
    }
    let extension = text
        .rfind('.')
        .filter(|i| *i > 0 && *i + 1 < text.len())
        .filter(|i| text.rfind('/').is_none_or(|slash| *i > slash + 1))
        .and_then(|i| boundaries.iter().position(|b| *b == i));
    let stem = extension.unwrap_or(count - 1);
    let suffix = extension.map_or(1, |i| count - i);
    let mut measures = 2;
    for keep in (1..=suffix.min(12)).rev() {
        let (mut low, mut high, mut best) = (0, stem, None);
        while low <= high {
            if measures >= 32 {
                return "…".into();
            }
            measures += 1;
            let prefix = low + (high - low) / 2;
            let candidate = if suffix > 12 {
                format!(
                    "{}….{}",
                    &text[..boundaries[prefix]],
                    &text[boundaries[count - keep + 1]..]
                )
            } else {
                format!(
                    "{}…{}",
                    &text[..boundaries[prefix]],
                    &text[boundaries[count - keep]..]
                )
            };
            if measure(&candidate) <= width {
                best = Some(candidate);
                low = prefix + 1;
            } else if prefix == 0 {
                break;
            } else {
                high = prefix - 1;
            }
        }
        if let Some(best) = best {
            return best;
        }
    }
    "…".into()
}

/// A fill-width label that middle-elides its text to fit, extension kept.
///
/// Elision is resolved during layout, so resize and typography changes
/// cannot go stale. The colour comes from the text theme unless
/// [`Label::color`] overrides it.
///
/// ```no_run
/// use toolkit::elide::Label;
///
/// fn view<'a, Theme, Renderer>(theme: &Theme) -> iced_core::Element<'a, (), Theme, Renderer>
/// where
///     Theme: iced_core::widget::text::Catalog + 'a,
///     Renderer: iced_core::text::Renderer + 'a,
/// {
///     Label::new("a-very-long-filename.tar.gz").size(14.0).into()
/// }
/// ```
#[must_use]
pub struct Label<'a, Theme, Renderer>
where
    Theme: Catalog,
    Renderer: text::Renderer,
{
    /// The full, unelided text.
    pub text: String,
    /// The font the label shapes with.
    pub font: Renderer::Font,
    /// The font size in px.
    pub px: Pixels,
    color: Option<Color>,
    class: Theme::Class<'a>,
}

impl<'a, Theme, Renderer> Label<'a, Theme, Renderer>
where
    Theme: Catalog,
    Renderer: text::Renderer,
{
    /// A label showing `text`, elided in the middle when it does not fit.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            font: Renderer::default_font(),
            px: Pixels(16.0),
            color: None,
            class: Theme::default(),
        }
    }

    /// Sets the font.
    pub fn font(mut self, font: impl Into<Renderer::Font>) -> Self {
        self.font = font.into();
        self
    }

    /// Sets the font size in px.
    pub fn size(mut self, px: impl Into<Pixels>) -> Self {
        self.px = px.into();
        self
    }

    /// Overrides the text colour (the theme's text colour by default).
    pub fn color(mut self, color: impl Into<Color>) -> Self {
        self.color = Some(color.into());
        self
    }

    /// Sets the style class of the underlying text.
    pub fn class(mut self, class: impl Into<Theme::Class<'a>>) -> Self {
        self.class = class.into();
        self
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer> for Label<'_, Theme, Renderer>
where
    Theme: Catalog,
    Renderer: text::Renderer,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Renderer::Paragraph>()
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Shrink)
    }

    fn state(&self) -> tree::State {
        tree::State::new(shape::<Renderer::Paragraph>("", self.font, self.px))
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        _renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let width = limits.max().width;
        let value = middle(&self.text, width, |s| {
            shape::<Renderer::Paragraph>(s, self.font, self.px)
                .min_bounds()
                .width
        });
        let paragraph = shape::<Renderer::Paragraph>(&value, self.font, self.px);
        let height =
            shape::<Renderer::Paragraph>("Ag", self.font, self.px).min_bounds().height;
        *tree.state.downcast_mut::<Renderer::Paragraph>() = paragraph;
        layout::Node::new(limits.resolve(
            Length::Fill,
            Length::Shrink,
            Size::new(width, height),
        ))
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        defaults: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let Style { color } = theme.style(&self.class);
        let color = self.color.or(color).unwrap_or(defaults.text_color);
        if let Some(clip) = layout.bounds().intersection(viewport) {
            renderer.with_layer(clip, |renderer| {
                renderer.fill_paragraph(
                    tree.state.downcast_ref::<Renderer::Paragraph>(),
                    layout.bounds().position(),
                    color,
                    clip,
                )
            });
        }
    }
}

impl<'a, Message, Theme, Renderer> From<Label<'a, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Theme: Catalog + 'a,
    Renderer: text::Renderer + 'a,
{
    fn from(label: Label<'a, Theme, Renderer>) -> Self {
        Element::new(label)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn width(s: &str) -> f32 {
        s.graphemes(true).count() as f32
    }

    #[test]
    fn short_names_are_untouched() {
        for name in ["a.txt", "Documents", ".profile", ""] {
            assert_eq!(middle(name, 40.0, width), name);
        }
    }

    #[test]
    fn dots_in_parent_directories_are_not_extensions() {
        let path = "/.config/a-very-long-directory-name";
        let result = middle(path, 16.0, width);
        assert!(result.starts_with("/.config/"), "{result}");
        assert!(result.ends_with('e'));
        let result = middle("/.config/long-filename.toml", 16.0, width);
        assert!(result.ends_with(".toml"));
    }

    #[test]
    fn extensions_and_graphemes_survive_elision() {
        for name in [
            "a-very-long-filename.tar.gz",
            "e\u{301}e\u{301}e\u{301}-👨\u{200d}👩\u{200d}👧.png",
            "long-directory-name",
        ] {
            let result = middle(name, 8.0, width);
            assert!(width(&result) <= 8.0);
            assert!(result.contains('…'));
            assert!(!result.starts_with('\u{301}'));
            if name.ends_with(".png") {
                assert!(result.ends_with(".png"));
            }
            if name.ends_with(".gz") {
                assert!(result.ends_with(".gz"));
            }
            if name.ends_with("name") {
                assert!(result.ends_with('e'));
            }
        }
    }

    #[test]
    fn tiny_or_invalid_widths_are_safe() {
        assert_eq!(middle("long-name.md", 1.0, width), "…");
        for budget in [0.0, 0.5, -1.0, f32::NAN, f32::INFINITY] {
            assert_eq!(middle("long-name.md", budget, width), "");
        }
    }

    #[test]
    fn long_extension_has_a_bounded_tail_and_search() {
        let mut calls = 0;
        let result = middle("report.this-extension-is-far-too-long", 18.0, |s| {
            calls += 1;
            width(s)
        });
        assert!(result.ends_with(".ar-too-long"));
        assert!(calls <= 32);
        assert!(width(&result) <= 18.0);
    }
}

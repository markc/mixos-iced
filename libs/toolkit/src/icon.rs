// SPDX-License-Identifier: MIT OR Apache-2.0
//! A named glyph from the installed [`IconFont`](crate::IconFont), as a
//! text widget: `icon("delete").size(20)`.
//!
//! The glyph takes the text colour of its surroundings unless given one, so
//! an icon in a button follows the button's text colour under every theme.
//! A name the installed table lacks (or no installed icon font) renders the
//! name itself in the default font, so a missing icon is visible rather
//! than blank.

use iced_core::widget::text::{Catalog, Style, StyleFn};
use iced_core::{Color, Element, Font, Pixels, text};
use iced_widget::Text;

/// A glyph by name.
#[derive(Debug, Clone, PartialEq)]
pub struct Icon {
    name: String,
    size: Option<Pixels>,
    color: Option<Color>,
}

/// [`Icon::new`].
pub fn icon(name: impl Into<String>) -> Icon {
    Icon::new(name)
}

impl Icon {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            size: None,
            color: None,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The glyph size (the text size).
    pub fn size(mut self, size: impl Into<Pixels>) -> Self {
        self.size = Some(size.into());
        self
    }

    /// A fixed colour instead of the surrounding text colour.
    pub fn color(mut self, color: impl Into<Color>) -> Self {
        self.color = Some(color.into());
        self
    }

    /// The glyph and font, if the installed icon font names it.
    pub fn glyph(&self) -> Option<(char, Font)> {
        crate::fonts::icon(&self.name)
    }

    /// The text widget: the glyph in the icon font, or the name.
    pub fn view<'a, Theme, Renderer>(self) -> Text<'a, Theme, Renderer>
    where
        Theme: Catalog + 'a,
        Theme::Class<'a>: From<StyleFn<'a, Theme>>,
        Renderer: text::Renderer + 'a,
        Renderer::Font: From<Font>,
    {
        let mut text = match self.glyph() {
            Some((glyph, font)) => Text::new(glyph.to_string())
                .font(font)
                .line_height(text::LineHeight::Relative(1.0))
                .shaping(text::Shaping::Advanced),
            None => Text::new(self.name),
        };
        if let Some(size) = self.size {
            text = text.size(size);
        }
        let color = self.color;
        text.style(move |_| Style { color })
    }
}

impl<'a, Message, Theme, Renderer> From<Icon> for Element<'a, Message, Theme, Renderer>
where
    Theme: Catalog + 'a,
    Theme::Class<'a>: From<StyleFn<'a, Theme>>,
    Renderer: text::Renderer + 'a,
    Renderer::Font: From<Font>,
{
    fn from(icon: Icon) -> Self {
        icon.view().into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_keeps_name_size_and_colour() {
        let plain = icon("delete");
        assert_eq!(plain.name(), "delete");
        assert_eq!(plain.size, None);
        assert_eq!(plain.color, None);
        let sized = Icon::new("folder").size(20).color(Color::TRANSPARENT);
        assert_eq!(sized.size, Some(Pixels(20.0)));
        assert_eq!(sized.color, Some(Color::TRANSPARENT));
        // Without an installed icon font there is no glyph; the view then
        // shows the name (checked by the gallery snapshot, which installs
        // nothing either).
        assert!(Icon::new("no-such-icon").glyph().is_none() || crate::fonts::installed().is_some());
    }
}

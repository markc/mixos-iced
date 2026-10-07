// SPDX-License-Identifier: MIT OR Apache-2.0
//! Prepared text defaults supplied by the host. No settings, files or font
//! discovery here: standard controls take the same current role on every view.
use iced_core::{Font, Pixels, text::LineHeight};
use std::collections::BTreeMap;

/// One prepared role: font, logical text size and optional absolute line
/// height. Generic over the host renderer's font, so a shared role applies
/// to any renderer; plain `TextStyle` is the `iced_core::Font` form.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextStyle<F = Font> {
    pub font: F,
    pub size: f32,
    pub line_height: Option<f32>,
}
impl<F> TextStyle<F> {
    /// Height of a line box laid out with this style, in logical pixels:
    /// the absolute line height when set, else iced's default 1.3 factor.
    pub fn line_box(self) -> f32 {
        self.line_height.unwrap_or(self.size * 1.3)
    }

    /// The iced [`LineHeight`] this style lays out with: absolute when a
    /// line height is set, else the default relative factor.
    pub fn line_height_or_default(self) -> LineHeight {
        match self.line_height {
            Some(height) => LineHeight::Absolute(Pixels(height)),
            None => LineHeight::default(),
        }
    }

    pub fn text<'a, Theme, Renderer>(
        self,
        content: impl iced_core::text::IntoFragment<'a>,
    ) -> iced_widget::Text<'a, Theme, Renderer>
    where
        Theme: iced_widget::text::Catalog,
        Renderer: iced_core::text::Renderer<Font = F>,
    {
        iced_widget::Text::new(content)
            .font(self.font)
            .size(self.size)
            .line_height(self.line_height_or_default())
    }

    pub fn input<'a, Message, Theme, Renderer>(
        self,
        placeholder: impl iced_core::text::IntoFragment<'a>,
        value: impl iced_core::text::IntoFragment<'a>,
    ) -> iced_widget::TextInput<'a, Message, Theme, Renderer>
    where
        Message: Clone,
        Theme: iced_widget::text_input::Catalog,
        Renderer: iced_core::text::Renderer<Font = F>,
    {
        iced_widget::TextInput::new(placeholder, value)
            .font(self.font)
            .size(self.size)
            .line_height(self.line_height_or_default())
    }
}

/// Every record is immutable after preparation. Hosts swap the whole collection
/// with their tokens; a view borrows current defaults rather than startup copies.
#[derive(Clone, Debug, PartialEq)]
pub struct Typography(BTreeMap<String, TextStyle>);
impl Typography {
    pub fn new(records: BTreeMap<String, TextStyle>) -> Result<Self, &'static str> {
        if records.is_empty()
            || records.iter().any(|(name, style)| {
                name.is_empty()
                    || !style.size.is_finite()
                    || style.size <= 0.0
                    || style
                        .line_height
                        .is_some_and(|height| !height.is_finite() || height <= 0.0)
            })
        {
            return Err("invalid prepared typography");
        }
        Ok(Self(records))
    }
    pub fn get(&self, name: &str) -> Option<TextStyle> {
        self.0.get(name).copied()
    }
    pub fn records(&self) -> &BTreeMap<String, TextStyle> {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Face(u8);

    #[test]
    fn line_box_uses_the_absolute_line_height_else_the_default_factor() {
        assert_eq!(
            TextStyle {
                font: Font::DEFAULT,
                size: 10.0,
                line_height: Some(30.0)
            }
            .line_box(),
            30.0
        );
        assert_eq!(
            TextStyle {
                font: Font::DEFAULT,
                size: 10.0,
                line_height: None
            }
            .line_box(),
            13.0
        );
    }

    #[test]
    fn text_styles_carry_non_default_fonts() {
        let style = TextStyle {
            font: Face(1),
            size: 12.0,
            line_height: Some(20.0),
        };
        assert_eq!(style.font, Face(1));
        assert_eq!(
            style.line_height_or_default(),
            LineHeight::Absolute(Pixels(20.0))
        );
        assert_eq!(
            TextStyle {
                font: Face(0),
                size: 12.0,
                line_height: None
            }
            .line_height_or_default(),
            LineHeight::default()
        );
    }
}

// SPDX-License-Identifier: MIT OR Apache-2.0
//! Prepared text defaults supplied by the host. No settings, files or font
//! discovery here: standard controls take the same current role on every view.
use iced_core::{Font, Pixels, text::LineHeight};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextStyle {
    pub font: Font,
    pub size: f32,
    pub line_height: Option<f32>,
}
impl TextStyle {
    pub fn text<'a, Theme, Renderer>(
        self,
        content: impl iced_core::text::IntoFragment<'a>,
    ) -> iced_widget::Text<'a, Theme, Renderer>
    where
        Theme: iced_widget::text::Catalog,
        Renderer: iced_core::text::Renderer<Font = Font>,
    {
        let text = iced_widget::Text::new(content).font(self.font).size(self.size);
        match self.line_height {
            Some(height) => text.line_height(LineHeight::Absolute(Pixels(height))),
            None => text,
        }
    }
    pub fn input<'a, Message, Theme, Renderer>(
        self,
        placeholder: impl iced_core::text::IntoFragment<'a>,
        value: impl iced_core::text::IntoFragment<'a>,
    ) -> iced_widget::TextInput<'a, Message, Theme, Renderer>
    where
        Message: Clone,
        Theme: iced_widget::text_input::Catalog,
        Renderer: iced_core::text::Renderer<Font = Font>,
    {
        let input = iced_widget::TextInput::new(placeholder, value).font(self.font).size(self.size);
        match self.line_height {
            Some(height) => input.line_height(LineHeight::Absolute(Pixels(height))),
            None => input,
        }
    }
}

/// Every record is immutable after preparation. Hosts swap the whole collection
/// with their tokens; a view borrows current defaults rather than startup copies.
#[derive(Clone, Debug, PartialEq)]
pub struct Typography(BTreeMap<String, TextStyle>);
impl Typography {
    pub fn new(records: BTreeMap<String, TextStyle>) -> Result<Self, &'static str> {
        if records.is_empty() || records.iter().any(|(name, style)| {
            name.is_empty() || !style.size.is_finite() || style.size <= 0.0
                || style.line_height.is_some_and(|height| !height.is_finite() || height <= 0.0)
        }) {
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

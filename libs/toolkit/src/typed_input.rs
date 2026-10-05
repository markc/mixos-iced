// SPDX-License-Identifier: MIT OR Apache-2.0
//! A field that can only be filled with a specific type:
//! [`TypedInput`], an iced text input whose value is `T: FromStr` — typing
//! an unparseable value simply produces no message, and `on_submit`
//! reports `Err(text)` for a value that does not parse.
//!
//! [`NumberInput`](crate::number_input::NumberInput) layers numeric
//! bounds and step buttons on top of this.

use iced_core::layout::{Layout, Limits, Node};
use iced_core::mouse::{self, Cursor};
use iced_core::widget::tree::{State, Tag};
use iced_core::widget::{Operation, Tree, Widget};
use iced_core::{Element, Event, Length, Padding, Pixels, Rectangle, Size};
use iced_widget::text_input::{self, TextInput};

use std::fmt::Display;
use std::str::FromStr;

/// The default padding.
const DEFAULT_PADDING: Padding = Padding::new(5.0);

/// A field that can only be filled with a specific type.
#[allow(missing_debug_implementations)]
pub struct TypedInput<'a, T, Message, Theme, Renderer>
where
    Renderer: iced_core::text::Renderer,
    Theme: text_input::Catalog,
{
    /// The current value of the [`TypedInput`].
    value: T,
    /// The underlying text input.
    text_input: TextInput<'a, InternalMessage, Theme, Renderer>,
    /// The current text (parsed or not).
    text: String,
    /// The `on_change` event of the [`TypedInput`].
    on_change: Option<Box<dyn 'a + Fn(T) -> Message>>,
    /// The `on_submit` event of the [`TypedInput`].
    #[allow(clippy::type_complexity)]
    on_submit: Option<Box<dyn 'a + Fn(Result<T, String>) -> Message>>,
}

#[derive(Debug, Clone, PartialEq)]
enum InternalMessage {
    OnChange(String),
    OnSubmit,
}

impl<'a, T, Message, Theme, Renderer> TypedInput<'a, T, Message, Theme, Renderer>
where
    T: Display + FromStr,
    Message: Clone,
    Renderer: iced_core::text::Renderer,
    Theme: text_input::Catalog,
{
    /// Creates a new [`TypedInput`] showing `value`.
    #[must_use]
    pub fn new(placeholder: &'a str, value: &T) -> Self
    where
        T: 'a + Clone,
    {
        Self {
            value: value.clone(),
            text_input: TextInput::new(placeholder, value.to_string())
                .padding(DEFAULT_PADDING)
                .width(Length::Fixed(127.0))
                .class(<Theme as text_input::Catalog>::default()),
            text: value.to_string(),
            on_change: None,
            on_submit: None,
        }
    }

    /// Sets the [`Id`](iced_core::widget::Id) of the internal text input.
    #[must_use]
    pub fn id(mut self, id: impl Into<iced_core::widget::Id>) -> Self {
        self.text_input = self.text_input.id(id);
        self
    }

    /// Converts the [`TypedInput`] into a secure password input.
    #[must_use]
    pub fn secure(mut self, is_secure: bool) -> Self {
        self.text_input = self.text_input.secure(is_secure);
        self
    }

    /// Sets the message produced when some valid text is typed. If neither
    /// this nor [`on_submit`](Self::on_submit) is set, the input is inert.
    #[must_use]
    pub fn on_input<F>(mut self, callback: F) -> Self
    where
        F: 'a + Fn(T) -> Message,
    {
        self.text_input = self.text_input.on_input(InternalMessage::OnChange);
        self.on_change = Some(Box::new(callback));
        self
    }

    /// Sets the message produced when Enter is pressed while focused:
    /// `Ok(T)` for a parseable value, `Err(text)` otherwise. Also wires
    /// change tracking, as the iced text input needs it for submission.
    #[must_use]
    pub fn on_submit<F>(mut self, callback: F) -> Self
    where
        F: 'a + Fn(Result<T, String>) -> Message,
    {
        self.text_input = self
            .text_input
            .on_input(InternalMessage::OnChange)
            .on_submit(InternalMessage::OnSubmit);
        self.on_submit = Some(Box::new(callback));
        self
    }

    /// Sets the font of the [`TypedInput`].
    #[must_use]
    pub fn font(mut self, font: impl Into<Renderer::Font>) -> Self {
        self.text_input = self.text_input.font(font.into());
        self
    }

    /// Sets the width of the [`TypedInput`].
    #[must_use]
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.text_input = self.text_input.width(width);
        self
    }

    /// Sets the padding of the [`TypedInput`].
    #[must_use]
    pub fn padding(mut self, padding: impl Into<Padding>) -> Self {
        self.text_input = self.text_input.padding(padding);
        self
    }

    /// Sets the text size of the [`TypedInput`].
    #[must_use]
    pub fn size(mut self, size: impl Into<Pixels>) -> Self {
        self.text_input = self.text_input.size(size);
        self
    }

    /// Sets the line height of the [`TypedInput`].
    #[must_use]
    pub fn line_height(mut self, line_height: impl Into<iced_widget::text::LineHeight>) -> Self {
        self.text_input = self.text_input.line_height(line_height);
        self
    }

    /// Sets the horizontal alignment of the [`TypedInput`].
    #[must_use]
    pub fn align_x(mut self, alignment: iced_core::alignment::Horizontal) -> Self {
        self.text_input = self.text_input.align_x(alignment);
        self
    }

    /// Sets the style of the underlying text input.
    #[must_use]
    pub fn style(
        mut self,
        style: impl Fn(&Theme, text_input::Status) -> text_input::Style + 'a,
    ) -> Self
    where
        <Theme as text_input::Catalog>::Class<'a>: From<text_input::StyleFn<'a, Theme>>,
    {
        self.text_input = self.text_input.style(style);
        self
    }

    /// Sets the style class of the underlying text input.
    #[must_use]
    pub fn class(mut self, class: impl Into<<Theme as text_input::Catalog>::Class<'a>>) -> Self {
        self.text_input = self.text_input.class(class);
        self
    }

    /// The current text, whether or not it parses.
    pub fn text(&self) -> &str {
        &self.text
    }
}

impl<'a, T, Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for TypedInput<'a, T, Message, Theme, Renderer>
where
    T: Display + FromStr + Clone + PartialEq,
    Message: 'a + Clone,
    Renderer: 'a + iced_core::text::Renderer + 'static,
    Theme: text_input::Catalog,
{
    fn tag(&self) -> Tag {
        self.text_input.tag()
    }
    fn state(&self) -> State {
        self.text_input.state()
    }

    fn diff(&mut self, state: &mut Tree) {
        self.text_input.diff(state);
    }

    fn size(&self) -> Size<Length> {
        <TextInput<_, _, _> as Widget<_, _, _>>::size(&self.text_input)
    }

    fn layout(&mut self, state: &mut Tree, renderer: &Renderer, limits: &Limits) -> Node {
        self.text_input.layout(state, renderer, limits)
    }

    fn draw(
        &self,
        state: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &iced_core::renderer::Style,
        layout: Layout<'_>,
        cursor: Cursor,
        viewport: &Rectangle,
    ) {
        self.text_input
            .draw(state, renderer, theme, style, layout, cursor, viewport);
    }

    fn mouse_interaction(
        &self,
        state: &Tree,
        layout: Layout<'_>,
        cursor: Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.text_input.mouse_interaction(state, layout, cursor, viewport, renderer)
    }

    fn operate(
        &mut self,
        state: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.text_input
            .operate(state, layout, renderer, operation);
    }

    fn update(
        &mut self,
        state: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: Cursor,
        renderer: &Renderer,
        shell: &mut iced_core::Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        // A local bus captures the inner input's messages so this widget
        // can validate them before publishing anything.
        let mut bus = iced_core::shell::Bus::new();
        let sub_shell = {
            let mut sub_shell = shell.local(&mut bus);
            self.text_input.update(
                state,
                event,
                layout,
                cursor,
                renderer,
                &mut sub_shell,
                viewport,
            );
            sub_shell
        };
        // Merge the inner shell's redraw request, IME request and layout
        // invalidations upward; its messages all went to the bus.
        shell.merge(sub_shell, |_| unreachable!("bus carried the messages"));

        for message in bus.drain() {
            match message {
                InternalMessage::OnChange(value) => {
                    self.text = value;

                    if let Ok(value) = T::from_str(&self.text)
                        && self.value != value
                        && let Some(on_change) = &self.on_change
                    {
                        self.value = value.clone();
                        shell.publish(on_change(value));
                    }

                    shell.invalidate_layout();
                }
                InternalMessage::OnSubmit => {
                    if let Some(on_submit) = &self.on_submit {
                        let value = match T::from_str(&self.text) {
                            Ok(v) => Ok(v),
                            Err(_) => Err(self.text.clone()),
                        };
                        shell.publish(on_submit(value));
                    }

                    shell.invalidate_layout();
                }
            }
        }
    }
}

impl<'a, T, Message, Theme, Renderer> From<TypedInput<'a, T, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    T: 'a + Display + FromStr + Clone + PartialEq,
    Message: 'a + Clone,
    Renderer: 'a + iced_core::text::Renderer + 'static,
    Theme: 'a + text_input::Catalog,
{
    fn from(typed_input: TypedInput<'a, T, Message, Theme, Renderer>) -> Self {
        Element::new(typed_input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_renderer::LayoutRenderer;

    type TestInput<'a> = TypedInput<'a, u32, u32, iced_core::Theme, LayoutRenderer>;

    #[test]
    fn new_carries_the_value() {
        let input = TestInput::new("Enter a number", &42);
        assert_eq!(input.value, 42);
        assert_eq!(input.text(), "42");
        assert!(input.on_change.is_none());
        assert!(input.on_submit.is_none());
    }

    #[test]
    fn callbacks_are_registered() {
        let input = TestInput::new("n", &1).on_input(|v| v * 2).on_submit(|_| 0);
        assert!(input.on_change.is_some());
        assert!(input.on_submit.is_some());
    }
}

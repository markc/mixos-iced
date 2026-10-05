// SPDX-License-Identifier: MIT OR Apache-2.0
//! A field that can only be filled with a numeric type:
//! [`NumberInput`], a [`TypedInput`] with bounds, a step, and up/down
//! modifier buttons (mouse, wheel and Arrow Up/Down all step).
//!
//! A value is only published when the text parses and lies within the
//! bounds. (iced_aw also refuses each keystroke that would leave the text
//! unparseable, but this iced base no longer exposes the text input's
//! cursor internals, so the field can transiently hold text that does
//! not parse; it never publishes it.) The modifier glyphs are text
//! (`▲` `▼`, or `+` `-` beside a tight padding), so no icon font is
//! needed.

use iced_core::layout::{Limits, Node};
use iced_core::mouse::{self, Cursor};
use iced_core::renderer;
use iced_core::widget::tree::{State, Tag};
use iced_core::widget::{Operation, Tree, Widget};
use iced_core::{
    Alignment, Background, Border, Color, Element, Event, Layout, Length, Padding, Point,
    Rectangle, Shell, Size, alignment::Vertical, keyboard,
};
use iced_widget::{Column, Container, Row, text::LineHeight, text_input};
use num_traits::{Num, NumAssignOps, bounds::Bounded};
use std::{
    fmt::Display,
    ops::{Bound, RangeBounds},
    str::FromStr,
};

use crate::typed_input::TypedInput;

/// The default padding.
const DEFAULT_PADDING: Padding = Padding::new(5.0);

/// The interaction status of a [`NumberInput`]'s modifier buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Status {
    /// Idle.
    #[default]
    Active,
    /// The button is held.
    Pressed,
    /// The value cannot move that way (at the bound).
    Disabled,
}

/// The style of a [`NumberInput`]'s modifier buttons.
#[derive(Clone, Copy, Debug)]
pub struct Style {
    /// The background of a modifier button; `None` draws nothing.
    pub button_background: Option<Background>,
    /// The color of the modifier glyph.
    pub icon_color: Color,
}

/// The theme catalog of a [`NumberInput`].
pub trait Catalog {
    /// The item class of [`Catalog`].
    type Class<'a>;
    /// The default class produced by the [`Catalog`].
    fn default<'a>() -> Self::Class<'a>;
    /// The [`Style`] of a class with the given status.
    fn style(&self, class: &Self::Class<'_>, status: Status) -> Style;
}

/// A styling function for a [`NumberInput`].
pub type StyleFn<'a, Theme> = Box<dyn Fn(&Theme, Status) -> Style + 'a>;

/// A field that can only be filled with numeric type.
///
/// ```no_run
/// # use toolkit::number_input::NumberInput;
/// #[derive(Clone)]
/// enum Message { Amount(u32) }
///
/// fn view<'a, Theme, Renderer>() -> iced_core::Element<'a, Message, Theme, Renderer>
/// where
///     Theme: toolkit::number_input::Catalog + iced_widget::text_input::Catalog + 'a,
///     Renderer: iced_core::text::Renderer<Font = iced_core::Font> + 'a,
/// {
///     NumberInput::new(&12, 0..=1275, Message::Amount)
///         .step(2)
///         .into()
/// }
/// ```
#[allow(missing_debug_implementations)]
pub struct NumberInput<'a, T, Message, Theme, Renderer>
where
    Renderer: iced_core::text::Renderer<Font = iced_core::Font>,
    Theme: Catalog + text_input::Catalog,
{
    /// The current value of the [`NumberInput`].
    value: T,
    /// The step for each modify of the [`NumberInput`].
    step: T,
    /// The min value of the [`NumberInput`].
    min: Bound<T>,
    /// The max value of the [`NumberInput`].
    max: Bound<T>,
    /// The content padding of the [`NumberInput`].
    padding: Padding,
    /// The text size of the [`NumberInput`].
    size: Option<iced_core::Pixels>,
    /// The underlying typed input.
    content: TypedInput<'a, T, InternalMessage<T>, Theme, Renderer>,
    /// The `on_change` event of the [`NumberInput`].
    on_change: Option<Box<dyn 'a + Fn(T) -> Message>>,
    /// The `on_submit` event of the [`NumberInput`].
    on_submit: Option<Message>,
    /// The style of the [`NumberInput`].
    class: <Theme as Catalog>::Class<'a>,
    /// Ignore mouse scroll events; `false` by default.
    ignore_scroll_events: bool,
    /// Skip drawing the modifier buttons; `false` by default.
    ignore_buttons: bool,
}

#[derive(Debug, Clone, PartialEq)]
enum InternalMessage<T> {
    OnChange(T),
    OnSubmit(Result<T, String>),
}

impl<'a, T, Message, Theme, Renderer> NumberInput<'a, T, Message, Theme, Renderer>
where
    T: Num + NumAssignOps + PartialOrd + Display + FromStr + Clone + Bounded + 'a,
    Message: Clone + 'a,
    Renderer: iced_core::text::Renderer<Font = iced_core::Font>,
    Theme: Catalog + text_input::Catalog,
{
    /// Creates a new [`NumberInput`] over `bounds`, producing a message on
    /// each valid change.
    pub fn new<F>(value: &T, bounds: impl RangeBounds<T>, on_change: F) -> Self
    where
        F: 'a + Fn(T) -> Message + Clone,
    {
        Self {
            value: value.clone(),
            step: T::one(),
            min: bounds.start_bound().cloned(),
            max: bounds.end_bound().cloned(),
            padding: DEFAULT_PADDING,
            size: None,
            content: TypedInput::new("", value)
                .on_input(InternalMessage::OnChange)
                .padding(DEFAULT_PADDING)
                .width(Length::Fixed(127.0)),
            on_change: Some(Box::new(on_change)),
            on_submit: None,
            class: <Theme as Catalog>::default(),
            ignore_scroll_events: false,
            ignore_buttons: false,
        }
    }

    /// Sets the [`Id`](iced_core::widget::Id) of the underlying input.
    #[must_use]
    pub fn id(mut self, id: impl Into<iced_core::widget::Id>) -> Self {
        self.content = self.content.id(id.into());
        self
    }

    /// Sets the message produced on each valid typed change.
    #[must_use]
    pub fn on_input<F>(mut self, callback: F) -> Self
    where
        F: 'a + Fn(T) -> Message,
    {
        self.content = self.content.on_input(InternalMessage::OnChange);
        self.on_change = Some(Box::new(callback));
        self
    }

    /// Sets the message produced when Enter is pressed while focused.
    #[must_use]
    pub fn on_submit(mut self, message: Message) -> Self {
        self.content = self
            .content
            .on_input(InternalMessage::OnChange)
            .on_submit(InternalMessage::OnSubmit);
        self.on_submit = Some(message);
        self
    }

    /// Sets the font of the [`NumberInput`].
    #[must_use]
    pub fn font(mut self, font: impl Into<Renderer::Font>) -> Self {
        self.content = self.content.font(font);
        self
    }

    /// Sets the width of the [`NumberInput`].
    #[must_use]
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.content = self.content.width(width);
        self
    }

    /// Sets the padding of the [`NumberInput`]. A padding tighter than the
    /// default on the top, bottom or right lays the modifier buttons out
    /// beside (`+` `-`) instead of above (`▲` `▼`).
    #[must_use]
    pub fn padding(mut self, padding: impl Into<Padding>) -> Self {
        self.padding = padding.into();
        self.content = self.content.padding(self.padding);
        self
    }

    /// Sets the text size of the [`NumberInput`].
    #[must_use]
    pub fn set_size(mut self, size: impl Into<iced_core::Pixels>) -> Self {
        self.size = Some(size.into());
        self
    }

    /// Sets the line height of the [`NumberInput`].
    #[must_use]
    pub fn line_height(
        mut self,
        line_height: impl Into<iced_widget::text::LineHeight>,
    ) -> Self {
        self.content = self.content.line_height(line_height);
        self
    }

    /// Sets the horizontal alignment of the [`NumberInput`].
    #[must_use]
    pub fn align_x(mut self, alignment: impl Into<iced_core::alignment::Horizontal>) -> Self {
        self.content = self.content.align_x(alignment);
        self
    }

    /// Sets the style of the [`NumberInput`]'s modifier buttons.
    #[must_use]
    pub fn style(mut self, style: impl Fn(&Theme, Status) -> Style + 'a) -> Self
    where
        <Theme as Catalog>::Class<'a>: From<StyleFn<'a, Theme>>,
    {
        self.class = (Box::new(style) as StyleFn<'a, Theme>).into();
        self
    }

    /// Sets the style class of the [`NumberInput`]'s modifier buttons.
    #[must_use]
    pub fn class(mut self, class: impl Into<<Theme as Catalog>::Class<'a>>) -> Self {
        self.class = class.into();
        self
    }

    /// Sets the style class of the underlying text input.
    #[must_use]
    pub fn input_class(
        mut self,
        class: impl Into<<Theme as text_input::Catalog>::Class<'a>>,
    ) -> Self {
        self.content = self.content.class(class);
        self
    }

    /// Replaces the bounds.
    #[must_use]
    pub fn bounds(mut self, bounds: impl RangeBounds<T>) -> Self {
        self.min = bounds.start_bound().cloned();
        self.max = bounds.end_bound().cloned();
        self
    }

    /// Sets the step of the [`NumberInput`].
    #[must_use]
    pub fn step(mut self, step: T) -> Self {
        self.step = step;
        self
    }

    /// Skips the modifier buttons entirely.
    #[must_use]
    pub fn ignore_buttons(mut self, ignore: bool) -> Self {
        self.ignore_buttons = ignore;
        self
    }

    /// Ignores the mouse wheel over the input.
    #[must_use]
    pub fn ignore_scroll(mut self, ignore: bool) -> Self {
        self.ignore_scroll_events = ignore;
        self
    }

    /// Decrease the value by one step, clamped into the bounds.
    fn decrease_value(&mut self, shell: &mut Shell<Message>) {
        let min = self.min();

        if self.value <= min {
            return;
        }

        let next = self.value.clone() - self.step.clone();

        if next >= min && self.valid(&next) {
            self.value = next;
        } else {
            self.value = min;
        }

        if let Some(on_change) = &self.on_change {
            shell.publish(on_change(self.value.clone()));
        }
    }

    /// Increase the value by one step, clamped into the bounds.
    fn increase_value(&mut self, shell: &mut Shell<Message>) {
        let max = self.max();

        if self.value >= max {
            return;
        }

        let next = self.value.clone() + self.step.clone();

        if next <= max && self.valid(&next) {
            self.value = next;
        } else {
            self.value = max;
        }

        if let Some(on_change) = &self.on_change {
            shell.publish(on_change(self.value.clone()));
        }
    }

    /// The lowest reachable value: an excluded bound moves in by a step.
    fn min(&self) -> T {
        match &self.min {
            Bound::Included(n) => n.clone(),
            Bound::Excluded(n) => n.clone() + self.step.clone(),
            Bound::Unbounded => T::min_value(),
        }
    }

    /// The highest reachable value: an excluded bound moves in by a step.
    fn max(&self) -> T {
        match &self.max {
            Bound::Included(n) => n.clone(),
            Bound::Excluded(n) => n.clone() - self.step.clone(),
            Bound::Unbounded => T::max_value(),
        }
    }

    /// Whether the value lies within the bounds.
    fn valid(&self, value: &T) -> bool {
        (match &self.min {
            Bound::Included(n) if *n > *value => false,
            Bound::Excluded(n) if *n >= *value => false,
            _ => true,
        }) && (match &self.max {
            Bound::Included(n) if *n < *value => false,
            Bound::Excluded(n) if *n <= *value => false,
            _ => true,
        })
    }

    /// Whether the value can still be increased.
    fn can_increase(&self) -> bool {
        self.value < self.max()
    }

    /// Whether the value can still be decreased.
    fn can_decrease(&self) -> bool {
        self.value > self.min()
    }

    /// Whether the bounds are too tight for the value to ever change.
    fn disabled(&self) -> bool {
        match (&self.min, &self.max) {
            (Bound::Included(n) | Bound::Excluded(n), Bound::Included(m) | Bound::Excluded(m)) => {
                *n >= *m
            }
            _ => false,
        }
    }
}

/// The layout element for the modifier buttons: ▲▼ stacked, or +- beside
/// when the padding is too tight to overlay them.
fn modifier_element<T, Message, Theme, Renderer>(
    padding: &Padding,
    icon_size: f32,
) -> Element<'static, (), Theme, Renderer>
where
    Theme: iced_widget::container::Catalog + 'static,
    Renderer: renderer::Renderer + iced_core::text::Renderer + 'static,
    T: 'static,
    Message: 'static,
{
    let btn_mod = |c| {
        Container::<(), Theme, Renderer>::new(
            iced_widget::Text::new(format!(" {c} ")).size(icon_size),
        )
        .center_y(Length::Shrink)
        .center_x(Length::Shrink)
    };

    let tight = padding.top < DEFAULT_PADDING.top
        || padding.bottom < DEFAULT_PADDING.bottom
        || padding.right < DEFAULT_PADDING.right;

    if tight {
        Element::new(
            Row::<(), Theme, Renderer>::new()
                .spacing(1)
                .width(Length::Shrink)
                .push(btn_mod('+'))
                .push(btn_mod('-')),
        )
    } else {
        Element::new(
            Column::<(), Theme, Renderer>::new()
                .spacing(1)
                .width(Length::Shrink)
                .push(btn_mod('▲'))
                .push(btn_mod('▼')),
        )
    }
}

impl<'a, T, Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for NumberInput<'a, T, Message, Theme, Renderer>
where
    T: Num + NumAssignOps + PartialOrd + Display + FromStr + ToString + Clone + Bounded + 'a,
    Message: 'a + Clone,
    Renderer: 'a + iced_core::text::Renderer<Font = iced_core::Font>,
    Theme: Catalog + text_input::Catalog + iced_core::widget::text::Catalog,
{
    fn tag(&self) -> Tag {
        Tag::of::<ModifierState>()
    }
    fn state(&self) -> State {
        State::new(ModifierState::default())
    }

    fn diff(&mut self, tree: &mut Tree) {
        tree.diff_children_custom(
            &mut [&mut self.content],
            |state, content| content.diff(state),
            |content| Tree {
                tag: content.tag(),
                state: content.state(),
                children: vec![],
            },
        );
    }

    fn size(&self) -> Size<Length> {
        Widget::size(&self.content)
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &Limits) -> Node {
        let num_size = self.size();
        let limits = limits.width(num_size.width).height(Length::Shrink);
        let content = self
            .content
            .layout(&mut tree.children[0], renderer, &limits);
        let limits2 = Limits::new(Size::new(0.0, 0.0), content.size());
        let txt_size = self.size.unwrap_or_else(|| renderer.default_size());

        let icon_size = txt_size * 2.5 / 4.0;

        let mut element =
            modifier_element::<T, Message, Theme, Renderer>(&self.padding, icon_size.0);

        let input_tree = if let Some(child_tree) = tree.children.get_mut(1) {
            child_tree.diff(element.as_widget_mut());
            child_tree
        } else {
            let mut child_tree = Tree::new(element.as_widget());
            element.as_widget_mut().diff(&mut child_tree);
            tree.children.insert(1, child_tree);
            &mut tree.children[1]
        };

        let mut modifier = element
            .as_widget_mut()
            .layout(input_tree, renderer, &limits2.loose());
        let intrinsic = Size::new(
            content.size().width - 1.0,
            content.size().height.max(modifier.size().height),
        );
        modifier = modifier.align(Alignment::End, Alignment::Center, intrinsic);

        let size = limits.resolve(num_size.width, Length::Shrink, intrinsic);
        Node::with_children(size, vec![content, modifier])
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        operation.container(None, layout.bounds());

        let mut children = layout.children();

        if let Some(content_layout) = children.next() {
            self.content
                .operate(&mut tree.children[0], content_layout, renderer, operation);
        }

        if let Some(modifier_layout) = children.next()
            && !self.ignore_buttons
        {
            let txt_size = self.size.unwrap_or_else(|| renderer.default_size());
            let mut element = modifier_element::<T, Message, Theme, Renderer>(
                &self.padding,
                txt_size * 2.5 / 4.0,
            );
            let modifier_tree = if let Some(child_tree) = tree.children.get_mut(1) {
                child_tree.diff(element.as_widget_mut());
                child_tree
            } else {
                let mut child_tree = Tree::new(element.as_widget());
                element.as_widget_mut().diff(&mut child_tree);
                tree.children.insert(1, child_tree);
                &mut tree.children[1]
            };
            element
                .as_widget_mut()
                .operate(modifier_tree, modifier_layout, renderer, operation);
        }
    }

    #[allow(clippy::too_many_lines)]
    fn update(
        &mut self,
        state: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: Cursor,
        renderer: &Renderer,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let mut children = layout.children();
        let content = children.next().expect("fail to get content layout");
        let mut mod_children = children
            .next()
            .expect("fail to get modifiers layout")
            .children();
        let inc_bounds = mod_children
            .next()
            .expect("fail to get increase mod layout")
            .bounds();
        let dec_bounds = mod_children
            .next()
            .expect("fail to get decrease mod layout")
            .bounds();

        if self.disabled() {
            return;
        }
        let can_decrease = self.can_decrease();
        let can_increase = self.can_increase();

        let cursor_position = cursor.position().unwrap_or_default();
        let mouse_over_widget = layout.bounds().contains(cursor_position);
        let mouse_over_inc = inc_bounds.contains(cursor_position);
        let mouse_over_dec = dec_bounds.contains(cursor_position);
        let mouse_over_button = mouse_over_inc || mouse_over_dec;

        let modifiers = state.state.downcast_mut::<ModifierState>();

        let child = state.children.get_mut(0).expect("fail to get child");

        // A local bus drives the underlying input. This vendored iced
        // base no longer exposes the text input's cursor internals, so
        // unlike iced_aw we cannot pre-validate each keystroke; the
        // `TypedInput` underneath still refuses to publish anything that
        // does not parse, which is the guarantee callers rely on.
        let mut bus = iced_core::shell::Bus::new();
        let mut sub_shell = shell.local(&mut bus);

        let mut forward_to_text = |widget: &mut Self, child| {
            widget.content.update(
                child,
                &event.clone(),
                content,
                cursor,
                renderer,
                &mut sub_shell,
                viewport,
            );
        };

        match &event {
            Event::Keyboard(key) => {
                match key {
                    keyboard::Event::ModifiersChanged(_) => forward_to_text(self, child),
                    keyboard::Event::KeyReleased { .. } => return,
                    keyboard::Event::KeyPressed { key, text, modifiers, .. } => {
                        // Numpad arrows arrive with `text` set; treat those
                        // as number entry, not stepping (iced#2278).
                        let has_value = !modifiers.command()
                            && text
                                .as_ref()
                                .is_some_and(|t| t.chars().any(|c| !c.is_control()));

                        match key.as_ref() {
                            // Arrow Down decreases by a step
                            keyboard::Key::Named(keyboard::key::Named::ArrowDown)
                                if can_decrease && !has_value =>
                            {
                                shell.capture_event();
                                shell.request_redraw();
                                self.decrease_value(shell);
                            }
                            // Arrow Up increases by a step
                            keyboard::Key::Named(keyboard::key::Named::ArrowUp)
                                if can_increase && !has_value =>
                            {
                                shell.capture_event();
                                shell.request_redraw();
                                self.increase_value(shell);
                            }
                            // Everything else belongs to the text input.
                            _ => forward_to_text(self, child),
                        }
                    }
                }
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta })
                if mouse_over_widget && !self.ignore_scroll_events =>
            {
                match delta {
                    mouse::ScrollDelta::Lines { y, .. } | mouse::ScrollDelta::Pixels { y, .. } => {
                        if y.is_sign_positive() {
                            self.increase_value(shell);
                        } else {
                            self.decrease_value(shell);
                        }
                    }
                }
                shell.capture_event();
                shell.request_redraw();
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
                if mouse_over_button && !self.ignore_buttons =>
            {
                if mouse_over_dec {
                    modifiers.decrease_pressed = true;
                    self.decrease_value(shell);
                } else {
                    modifiers.increase_pressed = true;
                    self.increase_value(shell);
                }
                shell.capture_event();
                shell.request_redraw();
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                if mouse_over_button =>
            {
                if mouse_over_dec {
                    modifiers.decrease_pressed = false;
                } else {
                    modifiers.increase_pressed = false;
                }
                shell.capture_event();
                shell.request_redraw();
            }
            // Any other event is just forwarded.
            _ => forward_to_text(self, child),
        }

        // Merge the inner shell's redraw request, IME request and layout
        // invalidations upward; its messages all went to the bus.
        shell.merge(sub_shell, |_| unreachable!("bus carried the messages"));

        for message in bus.drain() {
            match message {
                InternalMessage::OnChange(value) => {
                    if self.value != value || self.value.is_zero() {
                        self.value = value.clone();
                        if let Some(on_change) = &self.on_change {
                            shell.publish(on_change(value));
                        }
                    }
                    shell.invalidate_layout();
                }
                InternalMessage::OnSubmit(result) => {
                    if let Err(text) = result {
                        assert!(
                            text.is_empty(),
                            "a number input cannot submit an invalid value"
                        );
                    }
                    if let Some(on_submit) = &self.on_submit {
                        shell.publish(on_submit.clone());
                    }
                    shell.invalidate_layout();
                }
            }
        }
    }

    fn mouse_interaction(
        &self,
        _state: &Tree,
        layout: Layout<'_>,
        cursor: Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        let bounds = layout.bounds();
        let mut children = layout.children();
        let _content_layout = children.next().expect("fail to get content layout");
        let mut mod_children = children
            .next()
            .expect("fail to get modifiers layout")
            .children();
        let inc_bounds = mod_children
            .next()
            .expect("fail to get increase mod layout")
            .bounds();
        let dec_bounds = mod_children
            .next()
            .expect("fail to get decrease mod layout")
            .bounds();
        let is_mouse_over = bounds.contains(cursor.position().unwrap_or_default());
        let is_decrease_disabled = !self.can_decrease();
        let is_increase_disabled = !self.can_increase();
        let mouse_over_decrease = dec_bounds.contains(cursor.position().unwrap_or_default());
        let mouse_over_increase = inc_bounds.contains(cursor.position().unwrap_or_default());

        if ((mouse_over_decrease && !is_decrease_disabled)
            || (mouse_over_increase && !is_increase_disabled))
            && !self.ignore_buttons
        {
            mouse::Interaction::Pointer
        } else if is_mouse_over {
            mouse::Interaction::Text
        } else {
            mouse::Interaction::default()
        }
    }

    fn draw(
        &self,
        state: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: Cursor,
        viewport: &Rectangle,
    ) {
        let mut children = layout.children();
        let content_layout = children.next().expect("fail to get content layout");
        let mut mod_children = children
            .next()
            .expect("fail to get modifiers layout")
            .children();
        let inc_bounds = mod_children
            .next()
            .expect("fail to get increase mod layout")
            .bounds();
        let dec_bounds = mod_children
            .next()
            .expect("fail to get decrease mod layout")
            .bounds();
        self.content.draw(
            &state.children[0],
            renderer,
            theme,
            style,
            content_layout,
            cursor,
            viewport,
        );
        let is_decrease_disabled = !self.can_decrease();
        let is_increase_disabled = !self.can_increase();

        let modifiers = state.state.downcast_ref::<ModifierState>();
        let decrease_btn_style = if is_decrease_disabled {
            <Theme as Catalog>::style(theme, &self.class, Status::Disabled)
        } else if modifiers.decrease_pressed {
            <Theme as Catalog>::style(theme, &self.class, Status::Pressed)
        } else {
            <Theme as Catalog>::style(theme, &self.class, Status::Active)
        };

        let increase_btn_style = if is_increase_disabled {
            <Theme as Catalog>::style(theme, &self.class, Status::Disabled)
        } else if modifiers.increase_pressed {
            <Theme as Catalog>::style(theme, &self.class, Status::Pressed)
        } else {
            <Theme as Catalog>::style(theme, &self.class, Status::Active)
        };

        let txt_size = self.size.unwrap_or_else(|| renderer.default_size());
        let icon_size = txt_size * 2.5 / 4.0;

        if self.ignore_buttons {
            return;
        }
        // Decrease button.
        if dec_bounds.intersects(viewport) {
            renderer.fill_quad(
                renderer::Quad {
                    bounds: dec_bounds,
                    border: Border {
                        radius: (3.0).into(),
                        width: 0.0,
                        color: Color::TRANSPARENT,
                    },
                    ..renderer::Quad::default()
                },
                decrease_btn_style
                    .button_background
                    .unwrap_or(Background::Color(Color::TRANSPARENT)),
            );
        }

        renderer.fill_text(
            iced_core::text::Text {
                content: "▼".to_owned(),
                bounds: Size::new(dec_bounds.width, dec_bounds.height),
                size: icon_size,
                font: renderer.default_font(),
                align_x: iced_core::text::Alignment::Center,
                align_y: Vertical::Center,
                line_height: LineHeight::Relative(1.3),
                shaping: iced_core::text::Shaping::Advanced,
                wrapping: iced_widget::text::Wrapping::default(),
                ellipsis: iced_core::text::Ellipsis::None,
                hint_factor: renderer.hint_factor(),
            },
            Point::new(dec_bounds.center_x(), dec_bounds.center_y()),
            decrease_btn_style.icon_color,
            dec_bounds,
        );

        // Increase button.
        if inc_bounds.intersects(viewport) {
            renderer.fill_quad(
                renderer::Quad {
                    bounds: inc_bounds,
                    border: Border {
                        radius: (3.0).into(),
                        width: 0.0,
                        color: Color::TRANSPARENT,
                    },
                    ..renderer::Quad::default()
                },
                increase_btn_style
                    .button_background
                    .unwrap_or(Background::Color(Color::TRANSPARENT)),
            );
        }

        renderer.fill_text(
            iced_core::text::Text {
                content: "▲".to_owned(),
                bounds: Size::new(inc_bounds.width, inc_bounds.height),
                size: icon_size,
                font: renderer.default_font(),
                align_x: iced_core::text::Alignment::Center,
                align_y: Vertical::Center,
                line_height: LineHeight::Relative(1.3),
                shaping: iced_core::text::Shaping::Advanced,
                wrapping: iced_widget::text::Wrapping::default(),
                ellipsis: iced_core::text::Ellipsis::None,
                hint_factor: renderer.hint_factor(),
            },
            Point::new(inc_bounds.center_x(), inc_bounds.center_y()),
            increase_btn_style.icon_color,
            inc_bounds,
        );
    }
}

/// The modifier state of a [`NumberInput`].
#[derive(Default, Clone, Debug)]
pub struct ModifierState {
    /// Whether the decrease button is held.
    pub decrease_pressed: bool,
    /// Whether the increase button is held.
    pub increase_pressed: bool,
}

impl<'a, T, Message, Theme, Renderer> From<NumberInput<'a, T, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    T: 'a + Num + NumAssignOps + PartialOrd + Display + FromStr + Clone + Bounded,
    Message: 'a + Clone,
    Renderer: 'a + iced_core::text::Renderer<Font = iced_core::Font>,
    Theme: 'a + Catalog + text_input::Catalog,
{
    fn from(num_input: NumberInput<'a, T, Message, Theme, Renderer>) -> Self {
        Element::new(num_input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_renderer::LayoutRenderer;

    type TestInput<'a> = NumberInput<'a, u32, u32, iced_core::Theme, LayoutRenderer>;

    #[test]
    fn new_sets_bounds_and_default_step() {
        let input = TestInput::new(&5, 0..=100, |v| v);
        assert_eq!(input.value, 5);
        assert_eq!(input.step, 1);
        assert!(matches!(input.min, Bound::Included(0)));
        assert!(matches!(input.max, Bound::Included(100)));
        assert!(!input.ignore_buttons);
        assert!(!input.ignore_scroll_events);
    }

    #[test]
    fn bounds_checks() {
        let mut input = TestInput::new(&5, 0..=100, |v| v);
        assert!(input.valid(&50));
        assert!(!input.valid(&101));
        assert!(!input.valid(&200));
        assert!(input.can_increase());
        assert!(input.can_decrease());

        input.value = 100;
        assert!(!input.can_increase());
        input.value = 0;
        assert!(!input.can_decrease());

        let input = TestInput::new(&5, 10..=10, |v| v);
        assert!(input.disabled(), "a closed range never changes");
    }
}

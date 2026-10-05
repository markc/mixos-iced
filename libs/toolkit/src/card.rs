// SPDX-License-Identifier: MIT OR Apache-2.0
//! A card consisting of a head, body and optional foot: [`Card`], with an
//! optional close button in the head. The panels of a settings page, a
//! notification's shell, a dialog's frame.

use iced_core::layout::{Limits, Node};
use iced_core::mouse::{self, Cursor};
use iced_core::overlay;
use iced_core::renderer;
use iced_core::widget::{Operation, Tree};
use iced_core::widget::text::Text;
use iced_core::{
    Alignment, Background, Border, Color, Element, Event, Layout, Length, Padding, Point,
    Rectangle, Shadow, Shell, Size, Vector, Widget,
};
use iced_widget::button;

/// The default padding of a [`Card`].
const DEFAULT_PADDING: Padding = Padding::new(10.0);

/// The default size of the close button glyph.
const DEFAULT_CLOSE_SIZE: f32 = 16.0;

/// Extra space around the close button for its click area.
const CLOSE_BUTTON_SPACING: f32 = 1.0;

/// The style of a [`Card`].
#[derive(Clone, Copy, Debug)]
pub struct Style {
    /// The background of the [`Card`].
    pub background: Background,
    /// The border radius of the [`Card`].
    pub border_radius: f32,
    /// The border width of the [`Card`].
    pub border_width: f32,
    /// The border color of the [`Card`].
    pub border_color: Color,
    /// The background of the head of the [`Card`].
    pub head_background: Background,
    /// The text color of the head of the [`Card`].
    pub head_text_color: Color,
    /// The background of the body of the [`Card`].
    pub body_background: Background,
    /// The text color of the body of the [`Card`].
    pub body_text_color: Color,
    /// The background of the foot of the [`Card`].
    pub foot_background: Background,
    /// The text color of the foot of the [`Card`].
    pub foot_text_color: Color,
    /// The color of the close glyph of the [`Card`].
    pub close_color: Color,
}

/// The theme catalog of a [`Card`].
pub trait Catalog {
    /// The item class of [`Catalog`].
    type Class<'a>;
    /// The default class produced by the [`Catalog`].
    fn default<'a>() -> Self::Class<'a>;
    /// The [`Style`] of a class.
    fn style(&self, class: &Self::Class<'_>) -> Style;
}

/// A styling function for a [`Card`].
pub type StyleFn<'a, Theme> = Box<dyn Fn(&Theme) -> Style + 'a>;

/// A card consisting of a head, body and optional foot.
///
/// ```no_run
/// # use toolkit::card::Card;
/// #[derive(Clone)]
/// enum Message { Closing }
///
/// fn view<'a, Theme, Renderer>() -> iced_core::Element<'a, Message, Theme, Renderer>
/// where
///     Theme: toolkit::card::Catalog + iced_widget::button::Catalog
///         + iced_core::widget::text::Catalog + 'a,
///     Renderer: iced_core::text::Renderer + 'a,
/// {
///     Card::new("Head", "Body")
///         .foot("Foot")
///         .on_close(Message::Closing)
///         .into()
/// }
/// ```
#[allow(missing_debug_implementations)]
pub struct Card<'a, Message, Theme, Renderer>
where
    Renderer: renderer::Renderer,
    Theme: Catalog,
{
    /// The width of the [`Card`].
    width: Length,
    /// The height of the [`Card`].
    height: Length,
    /// The padding of the head of the [`Card`].
    padding_head: Padding,
    /// The padding of the body of the [`Card`].
    padding_body: Padding,
    /// The padding of the foot of the [`Card`].
    padding_foot: Padding,
    /// The optional size of the close glyph of the [`Card`].
    close_size: Option<f32>,
    /// The optional message sent when the close glyph of the [`Card`] is pressed.
    on_close: Option<Message>,
    /// The head [`Element`] of the [`Card`].
    head: Element<'a, Message, Theme, Renderer>,
    /// The body [`Element`] of the [`Card`].
    body: Element<'a, Message, Theme, Renderer>,
    /// The optional foot [`Element`] of the [`Card`].
    foot: Option<Element<'a, Message, Theme, Renderer>>,
    /// The optional close button [`Element`] of the [`Card`].
    close_button: Option<Element<'a, Message, Theme, Renderer>>,
    /// The style of the [`Card`].
    class: Theme::Class<'a>,
}

impl<'a, Message, Theme, Renderer> Card<'a, Message, Theme, Renderer>
where
    Renderer: renderer::Renderer,
    Theme: Catalog,
{
    /// Creates a new [`Card`] containing the given head and body.
    pub fn new<H, B>(head: H, body: B) -> Self
    where
        H: Into<Element<'a, Message, Theme, Renderer>>,
        B: Into<Element<'a, Message, Theme, Renderer>>,
    {
        Card {
            width: Length::Fill,
            height: Length::Shrink,
            padding_head: DEFAULT_PADDING,
            padding_body: DEFAULT_PADDING,
            padding_foot: DEFAULT_PADDING,
            close_size: None,
            on_close: None,
            head: head.into(),
            body: body.into(),
            foot: None,
            close_button: None,
            class: Theme::default(),
        }
    }

    /// Sets the [`Element`] of the foot of the [`Card`].
    #[must_use]
    pub fn foot<F>(mut self, foot: F) -> Self
    where
        F: Into<Element<'a, Message, Theme, Renderer>>,
    {
        self.foot = Some(foot.into());
        self
    }

    /// Sets the size of the close glyph of the [`Card`].
    #[must_use]
    pub fn close_size(mut self, size: f32) -> Self {
        self.close_size = Some(size);
        self
    }

    /// Sets the height of the [`Card`].
    #[must_use]
    pub fn height(mut self, height: impl Into<Length>) -> Self {
        self.height = height.into();
        self
    }

    /// Sets the message produced when the close glyph is pressed. Setting
    /// this is what draws the glyph.
    #[must_use]
    pub fn on_close(mut self, msg: Message) -> Self
    where
        Message: Clone + 'a,
        Renderer: iced_core::text::Renderer<Font = iced_core::Font> + 'a,
        Theme: iced_core::widget::text::Catalog + button::Catalog + 'a,
        <Theme as button::Catalog>::Class<'a>: From<button::StyleFn<'a, Theme>>,
    {
        self.on_close = Some(msg.clone());
        self.close_button = Some(self.create_close_button(msg));
        self
    }

    /// Creates the close button element: a text `×` in the close colour.
    fn create_close_button(&self, msg: Message) -> Element<'a, Message, Theme, Renderer>
    where
        Message: Clone + 'a,
        Renderer: iced_core::text::Renderer<Font = iced_core::Font> + 'a,
        Theme: iced_core::widget::text::Catalog + button::Catalog + 'a,
        <Theme as button::Catalog>::Class<'a>: From<button::StyleFn<'a, Theme>>,
    {
        let size = self.close_size.unwrap_or(DEFAULT_CLOSE_SIZE);

        let glyph = Text::<Theme, Renderer>::new("×").size(size);

        button(glyph)
            .padding(0)
            .style(|theme: &Theme, _status| {
                let card_style = theme.style(&Theme::default());
                button::Style {
                    background: None,
                    text_color: card_style.close_color,
                    border: Border::default(),
                    shadow: Shadow::default(),
                }
            })
            .on_press(msg)
            .into()
    }

    /// Sets the padding of the [`Card`] — head, body and foot together.
    #[must_use]
    pub fn padding(mut self, padding: Padding) -> Self {
        self.padding_head = padding;
        self.padding_body = padding;
        self.padding_foot = padding;
        self
    }

    /// Sets the padding of the head of the [`Card`].
    #[must_use]
    pub fn padding_head(mut self, padding: Padding) -> Self {
        self.padding_head = padding;
        self
    }

    /// Sets the padding of the body of the [`Card`].
    #[must_use]
    pub fn padding_body(mut self, padding: Padding) -> Self {
        self.padding_body = padding;
        self
    }

    /// Sets the padding of the foot of the [`Card`].
    #[must_use]
    pub fn padding_foot(mut self, padding: Padding) -> Self {
        self.padding_foot = padding;
        self
    }

    /// Sets the style of the [`Card`].
    #[must_use]
    pub fn style(mut self, style: impl Fn(&Theme) -> Style + 'a) -> Self
    where
        Theme::Class<'a>: From<StyleFn<'a, Theme>>,
    {
        self.class = (Box::new(style) as StyleFn<'a, Theme>).into();
        self
    }

    /// Sets the class of the [`Card`].
    #[must_use]
    pub fn class(mut self, class: impl Into<Theme::Class<'a>>) -> Self {
        self.class = class.into();
        self
    }

    /// Sets the width of the [`Card`].
    #[must_use]
    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }
}

impl<'a, Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for Card<'a, Message, Theme, Renderer>
where
    Message: 'a + Clone,
    Renderer: 'a + renderer::Renderer + iced_core::text::Renderer<Font = iced_core::Font>,
    Theme: Catalog,
{
    fn diff(&mut self, tree: &mut Tree) {
        match (&mut self.foot, &mut self.close_button) {
            (Some(foot), Some(close_button)) => {
                tree.diff_children(&mut [&mut self.head, &mut self.body, foot, close_button]);
            }
            (Some(foot), None) => {
                tree.diff_children(&mut [&mut self.head, &mut self.body, foot]);
            }
            (None, Some(close_button)) => {
                tree.diff_children(&mut [&mut self.head, &mut self.body, close_button]);
            }
            (None, None) => {
                tree.diff_children(&mut [&mut self.head, &mut self.body]);
            }
        }
    }

    fn size(&self) -> Size<Length> {
        Size {
            width: self.width,
            height: self.height,
        }
    }

    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &Limits) -> Node {
        let close_button_tree_index = 2 + usize::from(self.foot.is_some());

        let head_node = head_node(
            renderer,
            &limits,
            &mut self.head,
            self.padding_head,
            self.width,
            self.close_button.as_mut(),
            self.close_size,
            tree,
            close_button_tree_index,
        );

        let limits = limits.shrink(Size::new(0.0, head_node.size().height));

        let mut foot_node = self
            .foot
            .as_mut()
            .map_or_else(Node::default, |foot| {
                foot_node(renderer, &limits, foot, self.padding_foot, self.width, tree)
            });
        let limits = limits.shrink(Size::new(0.0, foot_node.size().height));
        let mut body_node = body_node(
            renderer,
            &limits,
            &mut self.body,
            self.padding_body,
            self.width,
            tree,
        );
        let body_bounds = body_node.bounds();
        body_node = body_node.move_to(Point::new(
            body_bounds.x,
            body_bounds.y + head_node.bounds().height,
        ));

        let foot_bounds = foot_node.bounds();

        foot_node = foot_node.move_to(Point::new(
            foot_bounds.x,
            foot_bounds.y + head_node.bounds().height + body_node.bounds().height,
        ));

        Node::with_children(
            Size::new(
                body_node.size().width,
                head_node.size().height + body_node.size().height + foot_node.size().height,
            ),
            vec![head_node, body_node, foot_node],
        )
    }

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
        let head_layout = children
            .next()
            .expect("widget: Layout should have a head layout");
        let mut head_children = head_layout.children();

        self.head.as_widget_mut().update(
            &mut state.children[0],
            event,
            head_children
                .next()
                .expect("widget: Layout should have a head content layout"),
            cursor,
            renderer,
            shell,
            viewport,
        );

        // Update the close button if present.
        if let Some((close_layout, close_button)) =
            head_children.next().zip(self.close_button.as_mut())
        {
            let close_button_tree_index = 2 + usize::from(self.foot.is_some());
            close_button.as_widget_mut().update(
                &mut state.children[close_button_tree_index],
                event,
                close_layout,
                cursor,
                renderer,
                shell,
                viewport,
            );
        }

        let body_layout = children
            .next()
            .expect("widget: Layout should have a body layout");

        self.body.as_widget_mut().update(
            &mut state.children[1],
            event,
            body_layout
                .children()
                .next()
                .expect("widget: Layout should have a body content layout"),
            cursor,
            renderer,
            shell,
            viewport,
        );

        let foot_layout = children
            .next()
            .expect("widget: Layout should have a foot layout");

        if let Some(foot) = self.foot.as_mut() {
            foot.as_widget_mut().update(
                &mut state.children[2],
                event,
                foot_layout
                    .children()
                    .next()
                    .expect("widget: Layout should have a foot content layout"),
                cursor,
                renderer,
                shell,
                viewport,
            );
        }
    }

    fn mouse_interaction(
        &self,
        state: &Tree,
        layout: Layout<'_>,
        cursor: Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        let mut children = layout.children();

        let head_layout = children
            .next()
            .expect("widget: Layout should have a head layout");
        let mut head_children = head_layout.children();

        let head = head_children
            .next()
            .expect("widget: Layout should have a head layout");
        let close_layout = head_children.next();

        let is_mouse_over_close = close_layout.is_some_and(|layout| {
            let bounds = layout.bounds();
            bounds.contains(cursor.position().unwrap_or_default())
        });

        let mouse_interaction = if is_mouse_over_close {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::default()
        };

        let body_layout = children
            .next()
            .expect("widget: Layout should have a body layout");
        let mut body_children = body_layout.children();

        let foot_layout = children
            .next()
            .expect("widget: Layout should have a foot layout");
        let mut foot_children = foot_layout.children();

        mouse_interaction
            .max(self.head.as_widget().mouse_interaction(
                &state.children[0],
                head,
                cursor,
                viewport,
                renderer,
            ))
            .max(self.body.as_widget().mouse_interaction(
                &state.children[1],
                body_children
                    .next()
                    .expect("widget: Layout should have a body content layout"),
                cursor,
                viewport,
                renderer,
            ))
            .max(self.foot.as_ref().map_or_else(
                mouse::Interaction::default,
                |foot| {
                    foot.as_widget().mouse_interaction(
                        &state.children[2],
                        foot_children
                            .next()
                            .expect("widget: Layout should have a foot content layout"),
                        cursor,
                        viewport,
                        renderer,
                    )
                },
            ))
    }

    fn operate(
        &mut self,
        state: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        let mut children = layout.children();
        let head_layout = children.next().expect("Missing Head Layout");
        let body_layout = children.next().expect("Missing Body Layout");
        let foot_layout = children.next().expect("Missing Footer Layout");

        let mut head_children = head_layout.children();
        if let Some(head_content_layout) = head_children.next() {
            self.head
                .as_widget_mut()
                .operate(&mut state.children[0], head_content_layout, renderer, operation);
        }

        if let Some((close_layout, close_button)) =
            head_children.next().zip(self.close_button.as_mut())
        {
            let close_button_tree_index = 2 + usize::from(self.foot.is_some());
            close_button.as_widget_mut().operate(
                &mut state.children[close_button_tree_index],
                close_layout,
                renderer,
                operation,
            );
        }

        if let Some(body_content_layout) = body_layout.children().next() {
            self.body
                .as_widget_mut()
                .operate(&mut state.children[1], body_content_layout, renderer, operation);
        }

        if let Some((footer, foot_content_layout)) =
            self.foot.as_mut().zip(foot_layout.children().next())
        {
            footer
                .as_widget_mut()
                .operate(&mut state.children[2], foot_content_layout, renderer, operation);
        }
    }

    fn draw(
        &self,
        state: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        cursor: Cursor,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let mut children = layout.children();
        let style_sheet = theme.style(&self.class);

        if bounds.intersects(viewport) {
            renderer.fill_quad(
                renderer::Quad {
                    bounds,
                    border: Border {
                        radius: style_sheet.border_radius.into(),
                        width: style_sheet.border_width,
                        color: style_sheet.border_color,
                    },
                    shadow: Shadow::default(),
                    ..renderer::Quad::default()
                },
                style_sheet.background,
            );
        }

        // ----------- Head ----------------------
        let head_layout = children
            .next()
            .expect("Graphics: Layout should have a head layout");
        let close_button_tree_index = 2 + usize::from(self.foot.is_some());
        draw_head(
            &state.children[0],
            renderer,
            &self.head,
            head_layout,
            cursor,
            viewport,
            theme,
            &style_sheet,
            self.close_button.as_ref(),
            state.children.get(close_button_tree_index),
        );

        // ----------- Body ----------------------
        let body_layout = children
            .next()
            .expect("Graphics: Layout should have a body layout");
        draw_body(
            &state.children[1],
            renderer,
            &self.body,
            body_layout,
            cursor,
            viewport,
            theme,
            &style_sheet,
        );

        // ----------- Foot ----------------------
        let foot_layout = children
            .next()
            .expect("Graphics: Layout should have a foot layout");
        draw_foot(
            state.children.get(2),
            renderer,
            self.foot.as_ref(),
            foot_layout,
            cursor,
            viewport,
            theme,
            &style_sheet,
        );
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        let mut children = vec![&mut self.head, &mut self.body];
        if let Some(foot) = &mut self.foot {
            children.push(foot);
        }
        let children = children
            .into_iter()
            .zip(&mut tree.children)
            .zip(layout.children())
            .filter_map(|((child, state), layout)| {
                layout.children().next().and_then(|child_layout| {
                    child
                        .as_widget_mut()
                        .overlay(state, child_layout, renderer, viewport, translation)
                })
            })
            .collect::<Vec<_>>();

        if children.is_empty() {
            None
        } else {
            Some(overlay::Group::with_children(children).overlay())
        }
    }
}

/// Calculates the layout of the head.
#[allow(clippy::too_many_arguments)]
fn head_node<Message, Theme, Renderer>(
    renderer: &Renderer,
    limits: &Limits,
    head: &mut Element<'_, Message, Theme, Renderer>,
    padding: Padding,
    width: Length,
    close_button: Option<&mut Element<'_, Message, Theme, Renderer>>,
    close_size: Option<f32>,
    tree: &mut Tree,
    close_button_tree_index: usize,
) -> Node
where
    Renderer: renderer::Renderer + iced_core::text::Renderer<Font = iced_core::Font>,
{
    let header_size = head.as_widget().size();

    let mut limits = limits
        .loose()
        .width(width)
        .height(header_size.height)
        .shrink(padding);

    let close_size = close_size.unwrap_or_else(|| renderer.default_size().0);

    if close_button.is_some() {
        limits = limits.shrink(Size::new(close_size, 0.0));
    }

    let mut head = head
        .as_widget_mut()
        .layout(&mut tree.children[0], renderer, &limits);
    let mut size = limits.resolve(width, header_size.height, head.size());

    head = head.move_to(Point::new(padding.left, padding.top));
    let head_size = head.size();
    head = head.align(Alignment::Start, Alignment::Center, head_size);

    let close = if let Some(close_btn) = close_button {
        let button_size = close_size + CLOSE_BUTTON_SPACING;
        let button_limits = limits
            .loose()
            .width(Length::Fixed(button_size))
            .height(Length::Fixed(button_size));
        let mut close_node = close_btn.as_widget_mut().layout(
            &mut tree.children[close_button_tree_index],
            renderer,
            &button_limits,
        );
        let node_size = close_node.size();

        size = Size::new(size.width + close_size, size.height);

        close_node = close_node
            .move_to(Point::new(size.width - padding.right, padding.top))
            .align(Alignment::End, Alignment::Center, node_size);

        Some(close_node)
    } else {
        None
    };

    Node::with_children(
        size.expand(padding),
        match close {
            Some(node) => vec![head, node],
            None => vec![head],
        },
    )
}

/// Calculates the layout of the body.
fn body_node<Message, Theme, Renderer>(
    renderer: &Renderer,
    limits: &Limits,
    body: &mut Element<'_, Message, Theme, Renderer>,
    padding: Padding,
    width: Length,
    tree: &mut Tree,
) -> Node
where
    Renderer: renderer::Renderer,
{
    let body_size = body.as_widget().size();

    let limits = limits
        .loose()
        .width(width)
        .height(body_size.height)
        .shrink(padding);

    let mut body = body
        .as_widget_mut()
        .layout(&mut tree.children[1], renderer, &limits);
    let size = limits.resolve(width, body_size.height, body.size());

    body = body
        .move_to(Point::new(padding.left, padding.top))
        .align(Alignment::Start, Alignment::Start, size);

    Node::with_children(size.expand(padding), vec![body])
}

/// Calculates the layout of the foot.
fn foot_node<Message, Theme, Renderer>(
    renderer: &Renderer,
    limits: &Limits,
    foot: &mut Element<'_, Message, Theme, Renderer>,
    padding: Padding,
    width: Length,
    tree: &mut Tree,
) -> Node
where
    Renderer: renderer::Renderer,
{
    let foot_size = foot.as_widget().size();

    let limits = limits
        .loose()
        .width(width)
        .height(foot_size.height)
        .shrink(padding);

    let mut foot = foot
        .as_widget_mut()
        .layout(&mut tree.children[2], renderer, &limits);
    let size = limits.resolve(width, foot_size.height, foot.size());

    foot = foot
        .move_to(Point::new(padding.left, padding.right))
        .align(Alignment::Start, Alignment::Center, size);

    Node::with_children(size.expand(padding), vec![foot])
}

/// Draws the head of the card.
#[allow(clippy::too_many_arguments)]
fn draw_head<Message, Theme, Renderer>(
    state: &Tree,
    renderer: &mut Renderer,
    head: &Element<'_, Message, Theme, Renderer>,
    layout: Layout<'_>,
    cursor: Cursor,
    viewport: &Rectangle,
    theme: &Theme,
    style: &Style,
    close_button: Option<&Element<'_, Message, Theme, Renderer>>,
    close_button_state: Option<&Tree>,
) where
    Renderer: renderer::Renderer + iced_core::text::Renderer<Font = iced_core::Font>,
    Theme: Catalog,
{
    let mut head_children = layout.children();
    let bounds = layout.bounds();
    let border_radius = style.border_radius;

    // Head background.
    if bounds.intersects(viewport) {
        renderer.fill_quad(
            renderer::Quad {
                bounds,
                border: Border {
                    radius: border_radius.into(),
                    width: 0.0,
                    color: Color::TRANSPARENT,
                },
                shadow: Shadow::default(),
                ..renderer::Quad::default()
            },
            style.head_background,
        );
    }

    // Cover the rounded corner under the head.
    let button_bounds = Rectangle {
        x: bounds.x,
        y: bounds.y + bounds.height - border_radius,
        width: bounds.width,
        height: border_radius,
    };
    if button_bounds.intersects(viewport) {
        renderer.fill_quad(
            renderer::Quad {
                bounds: button_bounds,
                border: Border {
                    radius: (0.0).into(),
                    width: 0.0,
                    color: Color::TRANSPARENT,
                },
                shadow: Shadow::default(),
                ..renderer::Quad::default()
            },
            style.head_background,
        );
    }

    head.as_widget().draw(
        state,
        renderer,
        theme,
        &renderer::Style {
            text_color: style.head_text_color,
        },
        head_children
            .next()
            .expect("Graphics: Layout should have a head content layout"),
        cursor,
        viewport,
    );

    // Draw the close button if present.
    if let Some((close_layout, (close_btn, close_state))) = head_children
        .next()
        .zip(close_button.zip(close_button_state))
    {
        close_btn.as_widget().draw(
            close_state,
            renderer,
            theme,
            &renderer::Style {
                text_color: style.close_color,
            },
            close_layout,
            cursor,
            viewport,
        );
    }
}

/// Draws the body of the card.
#[allow(clippy::too_many_arguments)]
fn draw_body<Message, Theme, Renderer>(
    state: &Tree,
    renderer: &mut Renderer,
    body: &Element<'_, Message, Theme, Renderer>,
    layout: Layout<'_>,
    cursor: Cursor,
    viewport: &Rectangle,
    theme: &Theme,
    style: &Style,
) where
    Renderer: renderer::Renderer + iced_core::text::Renderer<Font = iced_core::Font>,
    Theme: Catalog,
{
    let mut body_children = layout.children();
    let bounds = layout.bounds();

    if bounds.intersects(viewport) {
        renderer.fill_quad(
            renderer::Quad {
                bounds,
                border: Border {
                    radius: (0.0).into(),
                    width: 0.0,
                    color: Color::TRANSPARENT,
                },
                shadow: Shadow::default(),
                ..renderer::Quad::default()
            },
            style.body_background,
        );
    }

    body.as_widget().draw(
        state,
        renderer,
        theme,
        &renderer::Style {
            text_color: style.body_text_color,
        },
        body_children
            .next()
            .expect("Graphics: Layout should have a body content layout"),
        cursor,
        viewport,
    );
}

/// Draws the foot of the card.
#[allow(clippy::too_many_arguments)]
fn draw_foot<Message, Theme, Renderer>(
    state: Option<&Tree>,
    renderer: &mut Renderer,
    foot: Option<&Element<'_, Message, Theme, Renderer>>,
    layout: Layout<'_>,
    cursor: Cursor,
    viewport: &Rectangle,
    theme: &Theme,
    style: &Style,
) where
    Renderer: renderer::Renderer + iced_core::text::Renderer<Font = iced_core::Font>,
    Theme: Catalog,
{
    let mut foot_children = layout.children();
    let bounds = layout.bounds();

    if bounds.intersects(viewport) {
        renderer.fill_quad(
            renderer::Quad {
                bounds,
                border: Border {
                    radius: style.border_radius.into(),
                    width: 0.0,
                    color: Color::TRANSPARENT,
                },
                shadow: Shadow::default(),
                ..renderer::Quad::default()
            },
            style.foot_background,
        );
    }

    if let Some((foot, state)) = foot.as_ref().zip(state) {
        foot.as_widget().draw(
            state,
            renderer,
            theme,
            &renderer::Style {
                text_color: style.foot_text_color,
            },
            foot_children
                .next()
                .expect("Graphics: Layout should have a foot content layout"),
            cursor,
            viewport,
        );
    }
}

impl<'a, Message, Theme, Renderer> From<Card<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a + Clone,
    Renderer: 'a + renderer::Renderer + iced_core::text::Renderer<Font = iced_core::Font>,
    Theme: 'a + Catalog,
{
    fn from(card: Card<'a, Message, Theme, Renderer>) -> Self {
        Self::new(card)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_renderer::LayoutRenderer;

    type TestCard<'a> = Card<'a, String, iced_core::Theme, LayoutRenderer>;

    #[test]
    fn defaults_are_padding_ten_fill_width() {
        let card = TestCard::new("Head", "Body");
        assert_eq!(card.padding_head, DEFAULT_PADDING);
        assert_eq!(card.padding_body, DEFAULT_PADDING);
        assert_eq!(card.padding_foot, DEFAULT_PADDING);
        assert_eq!(card.width, Length::Fill);
        assert_eq!(card.height, Length::Shrink);
        assert!(card.foot.is_none());
        assert!(card.close_button.is_none());
    }

    #[test]
    fn the_head_sits_above_the_body_and_the_foot_below() {
        let mut card = TestCard::new("Head", "Body").foot("Foot");
        let renderer = LayoutRenderer::new();
        let mut tree = Tree::new(&card);
        let limits = Limits::new(Size::ZERO, Size::new(300.0, f32::INFINITY));
        let node = Widget::<String, iced_core::Theme, LayoutRenderer>::layout(
            &mut card,
            &mut tree,
            &renderer,
            &limits,
        );
        let laid: Vec<_> = Layout::new(&node).children().collect();
        assert_eq!(laid.len(), 3);
        assert!(laid[0].bounds().y <= laid[1].bounds().y, "head at top");
        assert!(laid[1].bounds().y <= laid[2].bounds().y, "foot at bottom");
        assert_eq!(
            node.bounds().height,
            laid[0].bounds().height + laid[1].bounds().height + laid[2].bounds().height
        );
    }
}

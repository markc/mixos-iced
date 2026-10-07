// SPDX-License-Identifier: MIT OR Apache-2.0
//! One menu panel as a standalone widget, for a host that gives each panel
//! its own surface (xdg_popup), plus the panel geometry both modes share.
use iced_core::widget::{Tree, tree};
use iced_core::{
    Element, Event, Font, Layout, Length, Point, Rectangle, Shell, Size, Widget, layout, mouse,
    renderer, text, touch,
};

use super::{Item, Kind, MenuStyle, label, quad, resolve, resolve_text, text_width};
use crate::theme::Catalog;
use crate::typography::TextStyle;

/// Minimum panel width in logical pixels.
pub const MIN_PANEL_WIDTH: f32 = 160.0;
/// Height of a separator row in logical pixels.
pub const SEPARATOR_HEIGHT: f32 = 8.0;

/// Height of a non-separator row: the style's own height without a prepared
/// text style (legacy), else never less than the prepared content height
/// (the text size and the requested line height, whichever is larger; an
/// absent line height is the 1.3 default factor). Separators keep
/// `SEPARATOR_HEIGHT`.
pub(crate) fn row_height<Message, F: Copy>(
    item: &Item<Message>,
    style: MenuStyle,
    text: Option<TextStyle<F>>,
) -> f32 {
    if matches!(item.kind, Kind::Separator) {
        SEPARATOR_HEIGHT
    } else {
        text.map_or(style.row_height, |text| {
            style.row_height.max(text.size.max(text.line_box()))
        })
    }
}

/// Natural panel width: the widest label plus accelerator (and submenu
/// arrow) with padding, at least `MIN_PANEL_WIDTH`.
pub(crate) fn panel_width<Message, Renderer: text::Renderer>(
    renderer: &Renderer,
    items: &[Item<Message>],
    style: MenuStyle,
    text: TextStyle<Renderer::Font>,
) -> f32 {
    items
        .iter()
        .map(|item| {
            text_width(renderer, &item.label, text)
                + text_width(renderer, &item.accelerator, text)
                + if matches!(item.kind, Kind::Submenu(_)) {
                    text_width(renderer, "  ›", text)
                } else {
                    0.0
                }
                + style.padding * 3.0
        })
        .fold(MIN_PANEL_WIDTH, f32::max)
}

/// The size a panel showing `items` wants, in logical pixels: the style's
/// text size with the renderer's default font, and the style's own row
/// heights without a prepared text style.
fn panel_size_with<Message, Renderer: text::Renderer>(
    renderer: &Renderer,
    items: &[Item<Message>],
    style: MenuStyle,
    text: Option<TextStyle<Renderer::Font>>,
) -> Size {
    let resolved = text.unwrap_or(TextStyle {
        font: renderer.default_font(),
        size: style.text_size,
        line_height: None,
    });
    Size::new(
        panel_width(renderer, items, style, resolved),
        items.iter().map(|item| row_height(item, style, text)).sum(),
    )
}

/// The size a panel showing `items` wants, in logical pixels. Use it to size a
/// popup surface.
pub fn panel_size<Message, Renderer: text::Renderer>(
    renderer: &Renderer,
    items: &[Item<Message>],
    style: MenuStyle,
) -> Size {
    panel_size_with(renderer, items, style, None)
}

/// `panel_size` with a prepared text style: rows are never shorter than its
/// content height. External hosts sharing the menu's typography use this to
/// size a popup surface exactly.
pub fn panel_size_text<Message, Renderer: text::Renderer>(
    renderer: &Renderer,
    items: &[Item<Message>],
    style: MenuStyle,
    text: TextStyle<Renderer::Font>,
) -> Size {
    panel_size_with(renderer, items, style, Some(text))
}

/// Bounds of row `index` in a panel `width` wide, relative to the panel's
/// top-left; `None` past the end. Use it as a submenu's anchor. Rows are the
/// style's own height, or never shorter than a prepared text's content
/// height.
pub(crate) fn row_bounds_with<Message, F: Copy>(
    items: &[Item<Message>],
    index: usize,
    width: f32,
    style: MenuStyle,
    text: Option<TextStyle<F>>,
) -> Option<Rectangle> {
    let item = items.get(index)?;
    let y = items[..index]
        .iter()
        .map(|item| row_height(item, style, text))
        .sum();
    Some(Rectangle::new(
        Point::new(0.0, y),
        Size::new(width, row_height(item, style, text)),
    ))
}

/// Bounds of row `index` in a panel `width` wide, relative to the panel's
/// top-left; `None` past the end. Use it as a submenu's anchor.
pub fn row_bounds<Message>(
    items: &[Item<Message>],
    index: usize,
    width: f32,
    style: MenuStyle,
) -> Option<Rectangle> {
    row_bounds_with::<Message, Font>(items, index, width, style, None)
}

/// `row_bounds` with a prepared text style; the font does not change row
/// heights, the size and line height do.
pub fn row_bounds_text<Message, F: Copy>(
    items: &[Item<Message>],
    index: usize,
    width: f32,
    style: MenuStyle,
    text: TextStyle<F>,
) -> Option<Rectangle> {
    row_bounds_with(items, index, width, style, Some(text))
}

/// The row at panel-relative `y`, if any (separators and disabled rows
/// included; the navigator decides what they do).
pub(crate) fn row_at_with<Message, F: Copy>(
    items: &[Item<Message>],
    y: f32,
    style: MenuStyle,
    text: Option<TextStyle<F>>,
) -> Option<usize> {
    if y < 0.0 {
        return None;
    }
    let mut top = 0.0;
    for (index, item) in items.iter().enumerate() {
        let bottom = top + row_height(item, style, text);
        if y < bottom {
            return Some(index);
        }
        top = bottom;
    }
    None
}

/// The row at panel-relative `y`, if any (separators and disabled rows
/// included; the navigator decides what they do).
pub fn row_at<Message>(items: &[Item<Message>], y: f32, style: MenuStyle) -> Option<usize> {
    row_at_with::<Message, Font>(items, y, style, None)
}

/// `row_at` with a prepared text style; the font does not change row heights,
/// the size and line height do.
pub fn row_at_text<Message, F: Copy>(
    items: &[Item<Message>],
    y: f32,
    style: MenuStyle,
    text: TextStyle<F>,
) -> Option<usize> {
    row_at_with(items, y, style, Some(text))
}

/// Draws a panel of `items` in `bounds`, highlighting `selected`. Rows use
/// the style's own height, or a prepared text style's content height.
pub(crate) fn draw_panel<Message, Renderer: text::Renderer>(
    renderer: &mut Renderer,
    bounds: Rectangle,
    items: &[Item<Message>],
    selected: Option<usize>,
    style: MenuStyle,
    text: Option<TextStyle<Renderer::Font>>,
) {
    if bounds.height <= 0.0 {
        return;
    }
    quad(renderer, bounds, style.background, style);
    let resolved = resolve_text(renderer, style, text);
    let mut y = bounds.y;
    for (index, item) in items.iter().enumerate() {
        let height = row_height(item, style, text);
        let row = Rectangle {
            y,
            height,
            ..bounds
        };
        y += height;
        if matches!(item.kind, Kind::Separator) {
            renderer.fill_quad(
                renderer::Quad {
                    bounds: Rectangle {
                        x: row.x + style.padding,
                        y: row.center_y(),
                        width: (row.width - style.padding * 2.0).max(0.0),
                        height: 1.0,
                    },
                    ..Default::default()
                },
                style.border,
            );
            continue;
        }
        let is_selected = selected == Some(index);
        if is_selected {
            quad(renderer, row, style.selected, style);
        }
        let color = if !item.selectable() {
            style.disabled
        } else if is_selected {
            style.selected_text
        } else {
            style.text
        };
        let text_bounds = Rectangle {
            x: row.x + style.padding,
            width: (row.width - style.padding * 2.0).max(0.0),
            ..row
        };
        label(renderer, &item.label, text_bounds, color, resolved, false);
        let trailing = if matches!(item.kind, Kind::Submenu(_)) {
            format!("{}  ›", item.accelerator)
        } else {
            item.accelerator.clone()
        };
        label(renderer, &trailing, text_bounds, color, resolved, true);
    }
}

/// One menu panel filling its own surface. It reports the pointer and presses
/// as row indices; feed them to `Navigator::hover` and `Navigator::click` for
/// this panel's level, and pass the resulting selection back as `selected`.
pub struct Panel<'a, Message> {
    items: &'a [Item<Message>],
    selected: Option<usize>,
    style: Option<MenuStyle>,
    on_hover: Option<Box<dyn Fn(Option<usize>) -> Message + 'a>>,
    on_press: Option<Box<dyn Fn(usize) -> Message + 'a>>,
}

/// A [`Panel`] with a prepared text style (see [`Panel::text_style`]): rows
/// are never shorter than the prepared content height, and `F` is the host
/// renderer's font.
pub struct StyledPanel<'a, Message, F> {
    panel: Panel<'a, Message>,
    text_style: TextStyle<F>,
}

impl<'a, Message> Panel<'a, Message> {
    /// A panel listing `items` with row `selected` highlighted.
    pub fn new(items: &'a [Item<Message>], selected: Option<usize>) -> Self {
        Self {
            items,
            selected,
            style: None,
            on_hover: None,
            on_press: None,
        }
    }

    /// An explicit style. Without one the colours come from the theme
    /// (`theme::Catalog::menu_style`) over the default row metrics.
    pub fn style(mut self, style: MenuStyle) -> Self {
        self.style = Some(style);
        self
    }

    /// A prepared text style, shared with the `Menu` driving this panel. It
    /// overrides the style's `text_size` regardless of builder order, and
    /// rows are never shorter than the text's content height.
    pub fn text_style<F>(self, text: TextStyle<F>) -> StyledPanel<'a, Message, F> {
        StyledPanel {
            panel: self,
            text_style: text,
        }
    }

    fn metrics(&self) -> MenuStyle {
        self.style.unwrap_or_default()
    }

    /// Published when the row under the pointer changes (`None` when the
    /// pointer leaves the rows).
    pub fn on_hover(mut self, callback: impl Fn(Option<usize>) -> Message + 'a) -> Self {
        self.on_hover = Some(Box::new(callback));
        self
    }

    /// Published for a left press or touch on a row.
    pub fn on_press(mut self, callback: impl Fn(usize) -> Message + 'a) -> Self {
        self.on_press = Some(Box::new(callback));
        self
    }
}

#[derive(Default)]
struct PanelState {
    // The last hover reported; `None` until the first report.
    hovered: Option<Option<usize>>,
    // Identity of the items shown, to forget the hover when they change.
    items: (usize, usize),
}

fn identity<Message>(items: &[Item<Message>]) -> (usize, usize) {
    (items.as_ptr() as usize, items.len())
}

// The panel engine, shared by the legacy [`Panel`] and the prepared
// [`StyledPanel`]: they differ only in the text argument of the geometry,
// and keep the same [`PanelState`] tree.
fn panel_diff<Message>(items: &[Item<Message>], tree: &mut Tree) {
    let state = tree.state.downcast_mut::<PanelState>();
    if state.items != identity(items) {
        *state = PanelState {
            hovered: None,
            items: identity(items),
        };
    }
}

fn panel_layout<Message, Renderer: text::Renderer>(
    renderer: &Renderer,
    items: &[Item<Message>],
    style: MenuStyle,
    text: Option<TextStyle<Renderer::Font>>,
    limits: &layout::Limits,
) -> layout::Node {
    layout::Node::new(limits.resolve(
        Length::Shrink,
        Length::Shrink,
        panel_size_with(renderer, items, style, text),
    ))
}

struct PanelInput<'a, Message, F> {
    items: &'a [Item<Message>],
    selected: Option<usize>,
    style: Option<MenuStyle>,
    text: Option<TextStyle<F>>,
    on_hover: Option<&'a dyn Fn(Option<usize>) -> Message>,
    on_press: Option<&'a dyn Fn(usize) -> Message>,
}

fn panel_update<Message, Renderer: text::Renderer>(
    input: PanelInput<'_, Message, Renderer::Font>,
    tree: &mut Tree,
    event: &Event,
    layout: Layout<'_>,
    cursor: mouse::Cursor,
    shell: &mut Shell<'_, Message>,
) {
    let PanelInput { items, selected, style, text, on_hover, on_press } = input;
    let bounds = layout.bounds();
    let state = tree.state.downcast_mut::<PanelState>();
    let metrics = || style.unwrap_or_default();
    let row_under = |point: Option<Point>| {
        point
            .filter(|point| bounds.contains(*point))
            .and_then(|point| row_at_with(items, point.y - bounds.y, metrics(), text))
    };
    match event {
        Event::Mouse(mouse::Event::CursorMoved { .. } | mouse::Event::CursorLeft)
        | Event::Touch(touch::Event::FingerMoved { .. }) => {
            let point = match event {
                Event::Touch(touch::Event::FingerMoved { position, .. }) => Some(*position),
                _ => cursor.position(),
            };
            let row = row_under(point);
            // Also re-report a row that keyboard navigation moved the
            // selection away from, as the in-surface overlay reselects it.
            let reselect = row != selected && row.is_some_and(|row| items[row].selectable());
            if state.hovered != Some(row) || reselect {
                state.hovered = Some(row);
                if let Some(on_hover) = on_hover {
                    shell.publish(on_hover(row));
                }
                shell.request_redraw();
            }
        }
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
        | Event::Touch(touch::Event::FingerPressed { .. }) => {
            if shell.is_event_captured() {
                return;
            }
            let point = match event {
                Event::Touch(touch::Event::FingerPressed { position, .. }) => Some(*position),
                _ => cursor.position(),
            };
            if let Some(row) = row_under(point) {
                if let Some(on_press) = on_press {
                    shell.publish(on_press(row));
                }
                shell.capture_event();
            }
        }
        _ => {}
    }
}

fn panel_interaction<Message, Renderer: text::Renderer>(
    items: &[Item<Message>],
    style: MenuStyle,
    text: Option<TextStyle<Renderer::Font>>,
    layout: Layout<'_>,
    cursor: mouse::Cursor,
) -> mouse::Interaction {
    let Some(point) = cursor.position_in(layout.bounds()) else {
        return mouse::Interaction::None;
    };
    match row_at_with(items, point.y, style, text) {
        Some(row) if items[row].selectable() => mouse::Interaction::Pointer,
        _ => mouse::Interaction::Idle,
    }
}

impl<Message, Theme: Catalog, Renderer: text::Renderer> Widget<Message, Theme, Renderer>
    for Panel<'_, Message>
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<PanelState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(PanelState {
            hovered: None,
            items: identity(self.items),
        })
    }

    fn diff(&mut self, tree: &mut Tree) {
        panel_diff(self.items, tree);
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Shrink, Length::Shrink)
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        panel_layout(renderer, self.items, self.metrics(), None, limits)
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &Renderer,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        panel_update::<Message, Renderer>(
            PanelInput {
                items: self.items,
                selected: self.selected,
                style: self.style,
                text: None,
                on_hover: self.on_hover.as_deref(),
                on_press: self.on_press.as_deref(),
            },
            tree,
            event,
            layout,
            cursor,
            shell,
        );
    }

    fn draw(
        &self,
        _tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        draw_panel(
            renderer,
            layout.bounds(),
            self.items,
            self.selected,
            resolve(self.style, theme),
            None,
        );
    }

    fn mouse_interaction(
        &self,
        _tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        panel_interaction::<Message, Renderer>(self.items, self.metrics(), None, layout, cursor)
    }
}

impl<Message, Theme: Catalog, Renderer: text::Renderer> Widget<Message, Theme, Renderer>
    for StyledPanel<'_, Message, Renderer::Font>
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<PanelState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(PanelState {
            hovered: None,
            items: identity(self.panel.items),
        })
    }

    fn diff(&mut self, tree: &mut Tree) {
        panel_diff(self.panel.items, tree);
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Shrink, Length::Shrink)
    }

    fn layout(
        &mut self,
        _tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        panel_layout(
            renderer,
            self.panel.items,
            self.panel.metrics(),
            Some(self.text_style),
            limits,
        )
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &Renderer,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        panel_update::<Message, Renderer>(
            PanelInput {
                items: self.panel.items,
                selected: self.panel.selected,
                style: self.panel.style,
                text: Some(self.text_style),
                on_hover: self.panel.on_hover.as_deref(),
                on_press: self.panel.on_press.as_deref(),
            },
            tree,
            event,
            layout,
            cursor,
            shell,
        );
    }

    fn draw(
        &self,
        _tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        draw_panel(
            renderer,
            layout.bounds(),
            self.panel.items,
            self.panel.selected,
            resolve(self.panel.style, theme),
            Some(self.text_style),
        );
    }

    fn mouse_interaction(
        &self,
        _tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        panel_interaction::<Message, Renderer>(
            self.panel.items,
            self.panel.metrics(),
            Some(self.text_style),
            layout,
            cursor,
        )
    }
}

impl<'a, Message: 'a, Theme: Catalog + 'a, Renderer: text::Renderer + 'a> From<Panel<'a, Message>>
    for Element<'a, Message, Theme, Renderer>
{
    fn from(panel: Panel<'a, Message>) -> Self {
        Element::new(panel)
    }
}

impl<'a, Message: 'a, Theme: Catalog + 'a, Renderer: text::Renderer + 'a>
    From<StyledPanel<'a, Message, Renderer::Font>> for Element<'a, Message, Theme, Renderer>
{
    fn from(panel: StyledPanel<'a, Message, Renderer::Font>) -> Self {
        Element::new(panel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items() -> Vec<Item<u8>> {
        vec![
            Item::action("one", 1),
            Item::separator(),
            Item::submenu("more", vec![Item::action("two", 2)]),
        ]
    }

    #[test]
    fn rows_stack_with_separator_height() {
        let items = items();
        let style = MenuStyle::default();
        assert_eq!(
            row_bounds(&items, 0, 200.0, style),
            Some(Rectangle::new(Point::ORIGIN, Size::new(200.0, 28.0)))
        );
        assert_eq!(
            row_bounds(&items, 2, 200.0, style),
            Some(Rectangle::new(
                Point::new(0.0, 36.0),
                Size::new(200.0, 28.0)
            ))
        );
        assert_eq!(row_bounds(&items, 3, 200.0, style), None);
        assert_eq!(row_at(&items, 0.0, style), Some(0));
        assert_eq!(row_at(&items, 27.9, style), Some(0));
        assert_eq!(row_at(&items, 28.0, style), Some(1));
        assert_eq!(row_at(&items, 36.0, style), Some(2));
        assert_eq!(row_at(&items, 64.0, style), None);
        assert_eq!(row_at(&items, -1.0, style), None);
    }

    #[test]
    fn panel_size_has_a_minimum_width_and_sums_rows() {
        let size = panel_size(&(), &items(), MenuStyle::default());
        assert_eq!(size, Size::new(MIN_PANEL_WIDTH, 64.0));
        assert_eq!(
            panel_size::<u8, ()>(&(), &[], MenuStyle::default()).height,
            0.0
        );
    }

    #[test]
    fn text_styled_rows_keep_one_height_and_legacy_helpers_keep_the_style_height() {
        let items = items();
        let style = MenuStyle {
            row_height: 20.0,
            ..MenuStyle::default()
        };
        let text = TextStyle {
            font: iced_core::Font::DEFAULT,
            size: 14.0,
            line_height: Some(30.0),
        };
        // Legacy helpers keep the style's own row height without a prepared
        // text: 20 + 8 (separator) + 20.
        assert_eq!(panel_size(&(), &items, style).height, 48.0);
        assert_eq!(row_at(&items, 19.9, style), Some(0));
        assert_eq!(row_at(&items, 20.0, style), Some(1));
        // A prepared text style drives one height everywhere: rows are never
        // shorter than the 30px content height.
        assert_eq!(row_height(&items[0], style, Some(text)), 30.0);
        assert_eq!(row_height(&items[1], style, Some(text)), SEPARATOR_HEIGHT);
        assert_eq!(panel_size_text(&(), &items, style, text).height, 68.0);
        assert_eq!(
            row_bounds_text(&items, 2, 200.0, style, text).map(|row| row.y),
            Some(38.0)
        );
        // Boundaries: the separator starts at 30 and the third row at 38.
        assert_eq!(row_at_text(&items, 29.9, style, text), Some(0));
        assert_eq!(row_at_text(&items, 30.0, style, text), Some(1));
        assert_eq!(row_at_text(&items, 37.9, style, text), Some(1));
        assert_eq!(row_at_text(&items, 38.0, style, text), Some(2));
        assert_eq!(row_at_text(&items, 67.9, style, text), Some(2));
        assert_eq!(row_at_text(&items, 68.0, style, text), None);
    }

    #[test]
    fn rows_are_never_shorter_than_the_text_size() {
        let items = items();
        let style = MenuStyle {
            row_height: 20.0,
            ..MenuStyle::default()
        };
        // A requested line height smaller than the text size still allocates
        // rows that fit the text: content is max(size, line height).
        let text = TextStyle {
            font: iced_core::Font::DEFAULT,
            size: 40.0,
            line_height: Some(10.0),
        };
        assert_eq!(row_height(&items[0], style, Some(text)), 40.0);
        assert_eq!(panel_size_text(&(), &items, style, text).height, 88.0);
        assert_eq!(
            row_bounds_text(&items, 2, 200.0, style, text).map(|row| row.y),
            Some(48.0)
        );
        // Without a requested line height the 1.3 default factor applies,
        // unlike the legacy rows, which keep the style's own height.
        let text = TextStyle {
            font: iced_core::Font::DEFAULT,
            size: 20.0,
            line_height: None,
        };
        assert_eq!(row_height(&items[0], style, Some(text)), 26.0);
        assert_eq!(row_height::<u8, Font>(&items[0], style, None), 20.0);
    }

    #[test]
    fn panel_layout_draw_and_hover_use_the_supplied_line_box() {
        let items = items();
        let text = TextStyle {
            font: iced_core::Font::DEFAULT,
            size: 14.0,
            line_height: Some(30.0),
        };
        let style = MenuStyle {
            row_height: 20.0,
            ..MenuStyle::default()
        };
        let mut panel = Panel::new(&items, Some(0))
            .style(style)
            .on_hover(|row| row.map_or(99, |row| row as u8))
            .text_style(text);
        let mut tree = Tree {
            tag: tree::Tag::of::<PanelState>(),
            state: tree::State::new(PanelState::default()),
            children: Vec::new(),
        };
        let renderer = crate::test_renderer::LayoutRenderer::new();
        let node = Widget::<u8, iced_core::Theme, crate::test_renderer::LayoutRenderer>::layout(
            &mut panel,
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(200.0, 200.0)),
        );
        assert_eq!(
            node.size().height,
            68.0,
            "rows grow to the 30px content height: 30 + 8 (separator) + 30"
        );
        // Drawing highlights the selected row at the same height the layout
        // used, not the style's 20px row height.
        let mut draw_renderer = crate::test_renderer::LayoutRenderer::new();
        Widget::<u8, iced_core::Theme, crate::test_renderer::LayoutRenderer>::draw(
            &panel,
            &tree,
            &mut draw_renderer,
            &iced_core::Theme::Dark,
            &renderer::Style::default(),
            Layout::new(&node),
            mouse::Cursor::Unavailable,
            &Rectangle::with_size(Size::new(200.0, 200.0)),
        );
        assert_eq!(draw_renderer.quads[0].0.height, 68.0);
        assert_eq!(draw_renderer.quads[1].0.height, 30.0);
        // Pointer hit testing maps y with the same boundaries: at 25 the
        // pointer is still over row 0 (the legacy 20px rows would put the
        // separator there).
        let mut messages = iced_core::shell::Bus::new();
        let mut shell = Shell::new(
            &iced_core::window::Headless,
            iced_core::shell::Waker::noop(),
            &mut messages,
        );
        Widget::<u8, iced_core::Theme, crate::test_renderer::LayoutRenderer>::update(
            &mut panel,
            &mut tree,
            &Event::Mouse(mouse::Event::CursorMoved {
                position: Point::new(5.0, 25.0),
            }),
            Layout::new(&node),
            mouse::Cursor::Available(Point::new(5.0, 25.0)),
            &renderer,
            &mut shell,
            &Rectangle::with_size(Size::new(200.0, 200.0)),
        );
        assert_eq!(messages.into_iter().collect::<Vec<_>>(), [0]);
    }

    #[test]
    fn panel_reports_hover_changes_once_and_presses() {
        let items = items();
        let mut panel = Panel::new(&items, Some(2))
            .on_hover(|row| row.map_or(99, |row| row as u8))
            .on_press(|row| 100 + row as u8);
        let mut tree = Tree {
            tag: tree::Tag::of::<PanelState>(),
            state: tree::State::new(PanelState::default()),
            children: Vec::new(),
        };
        let node = layout::Node::new(Size::new(200.0, 64.0));
        let mut send = |event: Event, at: Option<Point>| {
            let mut messages = iced_core::shell::Bus::new();
            let mut shell = Shell::new(
                &iced_core::window::Headless,
                iced_core::shell::Waker::noop(),
                &mut messages,
            );
            Widget::<u8, iced_core::Theme, ()>::update(
                &mut panel,
                &mut tree,
                &event,
                Layout::new(&node),
                at.map_or(mouse::Cursor::Unavailable, mouse::Cursor::Available),
                &(),
                &mut shell,
                &Rectangle::with_size(Size::INFINITE),
            );
            messages.into_iter().collect::<Vec<_>>()
        };
        let moved = |x, y| {
            Event::Mouse(mouse::Event::CursorMoved {
                position: Point::new(x, y),
            })
        };
        assert_eq!(send(moved(5.0, 40.0), Some(Point::new(5.0, 40.0))), [2]);
        assert!(send(moved(6.0, 41.0), Some(Point::new(6.0, 41.0))).is_empty());
        assert_eq!(send(moved(5.0, 5.0), Some(Point::new(5.0, 5.0))), [0]);
        // The host has not selected row 0 yet (it still passes Some(2)), so
        // further motion over row 0 asks again, as the overlay would.
        assert_eq!(send(moved(6.0, 6.0), Some(Point::new(6.0, 6.0))), [0]);
        assert_eq!(send(Event::Mouse(mouse::Event::CursorLeft), None), [99]);
        assert_eq!(
            send(
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                Some(Point::new(5.0, 30.0))
            ),
            [101]
        );
        assert!(
            send(
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                Some(Point::new(5.0, 300.0))
            )
            .is_empty()
        );
        assert_eq!(
            send(
                Event::Touch(touch::Event::FingerPressed {
                    id: touch::Finger(0),
                    position: Point::new(5.0, 40.0),
                }),
                None
            ),
            [102]
        );
    }
}

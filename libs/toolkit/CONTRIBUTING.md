# Contributing widgets to `toolkit`

Everything in this crate is generic over the host's `Theme` and `Renderer`
and styles itself from `Tokens` — never from literals (the colour gate in
`tests/colours.rs` enforces the colour half). These two templates, adapted
from iced's `examples/custom_widget` and `examples/custom_quad`, are the
minimum shape of a widget that belongs here. Real widgets add state (a
`widget::Tree` entry via `tag`/`state`), events and operations — look at
`src/elide.rs` for a stateful text widget and `src/virtual_list.rs` for the
full ceremony.

## A widget that draws its own pixels

The smallest useful custom widget: fix a size in `layout`, paint quads (or
paragraphs, or layers) in `draw`.

```rust
use iced_core::Length;
use iced_core::layout::{self, Layout};
use iced_core::mouse;
use iced_core::renderer;
use iced_core::widget::{Widget, tree};
use iced_core::{Element, Rectangle, Size};

pub struct Circle {
    radius: f32,
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer> for Circle
where
    Renderer: renderer::Renderer, // take iced_core::text::Renderer to shape text
{
    fn size(&self) -> Size<Length> {
        Size { width: Length::Shrink, height: Length::Shrink }
    }

    fn layout(
        &mut self,
        _tree: &mut tree::Tree,
        _renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let side = self.radius * 2.0;
        layout::Node::new(limits.resolve(Length::Shrink, Length::Shrink, Size::new(side, side)))
    }

    fn draw(
        &self,
        _tree: &tree::Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        _defaults: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        renderer.fill_quad(
            renderer::Quad {
                bounds: layout.bounds(),
                border: iced_core::border::rounded(self.radius),
                ..renderer::Quad::default()
            },
            // Colours arrive through the theme's tokens; the gate refuses
            // literals outside src/tokens.rs.
            theme_color_from_tokens(theme),
        );
    }
}

impl<Message, Theme, Renderer> From<Circle> for Element<'_, Message, Theme, Renderer>
where
    Renderer: renderer::Renderer,
{
    fn from(circle: Circle) -> Self {
        Element::new(circle)
    }
}
```

## A quad-based widget driven by iced's own styling

When the drawing is only flat rounded boxes, implement a `Catalog` so the
host theme styles the widget the way it styles iced's own —
`src/theme.rs` collects one `Catalog` per widget kind. This is the shape
of `examples/custom_quad`, reduced to its contract:

```rust
/// The styling contract: the host theme supplies the appearance.
pub struct Style {
    pub background: iced_core::background::Background,
    pub border: iced_core::border::Border,
}

pub trait Catalog: Sized {
    /// The named variants a host can choose from (`Class::Primary`…);
    /// `Box<dyn Fn(&Self) -> Style>` is the escape hatch.
    type Class<'a>;

    fn default<'a>() -> Self::Class<'a>;
    fn style(&self, class: &Self::Class<'_>) -> Style;
}
```

Implement `Catalog` for `toolkit::theme::Theme` (mapping tokens to the
style) and for `iced_core::Theme` (a neutral fallback), take colours from
`Style` in `draw`, and the widget is themeable by every host. `src/focus.rs`
supplies the `:focus-visible` ring for focusable widgets; `src/tips.rs`
the tooltip helpers; `tests/` wants the widget on a gallery page and a
unit test beside it.

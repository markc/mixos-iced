// SPDX-License-Identifier: MIT OR Apache-2.0
//! A sortable header sharing the listing's responsive column geometry.
use iced_core::text::{self, Paragraph as _};
use iced_core::{Element, Event, Length, Point, Rectangle, Size, Color, Widget, Layout,
    Shell, layout, mouse, renderer, widget::{Tree, tree}};
use super::{Columns, FilePane, Presentation};
pub struct Header<'a, Message, Theme, Renderer> {
    look: Presentation, sort: usize, ascending: bool,
    titles: [String; 3], labels: [String; 3],
    tips: Vec<Element<'a, Message, Theme, Renderer>>,
    columns: Columns, background: Color, foreground: Color,
    on_sort: Box<dyn Fn(usize) -> Message + 'a>,
    tooltip: Option<Box<dyn Fn(String, Size) -> Element<'a, Message, Theme, Renderer> + 'a>>,
}
impl<'a, Message, Theme, Renderer> Header<'a, Message, Theme, Renderer>
where Renderer: text::Renderer<Font = iced_core::Font> + 'static {
    pub fn new(look: Presentation, columns: Columns, titles: [String; 3], sort: usize,
        ascending: bool, on_sort: impl Fn(usize) -> Message + 'a) -> Self {
        Self { look, columns, labels: titles.clone(), titles, sort, ascending,
            tips: Vec::new(), on_sort: Box::new(on_sort), tooltip: None,
            background: look.tokens.palette.muted_surface, foreground: look.tokens.palette.muted_text }
    }
    pub fn colours(mut self, background: Color, foreground: Color) -> Self {
        self.background = background; self.foreground = foreground; self
    }
    pub fn tooltip(mut self, labels: [String; 3], build: impl Fn(String, Size) -> Element<'a, Message, Theme, Renderer> + 'a) -> Self {
        self.labels = labels; self.tooltip = Some(Box::new(build)); self
    }
    fn labels(&self) -> [Renderer::Paragraph; 3] {
        std::array::from_fn(|sort| {
            let label = &self.titles[sort];
            let label = if sort == self.sort {
                format!("{label} {}", if self.ascending { "↑" } else { "↓" })
            } else {
                label.clone()
            };
            FilePane::<Theme, Renderer>::shape(&label, self.look.ui_font, self.look.small_px)
        })
    }
}
impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer> for Header<'_, Message, Theme, Renderer>
where Renderer: text::Renderer<Font = iced_core::Font> + 'static {
    fn diff(&mut self, _tree: &mut Tree) {
        // Region children are reconciled in layout, once widths are known.
    }
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<[Renderer::Paragraph; 3]>()
    }
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Shrink)
    }
    fn state(&self) -> tree::State {
        tree::State::new(self.labels())
    }
    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let labels = self.labels();
        let height = labels[0].min_bounds().height + self.look.chrome.small * 2.0;
        *tree.state.downcast_mut::<[Renderer::Paragraph; 3]>() = labels;
        let size = limits.resolve(Length::Fill, Length::Shrink, Size::new(0.0, height));
        let regions: Vec<(Rectangle, String)> = self
            .columns
            .cells(size.width)
            .into_iter()
            .zip(&self.labels)
            .filter(|((_, width), _)| *width > 0.0)
            .map(|((x, width), label)| {
                (
                    Rectangle {
                        x,
                        y: 0.0,
                        width,
                        height,
                    },
                    label.clone(),
                )
            })
            .collect();
                self.tips = match &self.tooltip {
            Some(build) => regions.iter().map(|(bounds, label)| build(label.clone(), bounds.size())).collect(),
            None => Vec::new(),
        };
        tree.diff_children(&mut self.tips);
        let children = self.tips.iter_mut().zip(&mut tree.children).zip(regions)
            .map(|((child, state), (bounds, _))| child.as_widget_mut()
                .layout(state, renderer, &layout::Limits::new(Size::ZERO, bounds.size()))
                .move_to(bounds.position())).collect();
        layout::Node::with_children(size, children)

    }
    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let hover = if bounds
            .intersection(viewport)
            .is_some_and(|clip| cursor.is_over(clip))
        {
            cursor
        } else {
            mouse::Cursor::Unavailable
        };
        for ((tip, state), child) in self
            .tips
            .iter_mut()
            .zip(&mut tree.children)
            .zip(layout.children())
        {
            tip.as_widget_mut()
                .update(state, event, child, hover, renderer, shell, viewport);
        }
        if matches!(
            event,
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
        ) && bounds
            .intersection(viewport)
            .is_some_and(|clip| cursor.is_over(clip))
        {
            let x = cursor.position().unwrap_or_default().x - bounds.x;
            if let Some(i) = self
                .columns
                .cells(bounds.width)
                .iter()
                .position(|(start, width)| x >= *start && x < start + width)
            {
                shell.publish((self.on_sort)(i));
                shell.capture_event();
            }
        }
    }
    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        _: &Theme,
        _: &renderer::Style,
        layout: Layout<'_>,
        _: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let Some(clip) = bounds.intersection(viewport) else {
            return;
        };
        renderer.with_layer(clip, |renderer| {
            renderer.fill_quad(
                renderer::Quad {
                    bounds,
                    ..Default::default()
                },
                self.background,
            );
            for (i, ((start, width), para)) in self
                .columns
                .cells(bounds.width)
                .into_iter()
                .zip(tree.state.downcast_ref::<[Renderer::Paragraph; 3]>())
                .enumerate()
            {
                if width <= 0.0 {
                    continue;
                }
                let cell = Rectangle {
                    x: bounds.x + start,
                    width,
                    ..bounds
                };
                let x = if i == 0 {
                    cell.x
                } else {
                    cell.x + cell.width - para.min_bounds().width
                };
                renderer.with_layer(cell, |renderer| {
                    renderer.fill_paragraph(
                        para,
                        Point::new(x, bounds.y + self.look.chrome.small),
                        self.foreground,
                        cell,
                    )
                });
            }
        });
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: iced_core::Vector,
    ) -> Option<
        iced_core::overlay::Element<'b, Message, Theme, Renderer>,
    > {
        iced_core::overlay::from_children(
            &mut self.tips,
            tree,
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}
impl<'a, M: 'a, T: 'a, R: text::Renderer<Font = iced_core::Font> + 'static> From<Header<'a, M, T, R>> for Element<'a, M, T, R> { fn from(header: Header<'a, M, T, R>) -> Self { Element::new(header) } }

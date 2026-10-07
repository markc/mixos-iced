// SPDX-License-Identifier: MIT OR Apache-2.0
//! The external contract of the selection list rows and menu panels: the
//! original `List` literal and the renderer-neutral `Panel` compile and
//! behave unchanged, and the prepared wrappers grow rows consistently.
use std::marker::PhantomData;

use toolkit::core::{
    Background, Color, Font, Pixels, Point, Rectangle, Size, Transformation,
    layout::Limits,
    renderer, text, widget::Tree,
};
use toolkit::menu::{Item, MenuStyle, Panel, panel_size_text};
use toolkit::selection_list::{self, List, SelectionList};
use toolkit::typography::TextStyle;

#[derive(Default)]
struct Recorder {
    texts: Vec<text::Text<String>>,
}

impl renderer::Renderer for Recorder {
    fn start_layer(&mut self, _: Rectangle) {}
    fn end_layer(&mut self) {}
    fn start_transformation(&mut self, _: Transformation) {}
    fn end_transformation(&mut self) {}
    fn hint(&mut self, _: renderer::Scale) {}
    fn scale(&self) -> Option<renderer::Scale> {
        None
    }
    fn settings(&self) -> renderer::Settings {
        renderer::Settings::default()
    }
    fn fill_quad(&mut self, _: renderer::Quad, _: impl Into<Background>) {}
    fn reset(&mut self, _: Rectangle) {}
    fn allocate_image(
        &mut self,
        handle: &toolkit::core::image::Handle,
        callback: impl FnOnce(
            Result<toolkit::core::image::Allocation, toolkit::core::image::Error>,
        ) + Send
        + 'static,
    ) {
        renderer::Renderer::allocate_image(&mut (), handle, callback);
    }
}

impl text::Renderer for Recorder {
    type Font = Font;
    type Paragraph = ();
    type Editor = ();
    const ICON_FONT: Font = Font::DEFAULT;
    const CHECKMARK_ICON: char = 'x';
    const ARROW_DOWN_ICON: char = 'v';
    const SCROLL_UP_ICON: char = '^';
    const SCROLL_DOWN_ICON: char = 'v';
    const SCROLL_LEFT_ICON: char = '<';
    const SCROLL_RIGHT_ICON: char = '>';
    const ICED_LOGO: char = 'i';
    fn default_font(&self) -> Font {
        Font::DEFAULT
    }
    fn default_size(&self) -> Pixels {
        Pixels(14.0)
    }
    fn fill_paragraph(&mut self, _: &(), _: Point, _: Color, _: Rectangle) {}
    fn fill_editor(&mut self, _: &(), _: Point, _: Color, _: Rectangle) {}
    fn fill_text(&mut self, text: text::Text<String>, _: Point, _: Color, _: Rectangle) {
        self.texts.push(text);
    }
}

fn limits() -> Limits {
    Limits::new(Size::ZERO, Size::new(200.0, 300.0))
}
#[test]
fn the_original_list_literal_compiles_and_lays_out_unchanged() {
    let options = vec!["a".to_owned(), "b".to_owned()];
    let class = <toolkit::core::Theme as selection_list::Catalog>::default();
    let on_selected = |_: usize, _: String| String::new();
    // The original external literal: exactly the old fields.
    let list: List<'_, '_, String, String, toolkit::core::Theme, Recorder> = List {
        options: &options,
        font: Font::DEFAULT,
        class: &class,
        on_selected: &on_selected,
        padding: 5.0.into(),
        text_size: 12.0,
        selected: None,
        phantomdata: PhantomData,
    };
    let mut tree = Tree::new(&list as &dyn toolkit::core::Widget<String, toolkit::core::Theme, Recorder>);
    let mut list = list;
    list.diff(&mut tree);
    let renderer = Recorder::default();
    let node = toolkit::core::Widget::layout(&mut list, &mut tree, &renderer, &limits());
    // Legacy rows: the text size plus the vertical padding.
    assert_eq!(node.size().height, (12.0 + 10.0) * 2.0);
    // The consuming builders return the prepared wrapper, whose rows grow.
    let mut styled = list.line_height(30.0);
    let mut tree = Tree::new(&styled as &dyn toolkit::core::Widget<String, toolkit::core::Theme, Recorder>);
    styled.diff(&mut tree);
    let node = toolkit::core::Widget::layout(&mut styled, &mut tree, &renderer, &limits());
    assert_eq!(node.size().height, (30.0 + 10.0) * 2.0);
}

#[test]
fn panels_remain_renderer_neutral_and_the_styled_forms_agree() {
    let items = vec![
        Item::action("one", 1),
        Item::separator(),
        Item::submenu("more", vec![Item::action("two", 2)]),
    ];
    let renderer = Recorder::default();
    // The legacy panel takes no font parameter.
    let panel: Panel<'_, u8> = Panel::new(&items, None);
    let mut panel = panel;
    let mut tree = Tree::new(&panel as &dyn toolkit::core::Widget<u8, toolkit::core::Theme, Recorder>);
    panel.diff(&mut tree);
    let node = toolkit::core::Widget::layout(&mut panel, &mut tree, &renderer, &limits());
    assert_eq!(node.size().height, 64.0);
    // The styled panel grows rows to the prepared content height and matches
    // the typed panel size the external host uses for its popup surface.
    let text = TextStyle {
        font: Font::DEFAULT,
        size: 14.0,
        line_height: Some(30.0),
    };
    let mut styled = panel.text_style(text);
    let mut tree = Tree::new(&styled as &dyn toolkit::core::Widget<u8, toolkit::core::Theme, Recorder>);
    styled.diff(&mut tree);
    let node = toolkit::core::Widget::layout(&mut styled, &mut tree, &renderer, &limits());
    assert_eq!(
        node.size(),
        panel_size_text(&renderer, &items, MenuStyle::default(), text)
    );
}

#[test]
fn the_selection_list_wraps_the_original_list_with_the_prepared_style() {
    let options = vec!["a".to_owned(), "b".to_owned()];
    let renderer = Recorder::default();
    let legacy: SelectionList<'_, String, &[String], String, toolkit::core::Theme, Recorder> =
        SelectionList::new(&options, |_, value: String| value);
    let mut tree = Tree::new(&legacy as &dyn toolkit::core::Widget<String, toolkit::core::Theme, Recorder>);
    let mut legacy = legacy;
    legacy.diff(&mut tree);
    let legacy_height =
        toolkit::core::Widget::layout(&mut legacy, &mut tree, &renderer, &limits())
            .size()
            .height;
    // The prepared rows are taller than the legacy text-size rows.
    let styled: SelectionList<'_, String, &[String], String, toolkit::core::Theme, Recorder> =
        SelectionList::new(&options, |_, value: String| value).line_height(30.0);
    let mut tree = Tree::new(&styled as &dyn toolkit::core::Widget<String, toolkit::core::Theme, Recorder>);
    let mut styled = styled;
    styled.diff(&mut tree);
    let styled_height =
        toolkit::core::Widget::layout(&mut styled, &mut tree, &renderer, &limits())
            .size()
            .height;
    assert!(styled_height > legacy_height);
}

// SPDX-License-Identifier: MIT OR Apache-2.0
//! Stateful compositions: palette, requester, table, overlays and drag/drop.
use super::strings::label;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use toolkit::command_palette::{self, Command};
use toolkit::dnd;
use toolkit::requester;
use toolkit::{
    Theme, Tokens,
    iced::{
        Element,
        widget::{button, column, container, row, text},
    },
};

#[derive(Debug, Clone)]
pub enum Message {
    Typed(u32),
    Sidebar(usize),
    Tab(usize),
    CloseTab(usize),
    Collapse(bool),
    Popover(bool),
    Split(f32),
    PaletteOpen,
    Palette(command_palette::Event<usize>),
    PaletteMove(i32),
    Request(requester::Event),
    SubmitRequest,
    Drag(dnd::Finished<String>),
    ColumnDrag(usize, f32),
    ColumnRelease,
    Sync(toolkit::iced::widget::scrollable::AbsoluteOffset),
}
impl From<requester::Event> for Message {
    fn from(event: requester::Event) -> Self {
        Self::Request(event)
    }
}

struct DemoFs;
impl requester::Filesystem for DemoFs {
    fn list(&self, _: &Path, hidden: bool) -> std::io::Result<(Vec<requester::Entry>, bool)> {
        let mut entries = vec![
            requester::Entry {
                name: "notes.txt".into(),
                dir: false,
            },
            requester::Entry {
                name: ".hidden".into(),
                dir: false,
            },
        ];
        entries.retain(|entry| hidden || !entry.name.starts_with('.'));
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok((entries, false))
    }
    fn is_dir(&self, path: &Path) -> bool {
        path == Path::new("/documents") || path == Path::new("/")
    }
    fn exists(&self, path: &Path) -> bool {
        self.is_dir(path) || path == Path::new("/documents/notes.txt")
    }
    fn home(&self) -> Option<PathBuf> {
        Some("/documents".into())
    }
}

struct Column {
    title: String,
    width: f32,
    preview: Option<f32>,
}
impl<'a> toolkit::table::Column<'a, Message, Theme, toolkit::iced::Renderer> for Column {
    type Row = (String, String);
    fn header(&'a self, _: usize) -> Element<'a, Message, Theme> {
        text(&self.title).into()
    }
    fn cell(&'a self, index: usize, _: usize, row: &'a Self::Row) -> Element<'a, Message, Theme> {
        text(if index == 0 { &row.0 } else { &row.1 }).into()
    }
    fn width(&self) -> f32 {
        self.width
    }
    fn resize_offset(&self) -> Option<f32> {
        self.preview
    }
}

pub struct State {
    typed: u32,
    sidebar: usize,
    tab: usize,
    tabs: Vec<usize>,
    collapsed: bool,
    popover: bool,
    split: f32,
    palette: bool,
    query: String,
    selection: Option<usize>,
    commands: Vec<Command<usize>>,
    request: requester::Requester,
    request_strings: requester::Strings,
    drag: dnd::Shared<String>,
    drag_labels: dnd::Labels,
    columns: Vec<Column>,
    rows: Vec<(String, String)>,
    outcome: String,
    typed_hint: String,
    palette_hint: String,
    #[cfg(feature = "image")]
    assets: toolkit::icons::Assets,
    #[cfg(feature = "image")]
    icon: toolkit::icons::Ready,
}
impl State {
    pub fn new() -> Self {
        #[cfg(feature = "image")]
        let assets = toolkit::icons::Assets::new().fallback(
            "gallery-vector",
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fallback.svg"),
            true,
        );
        #[cfg(feature = "image")]
        let icon = assets.resolve(
            toolkit::icon("gallery-vector"),
            32.0,
            2.0,
            Tokens::dark().palette.text,
        );
        Self {
            typed: 42,
            sidebar: 0,
            tab: 0,
            tabs: (0..20).collect(),
            collapsed: true,
            popover: false,
            split: 180.0,
            palette: false,
            query: String::new(),
            selection: Some(0),
            commands: (0..20)
                .map(|index| {
                    Command::new(format!("{} {index:02}", label("flows-command")), index)
                        .keyword(format!("task {index}"))
                })
                .collect(),
            request: requester::Requester::new(
                requester::Mode::Save,
                "/documents".into(),
                vec![],
                Arc::new(DemoFs),
            )
            .with_name("notes.txt"),
            request_strings: requester::Strings {
                placeholder: label("flows-path-hint"),
                show_hidden: label("flows-show-hidden"),
                hide_hidden: label("flows-hide-hidden"),
                truncated: label("flows-truncated"),
                recent: label("flows-recent"),
            },
            drag: Arc::new(std::sync::Mutex::new(dnd::State::default())),
            drag_labels: dnd::Labels {
                copy: label("flows-copy"),
                r#move: label("flows-move"),
                cancel: label("flows-cancel"),
            },
            columns: vec![
                Column {
                    title: label("flows-name"),
                    width: 240.0,
                    preview: None,
                },
                Column {
                    title: label("flows-value"),
                    width: 180.0,
                    preview: None,
                },
            ],
            rows: (0..30)
                .map(|i| (format!("{} {i}", label("flows-row")), format!("{}", i * 10)))
                .collect(),
            outcome: String::new(),
            typed_hint: label("flows-typed"),
            palette_hint: label("flows-palette-hint"),
            #[cfg(feature = "image")]
            assets,
            #[cfg(feature = "image")]
            icon,
        }
    }
    pub fn retheme(&mut self, tokens: Tokens) {
        #[cfg(feature = "image")]
        {
            self.icon = self.assets.resolve(
                toolkit::icon("gallery-vector"),
                tokens.metrics.text.xxl,
                2.0,
                tokens.palette.text,
            );
        }
        #[cfg(not(feature = "image"))]
        let _ = tokens;
    }
    pub fn update(&mut self, message: Message) {
        match message {
            Message::Typed(value) => self.typed = value,
            Message::Sidebar(index) => self.sidebar = index,
            Message::Tab(index) => self.tab = index,
            Message::CloseTab(index) => {
                self.tabs.retain(|tab| *tab != index);
                if self.tab == index {
                    self.tab = self.tabs.first().copied().unwrap_or(0);
                }
            }
            Message::Collapse(open) => self.collapsed = open,
            Message::Popover(open) => self.popover = open,
            Message::Split(width) => self.split = width,
            Message::PaletteOpen => {
                self.palette = true;
                self.query.clear();
                self.selection = Some(0);
            }
            Message::Palette(command_palette::Event::Dismissed) => self.palette = false,
            Message::Palette(command_palette::Event::QueryChanged(query)) => {
                self.query = query;
                self.selection =
                    (!command_palette::filter(&self.query, &self.commands).is_empty()).then_some(0);
            }
            Message::Palette(command_palette::Event::Activated(index)) => {
                self.outcome = format!("{} {index}", label("flows-activated"));
                self.palette = false;
            }
            Message::PaletteMove(delta) => {
                self.selection = command_palette::move_selection(
                    self.selection,
                    delta,
                    command_palette::filter(&self.query, &self.commands).len(),
                )
            }
            Message::Request(event) => {
                if let Some(outcome) = self.request.update(event) {
                    self.outcome = format!("{outcome:?}");
                }
            }
            Message::SubmitRequest => {
                if let Some(outcome) = self.request.update(requester::Event::Submit) {
                    self.outcome = format!("{outcome:?}");
                }
            }
            Message::Drag(finished) => {
                self.outcome = format!("{}: {:?}", finished.payload, finished.choice)
            }
            Message::ColumnDrag(index, offset) => {
                if let Some(column) = self.columns.get_mut(index) {
                    column.preview = Some(offset);
                }
            }
            Message::ColumnRelease => {
                for column in &mut self.columns {
                    column.width = (column.width + column.preview.take().unwrap_or(0.0)).max(4.0);
                }
            }
            Message::Sync(offset) => {
                let _ = offset;
            }
        }
    }
    pub fn view(&self, tokens: Tokens) -> Element<'_, Message, Theme> {
        let gap = tokens.metrics.spacing.md;
        let sidebar = toolkit::sidebar::Sidebar::new(Message::Sidebar)
            .push(0, toolkit::tab_bar::TabLabel::Text(label("flows-first")))
            .push(1, toolkit::tab_bar::TabLabel::Text(label("flows-second")))
            .set_active_tab(&self.sidebar)
            .width(160)
            .height(100);
        let typed = toolkit::typed_input::TypedInput::new(&self.typed_hint, &self.typed)
            .on_input(Message::Typed)
            .width(160);
        let flush = toolkit::flush_column::FlushColumn::new()
            .spacing(gap)
            .push(button(text(label("flows-palette"))).on_press(Message::PaletteOpen))
            .push(button(text(label("flows-submit"))).on_press(Message::SubmitRequest));
        let tabs = toolkit::tab_bar::TabBar::with_tab_labels(
            self.tabs
                .iter()
                .map(|index| {
                    (
                        *index,
                        toolkit::tab_bar::TabLabel::Text(format!("{} {index}", label("flows-tab"))),
                    )
                })
                .collect(),
            Message::Tab,
        )
        .set_active_tab(&self.tab)
        .on_close(Message::CloseTab)
        .tab_width(120.into())
        .scrollable();
        let popover = toolkit::popover::Popover::new(
            button(text(label("flows-popover"))).on_press(Message::Popover(!self.popover)),
            container(text(label("flows-popover-body")))
                .padding(gap)
                .style(toolkit::theme::container::popover),
            self.popover,
        )
        .on_dismiss(Message::Popover(false));
        let collapsible = toolkit::collapsible::Collapsible::new(
            label("flows-collapse"),
            self.collapsed,
            Message::Collapse,
        )
        .body(text(label("flows-collapse-body")))
        .padding(gap)
        .text_size(tokens.metrics.text.md);
        let split = toolkit::split::Split::new(
            self.split,
            container(text(label("flows-first"))).width(toolkit::core::Length::Fill),
            container(text(label("flows-second"))).width(toolkit::core::Length::Fill),
        )
        .strategy(toolkit::split::Strategy::Start)
        .on_drag(Message::Split);
        let table = toolkit::table::table(
            toolkit::core::widget::Id::new("flows-table-header"),
            toolkit::core::widget::Id::new("flows-table-body"),
            &self.columns,
            &self.rows,
            Message::Sync,
        )
        .on_column_resize(Message::ColumnDrag, Message::ColumnRelease);
        let drop = dnd::DropArea::new(
            container(text(label("flows-drop")))
                .padding(gap)
                .width(180)
                .height(60),
            self.drag.clone(),
        )
        .tokens(tokens);
        let drag = dnd::DragArea::new(
            container(text(label("flows-drag")))
                .padding(gap)
                .width(180)
                .height(60),
            label("flows-payload"),
        )
        .label(Clone::clone)
        .start_directly(self.drag.clone());
        #[cfg(feature = "image")]
        let icon = self.icon.clone().view();
        #[cfg(not(feature = "image"))]
        let icon: Element<'_, Message, Theme> =
            toolkit::icon(label("flows-vector-fallback")).into();
        let body = column![
            text(label("flows-heading")).size(tokens.metrics.text.xxl),
            row![
                sidebar,
                column![text(label("flows-typed")), typed],
                flush,
                icon
            ]
            .spacing(gap),
            tabs,
            collapsible,
            popover,
            row![
                toolkit::spinners::Circular::new(),
                toolkit::spinners::Linear::new()
            ]
            .spacing(gap),
            container(split).height(90),
            container(table).height(200),
            row![drag, drop].spacing(gap),
            text(label("flows-requester")),
            self.request.view::<Message>(tokens, &self.request_strings),
            text(&self.outcome),
        ]
        .spacing(gap)
        .width(toolkit::core::Length::Fill);
        dnd::Layer::new(body, self.drag.clone(), tokens, Message::Drag)
            .labels(self.drag_labels.clone())
            .into()
    }
    pub fn wrap<'a>(
        &'a self,
        base: Element<'a, super::Message, Theme>,
        tokens: Tokens,
    ) -> Element<'a, super::Message, Theme> {
        let layer = self.palette.then(|| {
            let palette = command_palette::command_palette_with_placeholder(
                &self.query,
                self.selection,
                &self.commands,
                &tokens,
                &self.palette_hint,
            )
            .map(Message::Palette);
            toolkit::keys::keys(palette, |_| None)
                .on_key_before_focused(
                    toolkit::core::widget::Id::new(command_palette::INPUT_ID),
                    |event| {
                        use toolkit::core::keyboard::{Event, Key, key::Named};
                        match event {
                            Event::KeyPressed {
                                key: Key::Named(Named::ArrowDown),
                                ..
                            } => Some(Message::PaletteMove(1)),
                            Event::KeyPressed {
                                key: Key::Named(Named::ArrowUp),
                                ..
                            } => Some(Message::PaletteMove(-1)),
                            _ => None,
                        }
                    },
                )
                .into()
        });
        let layer: Option<Element<'a, Message, Theme>> = layer;
        toolkit::dialog::Modal::host(base, layer.map(|layer| layer.map(super::Message::Flows)))
            .focus(
                self.palette
                    .then(|| toolkit::core::widget::Id::new(command_palette::INPUT_ID)),
            )
            .into()
    }
}

// SPDX-License-Identifier: MIT OR Apache-2.0
//! Portable panes with a small string document, synthetic listing and injected
//! terminal surface. Production engines bind to the same neutral interfaces.
#[path = "document.rs"]
mod document;
#[path = "../gallery/strings.rs"]
mod strings;
use std::path::{Path, PathBuf};
use toolkit::iced::widget::{button, column, container, row, text};
use toolkit::iced::{self, Element, Length, Size};
use toolkit::{EditorPane, FilePane, TerminalPane, Theme, Tokens};
use toolkit::{editor_pane as editor, file_pane as file};

#[derive(Clone, Debug)]
pub enum Message {
    Editor(editor::Message),
    File(file::Message),
    Split(f32),
    Theme,
    Sort(usize),
    Wheel(iced::mouse::ScrollDelta),
}
pub struct Demo {
    document: document::Text,
    palette: editor::Palette,
    tokens: Tokens,
    files: Vec<(PathBuf, String)>,
    selected: Option<PathBuf>,
    split: f32,
    notice: String,
    wheel: f32,
    dark: bool,
}
struct Listing<'a> {
    files: &'a [(PathBuf, String)],
    selected: Option<&'a Path>,
}
impl file::Source for Listing<'_> {
    fn root(&self) -> &Path {
        Path::new("/example")
    }
    fn len(&self) -> usize {
        self.files.len()
    }
    fn row(&self, index: usize) -> Option<file::Row<'_>> {
        self.files.get(index).map(|(path, name)| file::Row {
            path,
            name,
            depth: 0,
            is_dir: false,
        })
    }
    fn selected(&self) -> Option<&Path> {
        self.selected
    }
    fn size_text(&self, index: usize) -> String {
        format!("{} KiB", index % 100)
    }
    fn modified_text(&self, _index: usize) -> String {
        "01/01/26 12:00".into()
    }
}
impl Demo {
    pub fn new() -> Self {
        let tokens = Tokens::dark();
        Self {
            document: document::Text::from_text(&strings::label("compound-document")).unwrap(),
            palette: tokens.into(),
            tokens,
            files: (0..100_000)
                .map(|index| {
                    (
                        PathBuf::from(format!("/example/{index}")),
                        format!("file-{index}.txt"),
                    )
                })
                .collect(),
            selected: None,
            split: 0.42,
            notice: strings::label("compound-files-count"),
            wheel: 0.0,
            dark: true,
        }
    }
    pub fn update(&mut self, message: Message) {
        match message {
            Message::Editor(message) => self.document.apply(message),
            Message::File(
                file::Message::Select(path) | file::Message::SelectModified(path, _, _),
            ) => self.selected = Some(path),
            Message::File(message) => self.notice = format!("{message:?}"),
            Message::Split(split) => self.split = split.clamp(0.15, 0.85),
            Message::Sort(column) => {
                self.notice = strings::format("compound-sort", &[("column", column.to_string())])
            }
            Message::Theme => {
                self.dark = !self.dark;
                self.tokens = if self.dark {
                    Tokens::dark()
                } else {
                    Tokens::light()
                };
                self.palette = self.tokens.into();
            }
            Message::Wheel(delta) => {
                self.wheel += match delta {
                    iced::mouse::ScrollDelta::Lines { y, .. } => y,
                    iced::mouse::ScrollDelta::Pixels { y, .. } => y / 20.0,
                }
            }
        }
    }
    pub fn theme(&self) -> Theme {
        Theme::new(self.tokens)
    }
    pub fn document(&self) -> &document::Text {
        &self.document
    }
    pub fn view(&self) -> Element<'_, Message, Theme> {
        let columns = file::Columns {
            name_min: 100.0,
            size: 70.0,
            modified: 100.0,
            gap: 8.0,
            pad: 8.0,
        };
        let listing: Element<'_, file::Message, Theme> = FilePane::new(
            Listing {
                files: &self.files,
                selected: self.selected.as_deref(),
            },
            file::Presentation {
                tokens: self.tokens,
                ..Default::default()
            },
            columns,
        )
        .into();
        let header = file::Header::new(
            file::Presentation {
                tokens: self.tokens,
                ..Default::default()
            },
            columns,
            [
                strings::label("compound-name"),
                strings::label("compound-size"),
                strings::label("compound-modified"),
            ],
            0,
            true,
            Message::Sort,
        );
        let files = file::column(
            text(strings::label("compound-files")),
            header,
            listing.map(Message::File),
            text(self.notice.clone()),
            1,
        );
        let editor: Element<'_, editor::Message, Theme> = EditorPane::new(
            self.document().clone(),
            &self.palette,
            &editor::View::default(),
        )
        .into();
        let panes = toolkit::Split::new(self.split, files, editor.map(Message::Editor))
            .on_drag(Message::Split)
            .on_double_click(|| Message::Split(0.5));
        let surface = column![
            text(strings::label("compound-terminal")).font(iced::Font::MONOSPACE),
            text(strings::label("compound-terminal-engine")).font(iced::Font::MONOSPACE),
            text(strings::format(
                "compound-wheel",
                &[("position", format!("{:.1}", self.wheel))]
            ))
            .font(iced::Font::MONOSPACE)
        ];
        let terminal = TerminalPane::new(surface, Size::new(1000.0, 100.0), 1.2, self.tokens)
            .focus_ring(true)
            .on_scroll(Message::Wheel);
        container(
            column![
                row![
                    text(strings::label("compound-heading")),
                    button(text(strings::label("compound-theme"))).on_press(Message::Theme)
                ]
                .spacing(12),
                panes,
                terminal
            ]
            .spacing(8),
        )
        .padding(8)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }
}
pub fn run() -> iced::Result {
    iced::application(Demo::new, Demo::update, Demo::view)
        .theme(Demo::theme)
        .title(|_: &Demo| strings::label("compound-title"))
        .window_size(Size::new(1100.0, 700.0))
        .run()
}

// SPDX-License-Identifier: MIT OR Apache-2.0
//! The shell example, also driven by the headless integration tests.
#[cfg(not(any(feature = "wgpu", feature = "tiny-skia")))]
compile_error!("Select gallery-wgpu or gallery-tiny-skia to build the shell example.");

#[path = "../gallery/strings.rs"]
mod strings;

use strings::{format, label};
use toolkit::iced;
use toolkit::shell::{self, FieldKind, Shell, Side, StatusBar, Toolbar};
use toolkit::widget::{column, container, text};
use toolkit::{Item, Theme, Tokens, fonts};

pub fn run() -> iced::Result {
    iced::application(App::new, App::update, App::view)
        .default_font(fonts::default_ui_font())
        .title(|_: &App| label("shell-title"))
        .theme(App::theme)
        .run()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Dark,
    Light,
    Custom,
}

impl Mode {
    pub const ALL: [Self; 3] = [Self::Dark, Self::Light, Self::Custom];

    pub fn name(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
            Self::Custom => "custom",
        }
    }

    pub fn tokens(self) -> Tokens {
        match self {
            Self::Dark => Tokens::dark(),
            Self::Light => Tokens::light(),
            Self::Custom => {
                let mut tokens = Tokens::light();
                tokens.metrics.spacing.xs *= 2.0;
                tokens.metrics.spacing.sm *= 2.0;
                tokens.metrics.text.sm *= 1.25;
                tokens
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Message {
    New,
    Save,
    Places,
    Inspector,
    Select(usize),
    Theme,
    Resize(Side, f32),
}

pub struct App {
    pub mode: Mode,
    pub selected: usize,
    pub dirty: bool,
    pub revision: usize,
    pub show_places: bool,
    pub show_inspector: bool,
    pub left_width: f32,
    pub right_width: f32,
}

impl App {
    pub fn new() -> Self {
        Self {
            mode: Mode::Dark,
            selected: 0,
            dirty: false,
            revision: 1,
            show_places: true,
            show_inspector: true,
            left_width: 180.0,
            right_width: 180.0,
        }
    }

    pub fn update(&mut self, message: Message) {
        match message {
            Message::New => {
                self.revision += 1;
                self.dirty = true;
            }
            Message::Save => self.dirty = false,
            Message::Places => self.show_places = !self.show_places,
            Message::Inspector => self.show_inspector = !self.show_inspector,
            Message::Select(selected) => self.selected = selected,
            Message::Theme => {
                let current = Mode::ALL
                    .iter()
                    .position(|mode| *mode == self.mode)
                    .expect("current theme mode");
                self.mode = Mode::ALL[(current + 1) % Mode::ALL.len()];
            }
            Message::Resize(Side::Left, width) => self.left_width = width,
            Message::Resize(Side::Right, width) => self.right_width = width,
        }
    }

    pub fn theme(&self) -> Theme {
        Theme::named(
            self.mode.tokens(),
            label(&format!("shell-{}", self.mode.name())),
        )
    }

    pub fn view(&self) -> iced::Element<'_, Message, Theme> {
        let tokens = self.mode.tokens();
        let metrics = tokens.metrics;
        let menus = vec![
            Item::submenu(
                label("shell-file"),
                vec![
                    Item::action(label("shell-new"), Message::New),
                    Item::action(label("shell-save"), Message::Save).enabled(self.dirty),
                ],
            ),
            Item::submenu(
                label("shell-view"),
                vec![
                    Item::action(label("shell-places"), Message::Places),
                    Item::action(label("shell-inspector"), Message::Inspector),
                    Item::separator(),
                    Item::action(label("shell-theme"), Message::Theme),
                ],
            ),
        ];
        let toolbar = Toolbar::new()
            .leading(shell::tool(label("shell-places"), Message::Places).toggled(self.show_places))
            .push(shell::tool(label("shell-new"), Message::New))
            .push(shell::tool(label("shell-save"), Message::Save).enabled(self.dirty))
            .push(shell::tool(label("shell-theme"), Message::Theme))
            .trailing(
                shell::tool(label("shell-inspector"), Message::Inspector)
                    .toggled(self.show_inspector),
            );
        let content = container(
            column![
                text(label("shell-heading")).size(metrics.text.xxl),
                text(label("shell-description")).size(metrics.text.md),
                text(format(
                    "shell-document",
                    &[("revision", self.revision.to_string())]
                ))
                .size(metrics.text.sm),
            ]
            .spacing(metrics.spacing.lg),
        )
        .padding(metrics.spacing.lg)
        .width(iced::Fill)
        .height(iced::Fill);
        let state = if self.dirty {
            "shell-unsaved"
        } else {
            "shell-ready"
        };
        let status = StatusBar::new()
            .left(vec![shell::field(label(state)).kind(if self.dirty {
                FieldKind::Alarm
            } else {
                FieldKind::Plain
            })])
            .right(vec![
                shell::field(label("shell-encoding")).kind(FieldKind::Quiet),
            ]);
        let mut shell = Shell::new(content)
            .tokens(tokens)
            .menu(Some(menus))
            .toolbar(toolbar)
            .status(status)
            .on_split(Message::Resize);
        if self.show_places {
            let entries = ["shell-overview", "shell-documents", "shell-downloads"]
                .into_iter()
                .enumerate()
                .map(|(i, key)| shell::place(label(key), self.selected == i, Message::Select(i)))
                .collect();
            shell = shell.sidebar(Side::Left, self.left_width, shell::places(tokens, entries));
        }
        if self.show_inspector {
            let inspector = container(
                column![
                    text(label("shell-details")).size(metrics.text.sm),
                    text(label("shell-detail-text")).size(metrics.text.sm),
                ]
                .spacing(metrics.spacing.sm),
            )
            .padding(metrics.spacing.sm)
            .width(iced::Fill)
            .height(iced::Fill)
            .style(toolkit::theme::container::card);
            shell = shell.sidebar(Side::Right, self.right_width, inspector);
        }
        shell.into()
    }
}

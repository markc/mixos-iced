// SPDX-License-Identifier: MIT OR Apache-2.0
//! The gallery program shared by the `gallery` and `gallery_fonts` examples
//! and the offscreen `snapshots` test: iced's built-in widgets and every
//! toolkit widget under `toolkit::Theme`, switchable between the built-in
//! dark and light `Tokens` and a custom set while running, plus the
//! installed fonts and icons when the host registered some.
#[cfg(not(any(feature = "wgpu", feature = "tiny-skia")))]
compile_error!("Select gallery-wgpu or gallery-tiny-skia to build the gallery.");

use toolkit::iced::widget::{
    button, checkbox, column, combo_box, container, pick_list, progress_bar, radio, row, rule,
    scrollable, slider, text, text_editor, toggler, tooltip,
};
use toolkit::iced::{self, Color, Fill};
#[path = "strings.rs"]
mod strings;
#[path = "services.rs"]
pub mod services;
#[path = "lists.rs"]
mod lists;
use strings::label;
use toolkit::fonts::{self, Role};
use toolkit::scale::format_db;
use toolkit::theme::{self, Theme};
use toolkit::tokens::{Metrics, Palette, Radii};
use toolkit::{
    Fader, Item, Knob, LevelMeter, Menu, Note, PianoRoll, RollNotes, RollView, TextField, Toggle,
    Tokens, Waveform, WaveformPeaks, icon,
};

pub type Element<'a> = iced::Element<'a, Message, Theme>;

const STRIPS: usize = 8;
const ROLL_NOTES: usize = 131_072;
/// Icons shown from an installed icon font, so the row stays readable.
const ICON_SAMPLE: usize = 16;
/// One colour per track, so a dense roll reads as separate parts.
const TRACK_COLOURS: [Color; 4] = toolkit::tokens::PREVIEW_TRACK_COLOURS;
const CHOICES: [&str; 3] = ["Alpha", "Beta", "Gamma"];

/// Run the gallery with whatever `toolkit::fonts` has installed.
pub fn run() -> iced::Result {
    iced::application(Gallery::new, Gallery::update, Gallery::view)
        .default_font(fonts::default_ui_font())
        .title(|_: &Gallery| label("gallery-title"))
        .theme(Gallery::theme)
        .run()
}

/// The token sets the gallery switches between.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Dark,
    Light,
    /// An application's own palette and metrics: a violet primary on the
    /// light surfaces, larger radii and type.
    Custom,
}

impl Mode {
    pub const ALL: [Mode; 3] = [Mode::Dark, Mode::Light, Mode::Custom];

    pub fn name(self) -> &'static str {
        match self {
            Mode::Dark => "dark",
            Mode::Light => "light",
            Mode::Custom => "custom",
        }
    }

    pub fn tokens(self) -> Tokens {
        match self {
            Mode::Dark => Tokens::dark(),
            Mode::Light => Tokens::light(),
            Mode::Custom => {
                let light = Palette::light();
                let primary = Color::from_rgb8(98, 70, 234);
                Tokens::new(
                    Palette {
                        primary,
                        selection: Color::from_rgb8(226, 220, 252),
                        selection_text: Color::from_rgb8(40, 24, 110),
                        ring: primary,
                        ..light
                    },
                    Metrics {
                        radius: Radii {
                            sm: 6.0,
                            md: 10.0,
                            lg: 18.0,
                        },
                        ..Metrics::DEFAULT
                    },
                )
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    One,
    Two,
}

/// The gallery's pages; each new one lives in its own file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Widgets,
    Services,
    Lists,
}

impl Page {
    pub const ALL: [Page; 3] = [Page::Widgets, Page::Services, Page::Lists];

    pub fn name(self) -> &'static str {
        match self {
            Page::Widgets => "widgets",
            Page::Services => "services",
            Page::Lists => "lists",
        }
    }
}

pub struct Gallery {
    mode: Mode,
    theme: Theme,
    page: Page,
    services: services::State,
    value: String,
    password: String,
    last_action: String,
    checked: bool,
    choice: Option<Choice>,
    toggled: bool,
    amount: f32,
    pick: Option<&'static str>,
    combo: combo_box::State<&'static str>,
    combo_pick: Option<&'static str>,
    editor: text_editor::Content,
    gain: [f32; STRIPS],
    pan: [f32; STRIPS],
    mute: [bool; STRIPS],
    solo: [bool; STRIPS],
    level: [f32; STRIPS],
    peak: [Option<f32>; STRIPS],
    hold: [Option<f32>; STRIPS],
    peaks: WaveformPeaks,
    playhead: f32,
    notes: RollNotes,
    view: RollView,
    picked: Option<usize>,
    lists: lists::Lists,
}

#[derive(Debug, Clone)]
pub enum Message {
    Mode(Mode),
    Page(Page),
    Services(services::Message),
    Lists(lists::Message),
    Text(String),
    Password(String),
    Action(&'static str),
    Check(bool),
    Choose(Choice),
    Toggle(bool),
    Amount(f32),
    Pick(&'static str),
    Combo(&'static str),
    Edit(text_editor::Action),
    Gain(usize, f32),
    Pan(usize, f32),
    Mute(usize, bool),
    Solo(usize, bool),
    Levels(bool),
    Seek(f32),
    View(RollView),
    PickNote(usize),
}

/// Deterministic test data: a decaying chirp and a dense random song.
fn song() -> (WaveformPeaks, RollNotes) {
    let samples: Vec<f32> = (0..480_000)
        .map(|i| {
            let t = i as f32 / 48_000.0;
            (t * t * 400.0).sin() * (-t * 0.3).exp()
        })
        .collect();
    let mut seed = 0x9e37_79b9_u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let notes = (0..ROLL_NOTES)
        .map(|_| Note {
            start: (next() % 1_024_000) as f32 / 1000.0,
            length: 0.1 + (next() % 2000) as f32 / 1000.0,
            pitch: 24 + (next() % 84) as u8,
            velocity: (next() % 128) as u8,
            track: (next() % 8) as u16,
        })
        .collect();
    (
        WaveformPeaks::from_samples(&samples, 256),
        RollNotes::new(notes),
    )
}

fn items() -> Vec<Item<Message>> {
    vec![
        Item::action(label("new"), Message::Action("new")).accelerator("Ctrl+N"),
        Item::submenu(
            label("open-recent"),
            vec![
                Item::action(label("session-one"), Message::Action("session-one")),
                Item::submenu(
                    label("archive"),
                    vec![Item::action(
                        label("session-two"),
                        Message::Action("session-two"),
                    )],
                ),
            ],
        ),
        Item::separator(),
        Item::action(label("unavailable"), Message::Action("unavailable")).enabled(false),
        Item::action(label("save"), Message::Action("save")).accelerator("Ctrl+S"),
    ]
}

impl Gallery {
    pub fn new() -> Self {
        let (peaks, notes) = song();
        Self {
            mode: Mode::Dark,
            theme: Theme::new(Mode::Dark.tokens()),
            page: Page::Widgets,
            services: services::State::new(),
            value: String::new(),
            password: String::new(),
            last_action: String::new(),
            checked: true,
            choice: Some(Choice::One),
            toggled: true,
            amount: 0.35,
            pick: Some(CHOICES[0]),
            combo: combo_box::State::new(CHOICES.to_vec()),
            combo_pick: None,
            editor: text_editor::Content::with_text(&label("editor-sample")),
            gain: [0.0; STRIPS],
            pan: [0.0; STRIPS],
            mute: [false; STRIPS],
            solo: [false; STRIPS],
            level: [f32::NEG_INFINITY; STRIPS],
            peak: [None; STRIPS],
            hold: [None; STRIPS],
            peaks,
            playhead: 0.0,
            notes,
            view: RollView::default(),
            picked: None,
            lists: lists::Lists::new(),
        }
    }

    /// The "Lists & trees" page on its own (the snapshots render it at its
    /// own size).
    pub fn lists_page(&self) -> Element<'_> {
        self.lists.view(self.theme.tokens()).map(Message::Lists)
    }

    /// The current theme; iced asks for it every frame, so a new `Tokens`
    /// set in `update` restyles the whole gallery on the next frame.
    pub fn theme(&self) -> Theme {
        self.theme.clone()
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn page(&self) -> Page {
        self.page
    }

    /// Read by `tests/services.rs`.
    #[allow(dead_code)]
    pub fn services(&self) -> &services::State {
        &self.services
    }

    pub fn update(&mut self, message: Message) {
        match message {
            Message::Mode(mode) => {
                self.mode = mode;
                self.theme.set_tokens(mode.tokens());
            }
            Message::Page(page) => self.page = page,
            Message::Services(message) => self.services.update(message),
            Message::Lists(message) => self.lists.update(message),
            Message::Text(value) => self.value = value,
            Message::Password(value) => self.password = value,
            Message::Action(action) => self.last_action = label(action),
            Message::Check(on) => self.checked = on,
            Message::Choose(choice) => self.choice = Some(choice),
            Message::Toggle(on) => self.toggled = on,
            Message::Amount(amount) => self.amount = amount,
            Message::Pick(choice) => self.pick = Some(choice),
            Message::Combo(choice) => self.combo_pick = Some(choice),
            Message::Edit(action) => self.editor.perform(action),
            Message::Gain(strip, db) => self.gain[strip] = db,
            Message::Pan(strip, pan) => self.pan[strip] = pan,
            Message::Mute(strip, on) => self.mute[strip] = on,
            Message::Solo(strip, on) => self.solo[strip] = on,
            // No timer: the meters only change on these buttons, so an idle
            // gallery schedules nothing once the peak lines have fallen.
            Message::Levels(loud) => {
                for strip in 0..STRIPS {
                    let level = if loud {
                        -1.0 - strip as f32 * 4.0
                    } else {
                        -48.0
                    };
                    self.level[strip] = level;
                    // A host tracks its own peak and hold; here the buttons
                    // stand in for a meter feed.
                    self.peak[strip] = Some(level + 2.0);
                    self.hold[strip] = Some(self.hold[strip].unwrap_or(level).max(level + 2.0));
                }
            }
            Message::Seek(fraction) => self.playhead = fraction,
            Message::View(view) => self.view = view,
            Message::PickNote(index) => self.picked = Some(index),
        }
    }

    /// One channel strip. The audio widgets take their colours from the
    /// theme; nothing here passes a style.
    fn strip(&self, strip: usize, tokens: Tokens) -> Element<'_> {
        column![
            text(strings::format(
                "channel",
                &[("number", (strip + 1).to_string())]
            ))
            .size(tokens.metrics.text.sm),
            Knob::new(self.pan[strip]).on_change(move |pan| Message::Pan(strip, pan)),
            row![
                Fader::new(self.gain[strip]).on_change(move |db| Message::Gain(strip, db)),
                LevelMeter::new(self.level[strip])
                    .peak(self.peak[strip])
                    .hold(self.hold[strip])
                    .clipped(self.level[strip] > -1.0),
            ]
            .spacing(tokens.metrics.spacing.sm),
            text(format_db(self.gain[strip])).size(tokens.metrics.text.xs),
            row![
                Toggle::new(label("mute"), self.mute[strip])
                    .alert(true)
                    .on_toggle(move |on| Message::Mute(strip, on)),
                Toggle::new(label("solo"), self.solo[strip])
                    .on_toggle(move |on| Message::Solo(strip, on)),
            ]
            .spacing(tokens.metrics.spacing.xs),
        ]
        .spacing(tokens.metrics.spacing.md)
        .into()
    }

    /// The installed fonts and icons, if the host example registered any.
    fn fonts_section(&self, tokens: Tokens) -> Option<Element<'_>> {
        let fonts = fonts::installed()?;
        let heading = tokens.metrics.text.xxl;
        let mut section =
            column![text(label("fonts")).size(heading)].spacing(tokens.metrics.spacing.md);
        for role in Role::ALL {
            if let Some(font) = fonts.font(role) {
                let sample = strings::format(
                    "font-sample",
                    &[
                        ("role", role.name().to_owned()),
                        ("family", fonts.family(role).unwrap_or_default().to_owned()),
                    ],
                );
                section = section.push(text(sample).font(font).size(tokens.metrics.text.lg));
            }
        }
        if fonts.icon_font().is_some() {
            let icons = row(fonts.icon_names().take(ICON_SAMPLE).map(|name| {
                column![
                    icon(name).size(heading),
                    text(name.to_owned()).size(tokens.metrics.text.xs),
                ]
                .spacing(tokens.metrics.spacing.xs)
                .into()
            }))
            .spacing(tokens.metrics.spacing.lg)
            .wrap();
            section = section.push(icons);
        }
        Some(section.into())
    }

    /// iced's built-in widgets, each styled by the theme's default class
    /// (the buttons show the other classes).
    fn controls_section(&self, tokens: Tokens) -> Element<'_> {
        let spacing = tokens.metrics.spacing;
        let buttons = row![
            button(text(label("primary"))).on_press(Message::Action("primary")),
            button(text(label("secondary")))
                .style(theme::button::secondary)
                .on_press(Message::Action("secondary")),
            button(text(label("destructive")))
                .style(theme::button::destructive)
                .on_press(Message::Action("destructive")),
            button(text(label("ghost")))
                .style(theme::button::text)
                .on_press(Message::Action("ghost")),
            button(text(label("disabled"))),
            tooltip(
                button(
                    row![
                        icon("info").size(tokens.metrics.text.lg),
                        text(label("hover"))
                    ]
                    .spacing(spacing.sm)
                )
                .style(theme::button::secondary)
                .on_press(Message::Action("hover")),
                container(text(label("tooltip")))
                    .padding(spacing.md)
                    .style(theme::container::tooltip),
                tooltip::Position::Bottom,
            ),
        ]
        .spacing(spacing.md)
        .wrap();
        let choices = row![
            checkbox(self.checked)
                .label(label("checkbox"))
                .on_toggle(Message::Check),
            checkbox(true).label(label("disabled")),
            radio(
                label("radio-one"),
                Choice::One,
                self.choice,
                Message::Choose
            ),
            radio(
                label("radio-two"),
                Choice::Two,
                self.choice,
                Message::Choose
            ),
            toggler(self.toggled)
                .label(label("toggler"))
                .on_toggle(Message::Toggle),
            toggler(false).label(label("disabled")),
        ]
        .spacing(spacing.lg)
        .wrap();
        let ranges = row![
            slider(0.0..=1.0, self.amount, Message::Amount).width(200),
            progress_bar(0.0..=1.0, self.amount)
                .length(200)
                .girth(tokens.metrics.spacing.md),
            text(format!("{:.0}%", self.amount * 100.0)).style(theme::text::muted),
        ]
        .spacing(spacing.lg)
        .align_y(iced::Center)
        .wrap();
        let lists = row![
            pick_list(self.pick, CHOICES, |choice| (*choice).to_owned())
                .on_select(Message::Pick)
                .placeholder(label("pick")),
            combo_box(
                &self.combo,
                label("combo"),
                self.combo_pick.as_ref(),
                Message::Combo
            )
            .width(200),
            text(strings::format(
                "picked-option",
                &[("option", self.combo_pick.unwrap_or_default().to_owned())]
            ))
            .style(theme::text::primary),
        ]
        .spacing(spacing.lg)
        .wrap();
        let surfaces = row![
            container(text(label("card")))
                .padding(spacing.lg)
                .style(theme::container::card),
            container(text(label("popover")))
                .padding(spacing.lg)
                .style(theme::container::popover),
            container(text(label("elevated")))
                .padding(spacing.lg)
                .style(theme::container::elevated),
            container(text(label("tooltip")))
                .padding(spacing.lg)
                .style(theme::container::tooltip),
            container(text(label("destructive")).style(theme::text::destructive))
                .padding(spacing.lg)
                .style(theme::container::surface),
        ]
        .spacing(spacing.lg)
        .wrap();
        let editor = text_editor(&self.editor)
            .placeholder(label("editor"))
            .on_action(Message::Edit)
            .height(96);
        column![
            text(label("controls")).size(tokens.metrics.text.xxl),
            buttons,
            rule::horizontal(1),
            choices,
            ranges,
            lists,
            surfaces,
            editor,
        ]
        .spacing(spacing.lg)
        .into()
    }

    pub fn view(&self) -> Element<'_> {
        let tokens = self.theme.tokens();
        let heading = tokens.metrics.text.xxl;
        let bar = Menu::bar(vec![
            Item::submenu(label("file"), items()),
            Item::submenu(
                label("help"),
                vec![Item::action(label("about"), Message::Action("about"))],
            ),
        ]);
        let context = Menu::context(
            container(text(label("context-hint")))
                .padding(tokens.metrics.spacing.xl)
                .width(Fill)
                .style(theme::container::card),
            items(),
        );
        let mixer = row((0..STRIPS).map(|strip| self.strip(strip, tokens)))
            .spacing(tokens.metrics.spacing.lg);
        let end = self.notes.end_beat();
        let picked = self
            .picked
            .map(|index| self.notes.notes()[index])
            .map_or_else(String::new, |note| {
                strings::format(
                    "picked",
                    &[
                        ("pitch", note.pitch.to_string()),
                        ("beat", format!("{:.2}", note.start)),
                    ],
                )
            });
        let modes = row(Mode::ALL.into_iter().map(|mode| {
            let selected = mode == self.mode();
            let mut choice = button(text(label(&format!("theme-{}", mode.name()))));
            if !selected {
                choice = choice.style(theme::button::secondary);
            }
            choice.on_press(Message::Mode(mode)).into()
        }))
        .spacing(tokens.metrics.spacing.md);
        let pages = row(Page::ALL.into_iter().map(|page| {
            let mut choice = button(text(label(&format!("page-{}", page.name()))));
            if page != self.page() {
                choice = choice.style(theme::button::text);
            }
            choice.on_press(Message::Page(page)).into()
        }))
        .spacing(tokens.metrics.spacing.md);
        let mut page = column![
            row![
                modes,
                pages,
                text(self.theme.to_string()).style(theme::text::muted)
            ]
            .spacing(tokens.metrics.spacing.lg)
            .align_y(iced::Center),
        ]
        .spacing(tokens.metrics.spacing.lg)
        .padding(tokens.metrics.spacing.xl);
        if self.page == Page::Services {
            let page = page.push(self.services.view(tokens).map(Message::Services));
            let content = column![bar, scrollable(page).height(Fill)]
                .width(Fill)
                .height(Fill);
            return self
                .services
                .wrap(content.into(), tokens, Message::Services);
        }
        if self.page == Page::Lists {
            let page = page.push(self.lists_page());
            return column![bar, scrollable(page).height(Fill)]
                .width(Fill)
                .height(Fill)
                .into();
        }
        if let Some(fonts) = self.fonts_section(tokens) {
            page = page.push(fonts);
        }
        let page = page.extend([
            self.controls_section(tokens),
            text(label("text-input")).size(heading).into(),
            TextField::new(&label("input-hint"), &self.value)
                .on_input(Message::Text)
                .padding(tokens.metrics.spacing.md)
                .into(),
            TextField::new(&label("password"), &self.password)
                .secure(true)
                .on_input(Message::Password)
                .padding(tokens.metrics.spacing.md)
                .into(),
            text(label("menu-hint")).into(),
            context.into(),
            text(strings::format(
                "last-action",
                &[("action", self.last_action.clone())],
            ))
            .into(),
            text(label("mixer")).size(heading).into(),
            row![
                button(text(label("loud"))).on_press(Message::Levels(true)),
                button(text(label("quiet")))
                    .style(theme::button::secondary)
                    .on_press(Message::Levels(false)),
            ]
            .spacing(tokens.metrics.spacing.md)
            .into(),
            mixer.into(),
            text(label("waveform")).size(heading).into(),
            Waveform::new(&self.peaks)
                .playhead(Some(self.playhead))
                .on_seek(Message::Seek)
                .into(),
            text(strings::format(
                "piano-roll",
                &[("count", self.notes.len().to_string()), ("picked", picked)],
            ))
            .size(heading)
            .into(),
            PianoRoll::new(&self.notes, self.view)
                .track_colours(&TRACK_COLOURS)
                .playhead(Some(self.playhead * end))
                .on_view(Message::View)
                .on_note(Message::PickNote)
                .height(320)
                .into(),
        ]);
        column![bar, scrollable(page).height(Fill)]
            .width(Fill)
            .height(Fill)
            .into()
    }
}

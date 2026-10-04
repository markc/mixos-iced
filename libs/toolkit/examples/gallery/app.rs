// SPDX-License-Identifier: MIT OR Apache-2.0
//! The gallery program shared by the `gallery` and `gallery_fonts` examples:
//! every widget under the built-in dark and light `Tokens`, plus the
//! installed fonts and icons when the host example registered some.
#[cfg(not(any(feature = "wgpu", feature = "tiny-skia")))]
compile_error!("Select gallery-wgpu or gallery-tiny-skia to build the gallery.");

use toolkit::iced::{self, Element, Fill, Theme};
#[path = "strings.rs"]
mod strings;
use strings::label;
use toolkit::fonts::{self, Role};
use toolkit::scale::format_db;
use toolkit::widget::{button, column, container, row, scrollable, text};
use toolkit::{
    Fader, Item, Knob, LevelMeter, Menu, Note, PianoRoll, RollNotes, RollView, TextField, Toggle,
    Tokens, Waveform, WaveformPeaks,
};

const STRIPS: usize = 8;
const ROLL_NOTES: usize = 131_072;
/// Icons shown from an installed icon font, so the row stays readable.
const ICON_SAMPLE: usize = 16;
/// One colour per track, so a dense roll reads as separate parts.
const TRACK_COLOURS: [iced::Color; 4] = toolkit::tokens::PREVIEW_TRACK_COLOURS;

/// Run the gallery with whatever `toolkit::fonts` has installed.
pub fn run() -> iced::Result {
    iced::application(Gallery::new, Gallery::update, Gallery::view)
        .default_font(fonts::default_ui_font())
        .title(|_: &Gallery| label("gallery-title"))
        .theme(|gallery: &Gallery| {
            if gallery.dark {
                Theme::Dark
            } else {
                Theme::Light
            }
        })
        .run()
}

struct Gallery {
    dark: bool,
    value: String,
    password: String,
    last_action: String,
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
}

#[derive(Debug, Clone)]
enum Message {
    Dark(bool),
    Text(String),
    Password(String),
    Action(&'static str),
    Gain(usize, f32),
    Pan(usize, f32),
    Mute(usize, bool),
    Solo(usize, bool),
    Levels(bool),
    Seek(f32),
    View(RollView),
    Pick(usize),
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
    fn new() -> Self {
        let (peaks, notes) = song();
        Self {
            dark: true,
            value: String::new(),
            password: String::new(),
            last_action: String::new(),
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
        }
    }

    fn tokens(&self) -> Tokens {
        if self.dark {
            Tokens::dark()
        } else {
            Tokens::light()
        }
    }

    fn update(&mut self, message: Message) {
        match message {
            Message::Dark(dark) => self.dark = dark,
            Message::Text(value) => self.value = value,
            Message::Password(value) => self.password = value,
            Message::Action(action) => self.last_action = label(action),
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
            Message::Pick(index) => self.picked = Some(index),
        }
    }

    fn strip(&self, strip: usize, tokens: Tokens) -> Element<'_, Message> {
        let style = tokens.audio_style();
        column![
            text(strings::format(
                "channel",
                &[("number", (strip + 1).to_string())]
            ))
            .size(tokens.metrics.text.sm),
            Knob::new(self.pan[strip])
                .on_change(move |pan| Message::Pan(strip, pan))
                .style(style),
            row![
                Fader::new(self.gain[strip])
                    .on_change(move |db| Message::Gain(strip, db))
                    .style(style),
                LevelMeter::new(self.level[strip])
                    .peak(self.peak[strip])
                    .hold(self.hold[strip])
                    .clipped(self.level[strip] > -1.0)
                    .style(style),
            ]
            .spacing(tokens.metrics.spacing.sm),
            text(format_db(self.gain[strip])).size(tokens.metrics.text.xs),
            row![
                Toggle::new(label("mute"), self.mute[strip])
                    .alert(true)
                    .on_toggle(move |on| Message::Mute(strip, on))
                    .style(style),
                Toggle::new(label("solo"), self.solo[strip])
                    .on_toggle(move |on| Message::Solo(strip, on))
                    .style(style),
            ]
            .spacing(tokens.metrics.spacing.xs),
        ]
        .spacing(tokens.metrics.spacing.md)
        .into()
    }

    /// The installed fonts and icons, if the host example registered any.
    fn fonts_section(&self, tokens: Tokens) -> Option<Element<'_, Message>> {
        let fonts = fonts::installed()?;
        let heading = tokens.metrics.text.xxl;
        let mut section = column![text(label("fonts")).size(heading)]
            .spacing(tokens.metrics.spacing.md);
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
            let icons = row(fonts.icon_names().take(ICON_SAMPLE).filter_map(|name| {
                let (glyph, font) = fonts.icon(name)?;
                Some(
                    column![
                        text(glyph.to_string()).font(font).size(heading),
                        text(name.to_owned()).size(tokens.metrics.text.xs),
                    ]
                    .spacing(tokens.metrics.spacing.xs)
                    .into(),
                )
            }))
            .spacing(tokens.metrics.spacing.lg)
            .wrap();
            section = section.push(icons);
        }
        Some(section.into())
    }

    fn view(&self) -> Element<'_, Message> {
        let tokens = self.tokens();
        let style = tokens.audio_style();
        let heading = tokens.metrics.text.xxl;
        let bar = Menu::bar(vec![
            Item::submenu(label("file"), items()),
            Item::submenu(
                label("help"),
                vec![Item::action(label("about"), Message::Action("about"))],
            ),
        ])
        .style(tokens.menu_style());
        let context = Menu::context(
            container(text(label("context-hint")))
                .padding(tokens.metrics.spacing.xl)
                .width(Fill),
            items(),
        )
        .style(tokens.menu_style());
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
        let mut page = column![
            row![
                button(text(label("theme-dark"))).on_press(Message::Dark(true)),
                button(text(label("theme-light"))).on_press(Message::Dark(false)),
            ]
            .spacing(tokens.metrics.spacing.md),
        ]
        .spacing(tokens.metrics.spacing.lg)
        .padding(tokens.metrics.spacing.xl);
        if let Some(fonts) = self.fonts_section(tokens) {
            page = page.push(fonts);
        }
        let page = page.extend([
            text(label("text-input")).size(heading).into(),
            TextField::new(&label("input-hint"), &self.value)
                .on_input(Message::Text)
                .padding(tokens.metrics.spacing.md)
                .style(move |_, status| tokens.text_input(status))
                .into(),
            TextField::new(&label("password"), &self.password)
                .secure(true)
                .on_input(Message::Password)
                .padding(tokens.metrics.spacing.md)
                .style(move |_, status| tokens.text_input(status))
                .into(),
            text(label("menu-hint")).into(),
            context.into(),
            text(strings::format(
                "last-action",
                &[("action", self.last_action.clone())]
            ))
            .into(),
            text(label("mixer")).size(heading).into(),
            row![
                button(text(label("loud"))).on_press(Message::Levels(true)),
                button(text(label("quiet"))).on_press(Message::Levels(false)),
            ]
            .spacing(tokens.metrics.spacing.md)
            .into(),
            mixer.into(),
            text(label("waveform")).size(heading).into(),
            Waveform::new(&self.peaks)
                .playhead(Some(self.playhead))
                .on_seek(Message::Seek)
                .style(style)
                .into(),
            text(strings::format(
                "piano-roll",
                &[("count", self.notes.len().to_string()), ("picked", picked)]
            ))
            .size(heading)
            .into(),
            PianoRoll::new(&self.notes, self.view)
                .track_colours(&TRACK_COLOURS)
                .playhead(Some(self.playhead * end))
                .on_view(Message::View)
                .on_note(Message::Pick)
                .height(320)
                .style(style)
                .into(),
        ]);
        column![bar, scrollable(page).height(Fill)]
            .width(Fill)
            .height(Fill)
            .into()
    }
}

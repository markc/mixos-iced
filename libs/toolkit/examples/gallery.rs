// SPDX-License-Identifier: MIT OR Apache-2.0
//! Launch only inside a test compositor; see docs/dev/iced-widgets.md.
#[cfg(not(any(feature = "wgpu", feature = "tiny-skia")))]
compile_error!("Select gallery-wgpu or gallery-tiny-skia to build the gallery.");

use toolkit::iced::{self, Element, Fill, Theme};
#[path = "gallery/strings.rs"]
mod strings;
use strings::label;
use toolkit::scale::format_db;
use toolkit::widget::{button, column, container, row, scrollable, text};
use toolkit::{
    Fader, Item, Knob, LevelMeter, Menu, Note, PianoRoll, RollNotes, RollView, TextField, Toggle,
    Tokens, Waveform, WaveformPeaks,
};

const STRIPS: usize = 8;
const ROLL_NOTES: usize = 131_072;
/// One colour per track, so a dense roll reads as separate parts.
const TRACK_COLOURS: [iced::Color; 4] = toolkit::tokens::PREVIEW_TRACK_COLOURS;

fn main() -> iced::Result {
    iced::application(Gallery::new, Gallery::update, Gallery::view)
        .default_font(toolkit::fonts::default_ui_font())
        .title(|_: &Gallery| label("gallery-title"))
        .theme(Theme::Dark)
        .run()
}

struct Gallery {
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

    fn update(&mut self, message: Message) {
        match message {
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
            .size(12),
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
            .spacing(4),
            text(format_db(self.gain[strip])).size(11),
            row![
                Toggle::new(label("mute"), self.mute[strip])
                    .alert(true)
                    .on_toggle(move |on| Message::Mute(strip, on))
                    .style(style),
                Toggle::new(label("solo"), self.solo[strip])
                    .on_toggle(move |on| Message::Solo(strip, on))
                    .style(style),
            ]
            .spacing(2),
        ]
        .spacing(6)
        .into()
    }

    fn view(&self) -> Element<'_, Message> {
        let tokens = Tokens::default();
        let style = tokens.audio_style();
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
                .padding(24)
                .width(Fill),
            items(),
        )
        .style(tokens.menu_style());
        let mixer = row((0..STRIPS).map(|strip| self.strip(strip, tokens))).spacing(12);
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
        let page = column![
            text(label("text-input")).size(24),
            TextField::new(&label("input-hint"), &self.value)
                .on_input(Message::Text)
                .padding(10)
                .style(move |_, status| tokens.text_input(status)),
            TextField::new(&label("password"), &self.password)
                .secure(true)
                .on_input(Message::Password)
                .padding(10)
                .style(move |_, status| tokens.text_input(status)),
            text(label("menu-hint")),
            context,
            text(strings::format(
                "last-action",
                &[("action", self.last_action.clone())]
            )),
            text(label("mixer")).size(24),
            row![
                button(text(label("loud"))).on_press(Message::Levels(true)),
                button(text(label("quiet"))).on_press(Message::Levels(false)),
            ]
            .spacing(8),
            mixer,
            text(label("waveform")).size(24),
            Waveform::new(&self.peaks)
                .playhead(Some(self.playhead))
                .on_seek(Message::Seek)
                .style(style),
            text(strings::format(
                "piano-roll",
                &[("count", self.notes.len().to_string()), ("picked", picked)]
            ))
            .size(24),
            PianoRoll::new(&self.notes, self.view)
                .track_colours(&TRACK_COLOURS)
                .playhead(Some(self.playhead * end))
                .on_view(Message::View)
                .on_note(Message::Pick)
                .height(320)
                .style(style),
        ]
        .spacing(16)
        .padding(24);
        column![bar, scrollable(page).height(Fill)]
            .width(Fill)
            .height(Fill)
            .into()
    }
}

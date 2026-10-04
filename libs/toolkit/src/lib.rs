// SPDX-License-Identifier: MIT OR Apache-2.0
//! Event-driven iced widgets with plain-data theming: text field, menus,
//! pro-audio controls, waveform, piano roll; [`Tokens`] for colours and
//! metrics; [`fonts`] for caller-supplied font sets and icon fonts.
//!
//! Select `wgpu` or `tiny-skia` in the host. The default feature set
//! deliberately selects neither renderer and links no window shell.

pub mod audio_style;
pub mod fader;
pub mod fonts;
pub mod knob;
pub mod menu;
pub mod meter;
pub mod piano_roll;
pub mod scale;
pub mod text_field;
pub mod toggle;
pub mod tokens;
pub mod waveform;

#[cfg(test)]
mod test_renderer;

pub use audio_style::AudioStyle;
pub use fader::Fader;
pub use fonts::{FontSet, FontSource, Fonts, IconFont, Role};
pub use knob::Knob;
pub use menu::{Item, Menu, MenuState, MenuStyle, NavOutcome, Navigator, Panel};
pub use meter::LevelMeter;
pub use piano_roll::{Note, PianoRoll, RollNotes, RollView};
pub use text_field::TextField;
pub use toggle::Toggle;
pub use tokens::{Metrics, Palette, Tokens};
pub use waveform::{Waveform, WaveformPeaks};

#[cfg(feature = "gallery")]
pub use iced;
pub use iced_core as core;
pub use iced_graphics as graphics;
pub use iced_renderer as renderer;
pub use iced_runtime as runtime;
#[cfg(feature = "wgpu")]
pub use iced_wgpu as wgpu;
pub use iced_widget as widget;

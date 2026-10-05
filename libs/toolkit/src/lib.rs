// SPDX-License-Identifier: MIT OR Apache-2.0
//! Event-driven iced widgets with plain-data theming: text field, menus,
//! pro-audio controls, waveform, piano roll; [`Tokens`] for colours and
//! metrics, [`Theme`] as a complete iced theme made of them; [`fonts`] for
//! caller-supplied font sets and icon fonts, [`icon`] for glyphs by name,
//! [`icons::freedesktop`] for themed icon files on disk.
//!
//! Select `wgpu` or `tiny-skia` in the host. The default feature set
//! deliberately selects neither renderer and links no window shell.

pub mod audio_style;
pub mod dialog;
pub mod elide;
pub mod fader;
pub mod fit_text;
pub mod focus;
pub mod fonts;
pub mod icon;
pub mod icons;
pub mod ime;
pub mod images;
pub mod keys;
pub mod knob;
pub mod menu;
pub mod measure;
pub mod meter;
pub mod piano_roll;
pub mod scale;
pub mod text_field;
pub mod theme;
pub mod timers;
pub mod tips;
pub mod toast;
pub mod toggle;
pub mod tokens;
pub mod tree;
pub mod virtual_list;
pub mod waveform;

#[cfg(test)]
mod test_renderer;

pub use audio_style::AudioStyle;
pub use dialog::{Dialog, ModalQueue};
pub use elide::{Label, middle};
pub use fader::Fader;
pub use fit_text::FitText;
pub use fonts::{FontSet, FontSource, Fonts, IconFont, Role};
pub use icon::{Icon, icon};
pub use ime::Composition;
pub use images::Images;
pub use keys::{Bindings, Chord, KeyRouter};
pub use knob::Knob;
pub use measure::Measure;
pub use menu::{Item, Menu, MenuState, MenuStyle, NavOutcome, Navigator, Panel};
pub use meter::LevelMeter;
pub use piano_roll::{Note, PianoRoll, RollNotes, RollView};
pub use text_field::TextField;
pub use theme::Theme;
pub use timers::Timers;
pub use toast::{Toast, Toaster};
pub use toggle::Toggle;
pub use tokens::{Metrics, Palette, Tokens};
pub use tree::{Nodes, TreeView};
pub use virtual_list::{Selection, VirtualList};
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

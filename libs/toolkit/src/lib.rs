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
pub mod anchor;
pub mod badge;
pub mod card;
pub mod collapsible;
pub mod color_picker;
pub mod command_palette;
pub mod dialog;
pub mod drop_down;
pub mod elide;
pub mod fader;
pub mod fit_text;
pub mod flush_column;
pub mod focus;
pub mod fonts;
pub mod icon;
pub mod icons;
pub mod ime;
/// Eager raster decoding; opt-in (`image` feature) because it links a
/// decoder.
#[cfg(feature = "image")]
pub mod images;
pub mod keys;
pub mod knob;
pub mod labeled_frame;
pub mod menu;
pub mod measure;
pub mod meter;
pub mod number_input;
pub mod piano_roll;
pub mod popover;
pub mod requester;
pub mod scale;
pub mod selection_list;
pub mod sidebar;
pub mod slide_bar;
pub mod split;
pub mod spinner;
pub mod spinners;
pub mod table;
pub mod tab_bar;
pub mod tabs;
pub mod text_field;
pub mod theme;
pub mod timers;
pub mod typed_input;
pub mod tips;
pub mod toast;
pub mod toggle;
pub mod tokens;
pub mod tree;
pub mod virtual_list;
pub mod waveform;
pub mod wrap;

#[cfg(test)]
mod test_renderer;

pub use audio_style::AudioStyle;
pub use badge::Badge;
pub use card::Card;
pub use color_picker::{ColorPicker, Hsv, Spectrum};
pub use command_palette::Command;
pub use dialog::{Dialog, ModalQueue};
pub use drop_down::{Alignment, DropDown, Offset};
pub use elide::{Label, middle};
pub use fader::Fader;
pub use fit_text::FitText;
pub use flush_column::FlushColumn;
pub use fonts::{FontSet, FontSource, Fonts, IconFont, Role};
pub use icon::{Icon, icon};
pub use ime::Composition;
#[cfg(feature = "image")]
pub use images::Images;
pub use labeled_frame::LabeledFrame;
pub use keys::{Bindings, Chord, KeyRouter};
pub use knob::Knob;
pub use measure::Measure;
pub use menu::{Item, Menu, MenuState, MenuStyle, NavOutcome, Navigator, Panel};
pub use meter::LevelMeter;
pub use number_input::NumberInput;
pub use piano_roll::{Note, PianoRoll, RollNotes, RollView};
pub use requester::{Entry, Filesystem, Mode, Outcome, Requester, StdFs};
pub use selection_list::SelectionList;
pub use sidebar::Sidebar;
pub use slide_bar::SlideBar;
pub use spinner::Spinner;
pub use split::Split;
pub use tab_bar::{TabBar, TabLabel};
pub use table::Table;
pub use tabs::Tabs;
pub use text_field::TextField;
pub use theme::Theme;
pub use timers::Timers;
pub use toast::{Toast, Toaster};
pub use toggle::Toggle;
pub use tokens::{Metrics, Palette, Tokens};
pub use tree::{Nodes, TreeView};
pub use typed_input::TypedInput;
pub use virtual_list::{Selection, VirtualList};
pub use waveform::{Waveform, WaveformPeaks};
pub use wrap::Wrap;

#[cfg(feature = "gallery")]
pub use iced;
pub use iced_core as core;
pub use iced_graphics as graphics;
pub use iced_renderer as renderer;
pub use iced_runtime as runtime;
#[cfg(feature = "wgpu")]
pub use iced_wgpu as wgpu;
pub use iced_widget as widget;

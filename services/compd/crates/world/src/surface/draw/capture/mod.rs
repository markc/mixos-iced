//! Iced overlay UIs for window capture.
//!
//! All are screen-space instances driven by the capture interface:
//! - [`setup::SetupOverlay`]   — the black mask + passthrough hole + target/
//!   media chooser + region drag (the "ice screen" setup phase).
//! - [`border::RegionBorder`]  — the bright outline around the captured region
//!   (drawn above everything).
//! - [`dim::RegionDim`]        — the dark backdrop with a clear hole (drawn
//!   below windows).
//! - [`hud::StopHud`]          — the top-right Stop button shown while capturing.
//! - [`dialog::ContinueDialog`]— the 5-minute "continue?" countdown dialog.
//!
//! Every UI's `IcedUi::Message` is
//! [`crate::capture::session::message::CaptureMessage`].

// Developer logging: bring error!/warn!/info!/trace!/abort! into scope for every module in
// this crate. (Drop this line if the crate genuinely never logs.)

pub mod border;
pub mod dialog;
pub mod dim;
pub mod encodialog;
pub mod errordialog;
pub mod hud;
pub mod mask;
pub mod savedialog;
pub mod setup;
pub mod style;

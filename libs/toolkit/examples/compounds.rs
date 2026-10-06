// SPDX-License-Identifier: MIT OR Apache-2.0
//! Run with `--features gallery-tiny-skia` or `gallery-wgpu`.
#[path = "compounds/app.rs"]
mod app;
fn main() -> toolkit::iced::Result { app::run() }

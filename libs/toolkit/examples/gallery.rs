// SPDX-License-Identifier: MIT OR Apache-2.0
//! The widget gallery as a plain iced (winit) program with the default
//! fonts: `cargo run -p toolkit --example gallery --features gallery-wgpu`
//! (or `gallery-tiny-skia`). See `gallery_fonts` to supply a font set.
#[path = "gallery/app.rs"]
mod app;

fn main() -> toolkit::iced::Result {
    app::run()
}

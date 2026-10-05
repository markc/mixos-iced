// SPDX-License-Identifier: MIT OR Apache-2.0
//! A plain iced application composed with `toolkit::shell`.
//! `cargo run -p toolkit --example shell --features gallery-tiny-skia`
#[path = "shell/app.rs"]
mod app;

fn main() -> toolkit::iced::Result {
    app::run()
}

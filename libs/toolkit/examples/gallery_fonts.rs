// SPDX-License-Identifier: MIT OR Apache-2.0
//! The widget gallery with a caller-supplied `FontSet` and `IconFont`, read
//! from paths given on the command line (every option is optional):
//!
//! ```text
//! cargo run -p toolkit --example gallery_fonts --features gallery-wgpu -- \
//!     --sans Sans.ttf --mono Mono.ttf --serif Serif.ttf --display Display.ttf \
//!     --emoji Emoji.ttf --icons Symbols.ttf --codepoints Symbols.codepoints
//! ```
//!
//! `--codepoints` defaults to the icon font's path with a `.codepoints`
//! extension. Nothing is read from the environment.
use std::path::PathBuf;

use toolkit::{FontSet, IconFont, fonts};

#[path = "gallery/app.rs"]
mod app;

fn main() -> toolkit::iced::Result {
    let (set, icon) = match parse(std::env::args().skip(1)) {
        Ok(fonts) => fonts,
        Err(message) => {
            eprintln!("gallery_fonts: {message}");
            std::process::exit(2);
        }
    };
    if let Err(error) = fonts::install(set, icon) {
        eprintln!("gallery_fonts: {error}");
        std::process::exit(1);
    }
    app::run()
}

/// `--role PATH` pairs into a `FontSet`, plus the icon font and its table.
fn parse(
    args: impl IntoIterator<Item = String>,
) -> Result<(FontSet, Option<IconFont>), String> {
    let mut set = FontSet::new();
    let mut icons: Option<PathBuf> = None;
    let mut codepoints: Option<PathBuf> = None;
    let mut args = args.into_iter();
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .map(PathBuf::from)
            .ok_or_else(|| format!("{flag} needs a path"))?;
        set = match flag.as_str() {
            "--sans" => set.sans(value),
            "--mono" => set.mono(value),
            "--serif" => set.serif(value),
            "--display" => set.display(value),
            "--emoji" => set.emoji(value),
            "--icons" => {
                icons = Some(value);
                set
            }
            "--codepoints" => {
                codepoints = Some(value);
                set
            }
            _ => return Err(format!("unknown option {flag}")),
        };
    }
    let icon = icons
        .map(|font| {
            let table = codepoints.unwrap_or_else(|| font.with_extension("codepoints"));
            let text = std::fs::read_to_string(&table)
                .map_err(|error| format!("{}: {error}", table.display()))?;
            IconFont::from_codepoints(font, &text)
                .map_err(|error| format!("{}: {error}", table.display()))
        })
        .transpose()?;
    Ok((set, icon))
}

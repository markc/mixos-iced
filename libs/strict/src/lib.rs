//! Strict data: the MixOS configuration and data format.
//!
//! A `*.conf.mix` or `*.data.mix` file is a JSON-shaped tree written with
//! a friendlier surface: bare keys, optional top-level braces, `#` and `--`
//! comments, trailing commas, `nil`. It is loaded as data and never
//! executed: a variable, a call, a command substitution, string
//! interpolation or a `;` is refused with an error that names the construct
//! and its line.
//!
//! ```text
//! # A service config.
//! name: "alpha"
//! port: 25
//! tags: ["a", "b"]
//! limits: { connections: 100, idle_seconds: 30 }
//! ```
//!
//! - [`parse`] / [`parse_file`] give the [`Value`] tree.
//! - [`from_str`] / [`from_value`] / [`from_file`] hydrate a
//!   `#[derive(Deserialize)]` struct; [`to_string`] / [`to_string_pretty`]
//!   / [`to_value`] write one back.
//! - [`encode`] / [`encode_pretty`] write a [`Value`] as text that reads
//!   back unchanged.
//! - [`to_json`] / [`from_json`] convert to and from `serde_json::Value`.
//!
//! ## Grammar
//!
//! A document is a top-level map body (`key: value` pairs separated by
//! newlines or commas), a `{ map }`, a `[ list ]`, or empty (an empty map).
//! Inside braces and brackets, entries are separated by commas; a trailing
//! comma is allowed; newlines are insignificant.
//!
//! Keys are bare identifiers (`[A-Za-z_][A-Za-z0-9_]*`) or quoted strings.
//! A key may appear once per map.
//!
//! Values are `nil`, `true`, `false`, a number, a string, a list or a map.
//! A bare identifier in value position is a string (`mode: fs`).
//!
//! Numbers are `f64`: decimal with optional fraction and exponent, `_`
//! digit separators, or `0x` / `0o` / `0b` integers up to 2^53. A
//! multi-digit integer part may not start with `0`.
//!
//! Strings are `"…"` with the escapes `\n \t \r \e \" \\ \$ \~ \0 \a \b \f
//! \v \xHH \u{…} \uXXXX` (an unknown escape keeps its backslash; a `${…}`
//! or a leading `~`/`~/` is refused), `'…'` raw text (`\'` and `\\` only),
//! or a `<<TAG` heredoc whose body ends at a line holding only `TAG`.

mod de;
mod encode;
mod error;
mod json;
mod lexer;
mod parser;
mod ser;
mod value;

use std::path::Path;

pub use indexmap::IndexMap;

pub use de::{ValueDeserializer, from_file, from_str, from_value};
pub use encode::{encode, encode_pretty};
pub use error::{Error, ErrorKind, Result};
pub use json::{from_json, to_json};
pub use ser::{ValueSerializer, to_string, to_string_pretty, to_value};
pub use value::{Map, Value};

/// How deep lists and maps may nest. A deeper document is refused with
/// [`ErrorKind::Depth`] instead of overflowing the stack.
pub const MAX_DEPTH: usize = 200;

/// The largest integer magnitude that `f64` represents exactly (2^53).
/// Integer fields on both sides of the serde bridge refuse anything bigger.
pub(crate) const MAX_EXACT_INT: f64 = 9_007_199_254_740_992.0;

/// Parse a strict-data document.
pub fn parse(source: &str) -> Result<Value> {
    parser::parse(lexer::tokenize(source)?)
}

/// Read and parse the strict-data file at `path`. An unreadable file is
/// [`ErrorKind::Io`] naming the path.
pub fn parse_file(path: &Path) -> Result<Value> {
    let source = std::fs::read_to_string(path)
        .map_err(|e| Error::io(format!("failed to read {}: {e}", path.display())))?;
    parse(&source)
}

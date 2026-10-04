// SPDX-License-Identifier: MIT OR Apache-2.0
//! The colour gate: every colour a widget draws comes from the palette.
//! No file under `src/` other than `src/tokens.rs` (the built-in palettes
//! and the standalone defaults) may construct a `Color` from numbers or
//! name a fixed one, unless the exact line is in `ALLOWED`. The scanner is
//! checked against planted literals so it cannot silently stop biting.
use std::path::{Path, PathBuf};

/// The file that holds the palettes.
const PALETTES: &str = "src/tokens.rs";

/// Lines (file, trimmed line) that may construct a colour outside the
/// palettes. Empty: add an entry only with a reason beside it.
const ALLOWED: [(&str, &str); 0] = [];

/// Constructors and constants that make a colour from numbers.
const CONSTRUCTORS: [&str; 9] = [
    "Color::from_rgb(",
    "Color::from_rgba(",
    "Color::from_rgb8(",
    "Color::from_rgba8(",
    "Color::from_linear_rgba(",
    "Color::new(",
    "Color::WHITE",
    "Color::BLACK",
    "color!(",
];

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .expect("read dir")
        .map(|entry| entry.expect("dir entry").path())
        .collect();
    entries.sort();
    for entry in entries {
        if entry.is_dir() {
            rust_files(&entry, out);
        } else if entry.extension().is_some_and(|ext| ext == "rs") {
            out.push(entry);
        }
    }
}

/// `0x` followed by six or eight hex digits, or `#` followed by six.
fn has_hex_colour(line: &str) -> bool {
    let bytes = line.as_bytes();
    let hex_run = |from: usize| {
        bytes[from..]
            .iter()
            .take_while(|b| b.is_ascii_hexdigit())
            .count()
    };
    for (index, window) in bytes.windows(2).enumerate() {
        if window == b"0x" {
            let run = hex_run(index + 2);
            if run == 6 || run == 8 {
                return true;
            }
        }
    }
    bytes
        .iter()
        .enumerate()
        .any(|(index, byte)| *byte == b'#' && hex_run(index + 1) == 6)
}

/// The literal found on `line`, if any.
fn literal(line: &str) -> Option<&'static str> {
    CONSTRUCTORS
        .iter()
        .copied()
        .find(|needle| line.contains(needle))
        .or_else(|| has_hex_colour(line).then_some("hex colour"))
}

/// Violations in `text` as `(line number, what)`, skipping comments.
fn scan(text: &str) -> Vec<(usize, &'static str)> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim_start().starts_with("//"))
        .filter_map(|(index, line)| literal(line).map(|what| (index + 1, what)))
        .collect()
}

#[test]
fn no_colour_literal_outside_the_palettes() {
    let root = crate_root();
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    assert!(files.len() > 10, "scanned only {} files", files.len());
    let mut violations = Vec::new();
    let mut unused: Vec<_> = ALLOWED.iter().collect();
    for file in &files {
        let relative = file.strip_prefix(&root).unwrap_or(file);
        let relative = relative.to_string_lossy().replace('\\', "/");
        if relative == PALETTES {
            continue;
        }
        let text = std::fs::read_to_string(file).expect("read source");
        for (line, what) in scan(&text) {
            let trimmed = text.lines().nth(line - 1).unwrap_or_default().trim();
            if let Some(position) = unused
                .iter()
                .position(|(path, allowed)| *path == relative && *allowed == trimmed)
            {
                unused.remove(position);
                continue;
            }
            violations.push(format!("{relative}:{line}: {what}: {trimmed}"));
        }
    }
    assert!(
        violations.is_empty(),
        "colour literals outside {PALETTES} (derive from the palette instead):\n{}",
        violations.join("\n")
    );
    assert!(unused.is_empty(), "stale ALLOWED entries: {unused:?}");
    assert!(
        files.iter().any(|file| file.ends_with(PALETTES)),
        "{PALETTES} missing"
    );
}

/// The gate bites: every kind of literal it is meant to catch is caught in
/// a planted fixture, and palette-derived code is not.
#[test]
fn planted_literals_are_caught() {
    let planted = "\
let a = Color::from_rgb8(10, 20, 30);
let b = Color::from_rgb(0.1, 0.2, 0.3);
let c = Color::from_rgba(0.1, 0.2, 0.3, 1.0);
let d = Color::from_rgba8(1, 2, 3, 255);
let e = Color::from_linear_rgba(0.5, 0.5, 0.5, 1.0);
let f = Color::new(0.1, 0.2, 0.3, 1.0);
let g = Color::WHITE;
let h = Color::BLACK;
let i = color!(0x12abef);
let j = 0x3366ccff;
let k = \"#aabbcc\";
// let comment = Color::from_rgb8(1, 2, 3);
";
    let found = scan(planted);
    assert_eq!(found.len(), 11, "{found:?}");
    assert_eq!(found[0], (1, "Color::from_rgb8("));
    assert_eq!(found[8], (9, "color!("));
    assert_eq!(found[9], (10, "hex colour"));
    assert_eq!(found[10], (11, "hex colour"));

    let clean = "\
let fill = palette.primary.mix(palette.text, 0.1);
let clear = Color::TRANSPARENT;
let alpha = Color { a: 0.5, ..palette.selection };
let not_a_colour = 0x1234;
let id = 0xdeadbeefcafe;
let anchor = \"#section\";
";
    assert!(scan(clean).is_empty(), "{:?}", scan(clean));
}

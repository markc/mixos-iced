//! The scene documents this crate's tests pin. They are exported so a
//! host's tests can share them instead of carrying their own copies; the
//! files live in `tests/fixtures/`.

/// A clipboard panel: every family, a list with a row template and cell
/// substitution, a hidden list, headers with `window`, `subscribe` and
/// `targets`.
pub const CLIPPANEL: &str = include_str!("../tests/fixtures/clippanel.scene.mix");

/// One node of every family with every v0 port authored.
pub const CONFORMANCE: &str = include_str!("../tests/fixtures/conformance.scene.mix");

/// Two text nodes in a column, no window header.
pub const STATIC: &str = include_str!("../tests/fixtures/static.scene.mix");

/// Every fixture by its file stem.
pub const ALL: [(&str, &str); 3] = [
    ("clippanel", CLIPPANEL),
    ("conformance", CONFORMANCE),
    ("static", STATIC),
];

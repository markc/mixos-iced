// SPDX-License-Identifier: MIT OR Apache-2.0
//! The window's colours and text now come from the SHARED appearance
//! settings, borrowed from the settings session's prepared presentation (or
//! the immediate bootstrap before the first preparation). No per-app design
//! compilation: a colour literal in an app is still a bug, and the shared
//! path is the one that satisfies the desktop theming rule for every app.
//!
//! What this does NOT colour is the grid. A terminal's cell colours belong to
//! the program running in it (rio's ANSI palette, resolved in
//! `term_core::terminal::capture`); repainting those from the desktop theme
//! would make `ls --color` lie. The tokens own the window: the surface behind
//! and around the grid texture.

use appearance::settings::Prepared;
use toolkit::Tokens;

/// Resolved tokens for the terminal window, from the prepared presentation.
pub fn tokens(prepared: &Prepared) -> Tokens {
    prepared.tokens()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The immediate bootstrap must resolve, or every terminal runs on the
    /// preview palette and the theming rule is decoration.
    #[test]
    fn the_shared_bootstrap_resolves_tokens_for_this_app() {
        let prepared = appearance::settings::bootstrap().expect("shared bootstrap");
        let resolved = tokens(&prepared);
        assert_ne!(resolved.palette.surface, resolved.palette.text);
    }
}

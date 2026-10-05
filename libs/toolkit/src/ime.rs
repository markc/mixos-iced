// SPDX-License-Identifier: MIT OR Apache-2.0
//! Input-method composition ownership: [`Composition`], a small guard that
//! keeps an IME preedit attached to the surface that opened it, across
//! focus changes, until the runtime acknowledges the close.
//!
//! Wayland's input-method protocol has a window between an app cancelling
//! composition and the compositor confirming `done`: in that window queued
//! preedits and commits still arrive. Without a guard they attach to
//! whatever surface has focus now, so text lands in the wrong pane. The
//! guard answers one question — *may this event touch composition state?*
//! — and stays disabled until a real `Closed` arrives, because a stale
//! `Opened` must not re-enable it.
//!
//! The owner is the caller's own id (a pane, a tab, a surface); the
//! methods line up with `zwp_input_method` events:
//!
//! - `opened` / `closed` — enable state, `zwp_input_panel` surface state;
//! - `preedit(owner)` — a preedit string may be shown by `owner` alone;
//! - `commit(owner)` — a commit is accepted only from the owner (or before
//!   any preedit claimed one), and clears the owner;
//! - `focus(owner)` / `cancel` — focus moves cancel composition and
//!   disable the guard until `closed` re-enables it.

/// Composition ownership for one IME-enabled surface group.
///
/// The id type is whatever identifies the focusable surfaces sharing one
/// input-method seat (a pane id, a tab index). `Default` is the closed,
/// idle state.
#[derive(Debug, Clone)]
pub struct Composition<Id = u64> {
    owner: Option<Id>,
    open: bool,
    resetting: bool,
}

impl<Id> Default for Composition<Id> {
    fn default() -> Self {
        Self { owner: None, open: false, resetting: false }
    }
}

impl<Id: Clone + PartialEq> Composition<Id> {
    /// Whether any surface currently owns composition.
    pub fn has_owner(&self) -> bool {
        self.owner.is_some()
    }

    /// Whether composition events are considered at all; `false` from a
    /// `cancel` until the matching `closed`.
    pub fn enabled(&self) -> bool {
        !self.resetting
    }

    /// The composition surface opened.
    pub fn opened(&mut self) {
        if !self.resetting {
            self.open = true;
        }
    }

    /// The composition surface closed: back to the idle state.
    pub fn closed(&mut self) {
        *self = Self::default();
    }

    /// A preedit belongs to `active` only while it owns composition.
    /// Returns `true` when the preedit may be shown.
    pub fn preedit(&mut self, active: Option<Id>) -> bool {
        if self.focus(active.clone()) {
            return false;
        }
        if !self.open || self.resetting || active.is_none() {
            return false;
        }
        if self.owner.is_none() {
            self.owner = active.clone();
        }
        self.owner == active
    }

    /// A commit is accepted from the owner — or from any surface before a
    /// preedit claimed one — and releases ownership. Returns `true` when
    /// the commit may be applied.
    pub fn commit(&mut self, active: Option<Id>) -> bool {
        if self.focus(active.clone()) {
            return false;
        }
        let accepted = self.open
            && !self.resetting
            && active.is_some()
            && self.owner.as_ref().is_none_or(|owner| Some(owner) == active.as_ref());
        self.owner = None;
        accepted
    }

    /// Focus moved to `active`. Returns `true` when that move cancelled a
    /// composition owned by another surface.
    pub fn focus(&mut self, active: Option<Id>) -> bool {
        if self.owner.is_some() && self.owner != active {
            self.cancel();
            true
        } else {
            false
        }
    }

    /// Cancel composition: disabled until the runtime's `Closed` arrives,
    /// so queued preedits and commits cannot attach to the newly focused
    /// surface.
    pub fn cancel(&mut self) {
        self.resetting |= self.open;
        self.open = false;
        self.owner = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_change_close_and_return_cannot_redirect_composition() {
        for next in [Some(2), None] {
            // Other pane/tab, or owner closed.
            let mut ime = Composition::default();
            ime.opened();
            assert!(ime.preedit(Some(1)));
            assert!(!ime.focus(Some(1)));
            assert!(ime.focus(next));
            assert!(!ime.enabled());
            assert!(!ime.commit(next));
            assert!(!ime.preedit(next));
            ime.opened(); // A stale Opened does not undo the reset.
            assert!(!ime.commit(Some(1))); // Even after switching back.
            ime.closed();
            assert!(ime.enabled());
            assert!(!ime.commit(Some(2)));
            ime.opened();
            assert!(ime.preedit(Some(2)));
            assert!(ime.commit(Some(2)));
        }
    }

    #[test]
    fn commit_checks_owner_even_before_focus_notification() {
        let mut ime = Composition::default();
        ime.opened();
        assert!(ime.preedit(Some(1)));
        assert!(!ime.commit(Some(2)));
        assert!(!ime.enabled());
        ime.closed();
        ime.opened();
        assert!(ime.commit(Some(2))); // Direct commit without preedit.
        assert!(ime.preedit(Some(2)));
        assert!(ime.preedit(Some(2))); // Including an empty preedit update.
        assert!(ime.commit(Some(2)));
    }
}

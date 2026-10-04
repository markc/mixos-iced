//! Per-window chrome state, and the one rule that keeps chrome idle: its
//! elements change (a new commit, so damage, so a repaint) ONLY when this state
//! changes. Nothing here ticks.

use crate::layout::{
    ButtonState, CaptionButton, ChromeLayout, ChromePart, DecoTheme, Focus, Vec2, vec2,
};

/// Everything the chrome's pixels depend on, besides the theme.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChromeState {
    pub focused: bool,
    /// The client's committed content size (logical px).
    pub content_size: (i32, i32),
    pub title: String,
    /// Maximised or tiled: square corners.
    pub maximized: bool,
    /// The caption button under the pointer, if any.
    pub hovered: Option<CaptionButton>,
    /// Whether the pointer is over the button cluster (mac's glyphs show on
    /// cluster hover).
    pub cluster_hovered: bool,
    /// The caption button held down, if any.
    pub pressed: Option<CaptionButton>,
}

impl ChromeState {
    pub fn focus(&self) -> Focus {
        if self.focused {
            Focus::Focused
        } else {
            Focus::Unfocused
        }
    }

    pub fn button_state(&self, button: CaptionButton) -> ButtonState {
        if self.pressed == Some(button) {
            ButtonState::Pressed
        } else if self.hovered == Some(button) {
            ButtonState::Hover
        } else {
            ButtonState::Idle
        }
    }

    pub fn layout(&self, theme: &DecoTheme) -> ChromeLayout {
        ChromeLayout::compute(theme, content_vec(self.content_size))
    }
}

pub fn content_vec((w, h): (i32, i32)) -> Vec2 {
    vec2(w.max(0) as f32, h.max(0) as f32)
}

/// A window's chrome: its state and a commit counter that moves exactly when
/// the state (or the theme generation) does. Elements carry the counter, so an
/// unchanged window yields no damage.
#[derive(Clone, Debug, Default)]
pub struct Chrome {
    state: ChromeState,
    theme_generation: u64,
    commit: u64,
}

impl Chrome {
    pub fn state(&self) -> &ChromeState {
        &self.state
    }

    /// The commit the chrome's elements report.
    pub fn commit(&self) -> u64 {
        self.commit
    }

    /// Apply `state` for theme generation `theme_generation`; returns whether
    /// anything changed (and the commit moved).
    pub fn update(&mut self, state: ChromeState, theme_generation: u64) -> bool {
        if state == self.state && theme_generation == self.theme_generation {
            return false;
        }
        self.state = state;
        self.theme_generation = theme_generation;
        self.commit = self.commit.wrapping_add(1);
        true
    }

    /// Pointer at `p` (window-local, the layout's frame space): set hover from
    /// the part under it. Returns whether that changed the chrome.
    pub fn hover(&mut self, layout: &ChromeLayout, p: Option<Vec2>, theme_generation: u64) -> bool {
        let mut next = self.state.clone();
        next.hovered = match p.map(|p| layout.hit_test(p)) {
            Some(ChromePart::Button(button)) => Some(button),
            _ => None,
        };
        next.cluster_hovered = p.is_some_and(|p| layout.button_cluster.contains(p));
        self.update(next, theme_generation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{ChromeStyle, Mode, Scheme, presets};

    fn theme() -> DecoTheme {
        presets::resolve(ChromeStyle::Mac, Scheme::Ocean, Mode::Light)
    }

    fn state() -> ChromeState {
        ChromeState {
            focused: true,
            content_size: (400, 300),
            title: "t".into(),
            ..Default::default()
        }
    }

    #[test]
    fn the_commit_moves_only_when_the_state_does() {
        let mut chrome = Chrome::default();
        assert!(chrome.update(state(), 0));
        let first = chrome.commit();
        assert!(!chrome.update(state(), 0), "same state: no change");
        assert_eq!(chrome.commit(), first, "no new commit, so no damage");
        let mut unfocused = state();
        unfocused.focused = false;
        assert!(chrome.update(unfocused.clone(), 0));
        assert_ne!(chrome.commit(), first);
        let second = chrome.commit();
        assert!(
            chrome.update(unfocused, 1),
            "a new theme generation repaints"
        );
        assert_ne!(chrome.commit(), second);
    }

    #[test]
    fn hover_changes_only_when_the_part_under_the_pointer_does() {
        let theme = theme();
        let mut chrome = Chrome::default();
        chrome.update(state(), 0);
        let layout = chrome.state().layout(&theme);
        let (close, rect) = layout.buttons[0];
        let inside = rect.center();
        assert!(chrome.hover(&layout, Some(inside), 0));
        assert_eq!(chrome.state().hovered, Some(close));
        let commit = chrome.commit();
        // Moving within the same button changes nothing.
        let nudged = vec2(inside.x + 0.5, inside.y);
        assert!(!chrome.hover(&layout, Some(nudged), 0));
        assert_eq!(chrome.commit(), commit);
        // Over the content: no hover.
        let content = layout.content.center();
        assert!(chrome.hover(&layout, Some(content), 0));
        assert_eq!(chrome.state().hovered, None);
        assert!(!chrome.state().cluster_hovered);
    }

    #[test]
    fn button_state_prefers_pressed_over_hover() {
        let mut s = state();
        s.hovered = Some(CaptionButton::Close);
        assert_eq!(s.button_state(CaptionButton::Close), ButtonState::Hover);
        s.pressed = Some(CaptionButton::Close);
        assert_eq!(s.button_state(CaptionButton::Close), ButtonState::Pressed);
        assert_eq!(s.button_state(CaptionButton::Minimize), ButtonState::Idle);
    }
}

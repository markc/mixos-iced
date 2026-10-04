//! Pointer presses on chrome, turned into the intents a client's own requests
//! would produce:
//!
//! - titlebar press: an interactive MOVE, or on a double-click a maximise
//!   toggle instead;
//! - resize band press: an interactive RESIZE with the xdg edge bits (refused
//!   while maximised);
//! - caption button: pressed on press, FIRED on release only if the release
//!   lands on the same button (pressing close and sliding off cancels).
//!
//! The host turns an [`Intent`] into the SurfaceEvent the matching client
//! request makes (`Interactive::Begin`, `WindowRequest::*`), so chrome adds no
//! path of its own into window management.

use crate::layout::{CaptionButton, ChromePart, ResizeEdge, Vec2};

/// The titlebar double-click window.
pub const DOUBLE_CLICK_MILLIS: u32 = 400;
/// The titlebar double-click slop (logical px).
pub const DOUBLE_CLICK_SLOP: f32 = 5.0;

/// What a chrome press or release asks the window manager to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Intent {
    /// Start the interactive move grab (`xdg_toplevel.move`).
    Move,
    /// Start the interactive resize grab with these `xdg_toplevel.resize_edge`
    /// bits (`xdg_toplevel.resize`).
    Resize {
        edges: u32,
    },
    Close,
    ToggleMaximize,
    Minimize,
}

/// `xdg_toplevel.resize_edge` for a chrome edge.
pub fn xdg_edges(edge: ResizeEdge) -> u32 {
    match edge {
        ResizeEdge::Top => 1,
        ResizeEdge::Bottom => 2,
        ResizeEdge::Left => 4,
        ResizeEdge::TopLeft => 5,
        ResizeEdge::BottomLeft => 6,
        ResizeEdge::Right => 8,
        ResizeEdge::TopRight => 9,
        ResizeEdge::BottomRight => 10,
    }
}

/// The cursor shape name (cursor-shape-v1 / xcursor) for a part, for the
/// host's cursor override while the pointer is over chrome.
pub fn cursor_name(part: ChromePart) -> Option<&'static str> {
    match part {
        ChromePart::Resize(edge) => Some(match edge {
            ResizeEdge::Top => "n-resize",
            ResizeEdge::Bottom => "s-resize",
            ResizeEdge::Left => "w-resize",
            ResizeEdge::Right => "e-resize",
            ResizeEdge::TopLeft => "nw-resize",
            ResizeEdge::TopRight => "ne-resize",
            ResizeEdge::BottomLeft => "sw-resize",
            ResizeEdge::BottomRight => "se-resize",
        }),
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Candidate<K> {
    window: K,
    position: Vec2,
    time: u32,
}

/// The seat's chrome click state, across windows (`K` names a window).
#[derive(Clone, Debug)]
pub struct Clicks<K> {
    /// The caption button held down, and on which window.
    pressed: Option<(K, CaptionButton)>,
    /// The last titlebar press, for the double-click.
    candidate: Option<Candidate<K>>,
}

impl<K> Default for Clicks<K> {
    fn default() -> Self {
        Clicks {
            pressed: None,
            candidate: None,
        }
    }
}

impl<K: Clone + PartialEq> Clicks<K> {
    /// A primary-button press on `part` of `window` at `position` (window-local
    /// logical), at event `time` (ms, wrapping). Returns the intent to start
    /// now, if any. A button press returns none: it is armed for
    /// [`release`](Self::release).
    pub fn press(
        &mut self,
        window: &K,
        part: ChromePart,
        position: Vec2,
        time: u32,
        maximized: bool,
    ) -> Option<Intent> {
        self.pressed = None;
        match part {
            ChromePart::TitlebarDrag => {
                let double = self.candidate.take().is_some_and(|c| {
                    let (dx, dy) = (position.x - c.position.x, position.y - c.position.y);
                    c.window == *window
                        && time.wrapping_sub(c.time) <= DOUBLE_CLICK_MILLIS
                        && dx * dx + dy * dy <= DOUBLE_CLICK_SLOP * DOUBLE_CLICK_SLOP
                });
                if double {
                    Some(Intent::ToggleMaximize)
                } else {
                    self.candidate = Some(Candidate {
                        window: window.clone(),
                        position,
                        time,
                    });
                    Some(Intent::Move)
                }
            }
            ChromePart::Resize(edge) => {
                self.candidate = None;
                (!maximized).then_some(Intent::Resize {
                    edges: xdg_edges(edge),
                })
            }
            ChromePart::Button(button) => {
                self.candidate = None;
                self.pressed = Some((window.clone(), button));
                None
            }
            ChromePart::Content | ChromePart::Outside => {
                self.candidate = None;
                None
            }
        }
    }

    /// The armed caption button and its window, if any.
    pub fn armed(&self) -> Option<(&K, CaptionButton)> {
        self.pressed.as_ref().map(|(w, b)| (w, *b))
    }

    /// The button held down, if any (for the pressed fill).
    pub fn pressed(&self, window: &K) -> Option<CaptionButton> {
        self.pressed
            .as_ref()
            .filter(|(w, _)| w == window)
            .map(|(_, b)| *b)
    }

    /// The primary button released over `part` of `window` (`None`: over
    /// something else). Fires the armed button only if it is the same button
    /// of the same window.
    pub fn release(&mut self, window: Option<&K>, part: ChromePart) -> Option<Intent> {
        let (armed_window, armed) = self.pressed.take()?;
        let same = window == Some(&armed_window) && part == ChromePart::Button(armed);
        same.then_some(match armed {
            CaptionButton::Close => Intent::Close,
            CaptionButton::Maximize => Intent::ToggleMaximize,
            CaptionButton::Minimize => Intent::Minimize,
        })
    }

    /// Forget every window `matches` names (see [`forget`](Self::forget)).
    pub fn forget_if(&mut self, matches: impl Fn(&K) -> bool) {
        if self.pressed.as_ref().is_some_and(|(w, _)| matches(w)) {
            self.pressed = None;
        }
        if self.candidate.as_ref().is_some_and(|c| matches(&c.window)) {
            self.candidate = None;
        }
    }

    /// Forget `window` (it closed or lost its chrome): no stale double-click or
    /// armed button survives it.
    pub fn forget(&mut self, window: &K) {
        if self.pressed.as_ref().is_some_and(|(w, _)| w == window) {
            self.pressed = None;
        }
        if self.candidate.as_ref().is_some_and(|c| c.window == *window) {
            self.candidate = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::vec2;

    const W: u32 = 1;
    const OTHER: u32 = 2;

    #[test]
    fn a_titlebar_press_moves_and_a_double_click_maximises_instead() {
        let mut clicks = Clicks::default();
        let p = vec2(50.0, 10.0);
        assert_eq!(
            clicks.press(&W, ChromePart::TitlebarDrag, p, 1000, false),
            Some(Intent::Move)
        );
        assert_eq!(
            clicks.press(&W, ChromePart::TitlebarDrag, vec2(53.0, 13.0), 1300, false),
            Some(Intent::ToggleMaximize)
        );
        // The pair is consumed: a third press starts over.
        assert_eq!(
            clicks.press(&W, ChromePart::TitlebarDrag, p, 1400, false),
            Some(Intent::Move)
        );
    }

    #[test]
    fn a_double_click_needs_the_same_window_time_and_place() {
        let p = vec2(50.0, 10.0);
        let mut clicks = Clicks::default();
        clicks.press(&W, ChromePart::TitlebarDrag, p, 1000, false);
        assert_eq!(
            clicks.press(&W, ChromePart::TitlebarDrag, p, 1401, false),
            Some(Intent::Move),
            "too slow"
        );
        let mut clicks = Clicks::default();
        clicks.press(&W, ChromePart::TitlebarDrag, p, 1000, false);
        assert_eq!(
            clicks.press(&W, ChromePart::TitlebarDrag, vec2(56.0, 10.0), 1100, false),
            Some(Intent::Move),
            "too far"
        );
        let mut clicks = Clicks::default();
        clicks.press(&W, ChromePart::TitlebarDrag, p, 1000, false);
        assert_eq!(
            clicks.press(&OTHER, ChromePart::TitlebarDrag, p, 1100, false),
            Some(Intent::Move),
            "other window"
        );
    }

    /// The double click uses event time, slop and wrapping arithmetic.
    #[test]
    fn the_double_click_window_survives_the_event_clock_wrapping() {
        let p = vec2(50.0, 10.0);
        let mut clicks = Clicks::default();
        clicks.press(&W, ChromePart::TitlebarDrag, p, u32::MAX - 100, false);
        assert_eq!(
            clicks.press(&W, ChromePart::TitlebarDrag, p, 200, false),
            Some(Intent::ToggleMaximize)
        );
    }

    #[test]
    fn edges_resize_with_their_xdg_bits_except_when_maximised() {
        let mut clicks = Clicks::<u32>::default();
        let part = ChromePart::Resize(ResizeEdge::BottomRight);
        assert_eq!(
            clicks.press(&W, part, vec2(0.0, 0.0), 0, false),
            Some(Intent::Resize { edges: 10 })
        );
        assert_eq!(clicks.press(&W, part, vec2(0.0, 0.0), 0, true), None);
        for (edge, bits) in [
            (ResizeEdge::Top, 1),
            (ResizeEdge::Bottom, 2),
            (ResizeEdge::Left, 4),
            (ResizeEdge::TopLeft, 5),
            (ResizeEdge::BottomLeft, 6),
            (ResizeEdge::Right, 8),
            (ResizeEdge::TopRight, 9),
            (ResizeEdge::BottomRight, 10),
        ] {
            assert_eq!(xdg_edges(edge), bits);
            assert!(cursor_name(ChromePart::Resize(edge)).is_some());
        }
    }

    /// Close fires only when released inside the original button.
    #[test]
    fn a_button_fires_only_on_release_inside_the_same_button() {
        let close = ChromePart::Button(CaptionButton::Close);
        let mut clicks = Clicks::default();
        assert_eq!(clicks.press(&W, close, vec2(0.0, 0.0), 0, false), None);
        assert_eq!(clicks.pressed(&W), Some(CaptionButton::Close));
        assert_eq!(
            clicks.release(Some(&W), ChromePart::TitlebarDrag),
            None,
            "slid off"
        );
        assert_eq!(clicks.pressed(&W), None, "a release disarms");

        clicks.press(&W, close, vec2(0.0, 0.0), 0, false);
        assert_eq!(
            clicks.release(Some(&W), ChromePart::Button(CaptionButton::Minimize)),
            None,
            "another button"
        );
        clicks.press(&W, close, vec2(0.0, 0.0), 0, false);
        assert_eq!(
            clicks.release(Some(&OTHER), close),
            None,
            "another window's close"
        );
        clicks.press(&W, close, vec2(0.0, 0.0), 0, false);
        assert_eq!(clicks.release(Some(&W), close), Some(Intent::Close));

        for (button, intent) in [
            (CaptionButton::Maximize, Intent::ToggleMaximize),
            (CaptionButton::Minimize, Intent::Minimize),
        ] {
            clicks.press(&W, ChromePart::Button(button), vec2(0.0, 0.0), 0, false);
            assert_eq!(
                clicks.release(Some(&W), ChromePart::Button(button)),
                Some(intent)
            );
        }
    }

    #[test]
    fn forgetting_a_window_drops_its_armed_button_and_double_click() {
        let mut clicks = Clicks::default();
        clicks.press(&W, ChromePart::TitlebarDrag, vec2(1.0, 1.0), 0, false);
        clicks.forget(&W);
        assert_eq!(
            clicks.press(&W, ChromePart::TitlebarDrag, vec2(1.0, 1.0), 10, false),
            Some(Intent::Move)
        );
        clicks.press(
            &W,
            ChromePart::Button(CaptionButton::Close),
            vec2(0.0, 0.0),
            0,
            false,
        );
        clicks.forget(&W);
        assert_eq!(
            clicks.release(Some(&W), ChromePart::Button(CaptionButton::Close)),
            None
        );
    }
}

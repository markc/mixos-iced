//! The off-thread publish → compositor wake handshake.
//!
//! Cross-thread wake for EVERY off-thread producer — the background shader
//! worker, the bevy worker and the iced worker. It lives beside the ring they
//! publish through (`publish.ring`) because it is the other half of the same
//! contract: the ring says WHICH buffer is finished, this says THAT one is.
//!
//! The compositor's redraw loop is sustained by flip -> vblank -> render, and an
//! undamaged frame queues no flip, so the loop stops. Once a producer renders off
//! the compositor thread, its publish is the ONLY event that can restart the loop
//! — hence a flag the redraw handler consults plus a ping to wake it.
//!
//! One flag and one waker for all three on purpose: the handler only needs to
//! know THAT something published, and a producer-specific flag would have to be
//! re-gated against tearing exclusivity separately.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

pub type Waker = Arc<dyn Fn() + Send + Sync>;

fn published() -> &'static AtomicBool {
    static SLOT: AtomicBool = AtomicBool::new(false);
    &SLOT
}

fn waker() -> &'static RwLock<Option<Waker>> {
    static SLOT: OnceLock<RwLock<Option<Waker>>> = OnceLock::new();
    SLOT.get_or_init(|| RwLock::new(None))
}

/// Kernel: install the redraw ping the workers wake the compositor through.
pub fn set_offthread_waker(f: Waker) {
    *waker().write().unwrap_or_else(|e| e.into_inner()) = Some(f);
}

/// Worker thread: a frame is complete and published.
pub fn notify_offthread_published() {
    published().store(true, Ordering::Release);
    let w = waker().read().unwrap_or_else(|e| e.into_inner()).clone();
    if let Some(f) = w {
        f();
    }
}

/// Compositor: has a producer published since we last looked? Clears the flag.
pub fn take_offthread_published() -> bool {
    published().swap(false, Ordering::Acquire)
}

// ── Producer deadlines ───────────────────────────────────────────────────────
//
// The other way a producer asks for a frame: not "I published" but "I will need
// one at this instant" — an iced animation's `RedrawRequest::At`. Before, such a
// producer reported itself dirty until the instant came, which re-rendered the
// same frame every vblank. Now it offers the instant here each frame; the
// backend takes the earliest after the frame and arms ONE timer for it, which
// requests a frame with reason `Iced` when it fires. A deadline that went stale
// (the animation settled first) costs one frame that finds nothing to draw.

fn deadline() -> &'static std::sync::Mutex<Option<std::time::Instant>> {
    static SLOT: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
    &SLOT
}

/// Producer (compositor thread): a frame is wanted at `at`. Keeps the earliest.
pub fn offer_deadline(at: std::time::Instant) {
    if let Ok(mut slot) = deadline().lock() {
        *slot = Some(slot.map_or(at, |current| current.min(at)));
    }
}

/// Backend, after a frame: the earliest deadline offered since the last take,
/// cleared.
pub fn take_deadline() -> Option<std::time::Instant> {
    deadline().lock().ok().and_then(|mut slot| slot.take())
}

// The same, for screen capture's time-based work (round-1 finding D): a video
// recording on a static screen gets no frames — a truly empty frame has nothing
// to encode — so its keep-alive (the "still recording?" prompt and its
// countdown) offers the instant it next needs a frame. A separate slot so the
// backend attributes the frame to `Capture`, not `Iced`.

fn capture_deadline() -> &'static std::sync::Mutex<Option<std::time::Instant>> {
    static SLOT: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
    &SLOT
}

/// Capture (compositor thread): a frame is wanted at `at`. Keeps the earliest.
pub fn offer_capture_deadline(at: std::time::Instant) {
    if let Ok(mut slot) = capture_deadline().lock() {
        *slot = Some(slot.map_or(at, |current| current.min(at)));
    }
}

/// Backend, after a frame: capture's earliest offered deadline, cleared.
pub fn take_capture_deadline() -> Option<std::time::Instant> {
    capture_deadline().lock().ok().and_then(|mut slot| slot.take())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn deadline_keeps_the_earliest_offer_and_clears_on_take() {
        // The only test touching the slot, so the assertions run in order.
        let _ = take_deadline();
        let now = Instant::now();
        offer_deadline(now + Duration::from_millis(30));
        offer_deadline(now + Duration::from_millis(10));
        offer_deadline(now + Duration::from_millis(20));
        assert_eq!(take_deadline(), Some(now + Duration::from_millis(10)));
        assert_eq!(take_deadline(), None, "a take clears the slot");
    }

    #[test]
    fn capture_deadlines_are_their_own_slot() {
        // The only test touching the capture slot.
        let _ = take_capture_deadline();
        let now = Instant::now();
        offer_capture_deadline(now + Duration::from_secs(1));
        offer_capture_deadline(now + Duration::from_secs(300));
        assert_eq!(take_capture_deadline(), Some(now + Duration::from_secs(1)));
        assert_eq!(take_capture_deadline(), None);
    }
}

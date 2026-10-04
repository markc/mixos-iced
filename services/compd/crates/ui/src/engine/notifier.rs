//! Lazy-redraw signaling.
//!
//! Iced's `iced_graphics::shell::Notifier` is the callback Iced uses to
//! tell the host "I want to redraw" or "my layout invalidated, drop the
//! cache." Record the flags before waking the compositor's iced event source.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use iced_graphics::shell::Notifier;

/// Lazy-redraw flags. Cheap to clone; the inner `Arc`s are shared between
/// the runtime (which reads + clears them) and the `RuntimeNotifier` handed
/// to iced_wgpu (which sets them).
#[derive(Debug, Clone, Default)]
pub struct DirtyFlags {
    /// "Please redraw at the next opportunity." Set by Iced when widgets
    /// signal they need a repaint; cleared by the runtime after rendering.
    pub redraw: Arc<AtomicBool>,
    /// "Layout cache is stale; rebuild it from scratch." Used by widgets
    /// that mutate their own size (rare).
    pub invalidate_layout: Arc<AtomicBool>,
}

impl DirtyFlags {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read + clear the redraw flag. True if a redraw is pending.
    pub fn take_redraw(&self) -> bool {
        self.redraw.swap(false, Ordering::AcqRel)
    }

    /// Read + clear the invalidate flag.
    pub fn take_invalidate(&self) -> bool {
        self.invalidate_layout.swap(false, Ordering::AcqRel)
    }

    /// Read without clearing.
    pub fn redraw_pending(&self) -> bool {
        self.redraw.load(Ordering::Acquire)
    }

    /// Manually request a redraw (e.g., the compositor decided something
    /// changed externally and the UI should re-paint even though no Iced
    /// event flowed through).
    pub fn request_redraw(&self) {
        self.redraw.store(true, Ordering::Release);
        super::wake::notify();
    }
}

/// `Notifier` implementation handed to `iced_wgpu::Engine`. Sets the flags
/// in `DirtyFlags` when Iced internally requests a redraw or invalidation.
pub struct RuntimeNotifier {
    pub flags: DirtyFlags,
}

impl RuntimeNotifier {
    pub fn new(flags: DirtyFlags) -> Self {
        Self { flags }
    }
}

impl Notifier for RuntimeNotifier {
    fn tick(&self) {
        // iced_wgpu's per-frame tick. We treat it as "redraw at next opportunity"
        // — the host's frame loop drives actual redraws.
        self.flags.redraw.store(true, Ordering::Relaxed);
    }

    fn request_redraw(&self) {
        self.flags.request_redraw();
    }

    fn invalidate_layout(&self) {
        self.flags.invalidate_layout.store(true, Ordering::Release);
        self.flags.request_redraw();
    }
}

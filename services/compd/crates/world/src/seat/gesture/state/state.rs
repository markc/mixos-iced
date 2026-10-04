/// Per-seat touchpad swipe accumulator. A libinput swipe spans Begin→Update*→End
/// events across separate input dispatches, so the running delta + finger count
/// live here (one field on the Orchestrator). World-agnostic by design: it holds
/// only the raw 2D delta and finger count; the navigator semantics (angle → view)
/// are applied by the swipe handler at end-of-swipe.
#[derive(Default)]
pub struct GestureAccumulator {
    pub fingers: u32,
    pub acc_x: f64,
    pub acc_y: f64,
    pub active: bool,
    /// Last absolute pinch scale seen (relative to pinch begin = 1.0). Used to
    /// derive the per-update incremental factor (`scale / pinch_prev_scale`).
    pub pinch_prev_scale: f64,
    /// Pinch ownership latched at begin: `true` means the gesture is forwarded to
    /// the focused window client; `false` means the canvas zooms.
    pub pinch_to_window: bool,
    /// Finger count latched at pinch begin. A four-finger pinch is a discrete
    /// window command (fit one window / fit all) rather than a continuous zoom.
    pub pinch_fingers: u32,
    /// Latest absolute pinch scale (relative to begin) — read at end to decide a
    /// four-finger pinch's direction (in vs. out).
    pub pinch_scale: f64,
}

impl GestureAccumulator {
    /// A swipe started: latch the finger count and zero the running delta.
    pub fn begin(&mut self, fingers: u32) {
        self.fingers = fingers;
        self.acc_x = 0.0;
        self.acc_y = 0.0;
        self.active = true;
    }

    /// Accumulate one update's delta (libinput logical units; y is screen-down).
    pub fn update(&mut self, dx: f64, dy: f64) {
        if self.active {
            self.acc_x += dx;
            self.acc_y += dy;
        }
    }

    /// Magnitude of the accumulated swipe vector.
    pub fn magnitude(&self) -> f64 {
        (self.acc_x * self.acc_x + self.acc_y * self.acc_y).sqrt()
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

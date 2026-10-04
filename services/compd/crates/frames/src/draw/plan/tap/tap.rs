//! Tap identity + subscription registry.
//!
//! A tap is a named point between frame passes where the rendered state may be
//! observed. Consumers (capture pipes, future screencast) subscribe to a tap by
//! identity and never learn what the surrounding passes mean. Capture adaptation
//! is out of scope for now; the identity type is what the frame plan carries.

/// Identity of a tap point. Compared by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TapPoint(pub &'static str);

/// The tap placed after the scene pass and before any lock pass.
/// This is where the capture registry observes today (it must never see lock content).
pub const POST_SCENE: TapPoint = TapPoint("post-scene");

/// The tap observing the final composited frame (everything, lock included).
pub const FINAL: TapPoint = TapPoint("final");

/// Minimal subscription bookkeeping. Executors ask `is_active` before paying
/// the cost of a blit; consumers flip subscriptions on and off by identity.
#[derive(Debug, Default)]
pub struct TapSubscriptions {
    active: Vec<TapPoint>,
}

impl TapSubscriptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn subscribe(&mut self, tap: TapPoint) {
        if !self.active.contains(&tap) {
            info!("tap: subscribed to tap point {tap:?}");
            self.active.push(tap);
        }
    }

    pub fn unsubscribe(&mut self, tap: TapPoint) {
        self.active.retain(|t| *t != tap);
    }

    pub fn is_active(&self, tap: TapPoint) -> bool {
        self.active.contains(&tap)
    }
}

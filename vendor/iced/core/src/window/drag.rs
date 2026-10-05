//! Native drag and drop. Tokens identify a compositor-authorised
//! pointer press or offer. Backends validate origin, seat and liveness.
use std::sync::Arc;

/// A pointer press that can authorise one drag from its originating window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Gesture(pub u64);

/// An offer addressed to a particular window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Offer(pub u64);

/// A negotiated operation. Move completes only after the target finishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Copy,
    Move,
}

/// Source or target capabilities. At least one action must be enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Actions {
    pub copy: bool,
    pub move_: bool,
}
impl Actions {
    pub const COPY: Self = Self {
        copy: true,
        move_: false,
    };
    pub const MOVE: Self = Self {
        copy: false,
        move_: true,
    };
    pub const BOTH: Self = Self {
        copy: true,
        move_: true,
    };
    pub fn contains(self, action: Action) -> bool {
        match action {
            Action::Copy => self.copy,
            Action::Move => self.move_,
        }
    }
}

/// A bounded MIME payload supplied before starting the native drag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub mime: String,
    pub bytes: Arc<[u8]>,
    pub actions: Actions,
}
impl Source {
    pub const MAX_BYTES: usize = 16 * 1024 * 1024;
    pub fn valid(&self) -> bool {
        !self.mime.is_empty()
            && self.mime.len() <= 255
            && !self.mime.chars().any(|c| c.is_control())
            && self.bytes.len() <= Self::MAX_BYTES
            && (self.actions.copy || self.actions.move_)
    }
}

/// Queued operations are confirmed or rejected through window events.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Sent immediately before the corresponding left-button press.
    Gesture(Gesture),
    Started(Gesture),
    Rejected(Gesture),
    Enter {
        offer: Offer,
        position: crate::Point,
        mimes: Vec<String>,
    },
    Motion {
        offer: Offer,
        position: crate::Point,
    },
    Action {
        offer: Offer,
        action: Option<Action>,
    },
    Leave(Offer),
    Drop(Offer),
    /// Bytes are delivered once, at EOF. The caller must decode/apply them
    /// before calling finish_drag_offer; a physical drop alone is insufficient.
    Data {
        offer: Offer,
        mime: String,
        bytes: Arc<[u8]>,
        action: Action,
    },
    Failed(Offer),
    Finished {
        gesture: Gesture,
        action: Action,
    },
    Cancelled(Gesture),
}

/// Backend is unavailable, or the queued request itself is invalid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Unsupported,
    Invalid,
}

/// A request queued onto the owning native window backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Start(Gesture, Source),
    Accept(Offer, Option<String>, Actions, Action),
    Receive(Offer, String),
    Finish(Offer, bool),
    Cancel(Gesture),
}

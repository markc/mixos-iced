//! Native Wayland drag and drop. Tokens identify a compositor-authorised
//! pointer press or an offer on this event loop. The backend validates their
//! ownership and lifecycle before accepting adapter requests.
use std::sync::Arc;

/// A pointer press that can authorise one drag from its originating window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Gesture(pub(crate) u64);
impl Gesture {
    /// Opaque identity for adapters. Start requests still require a held press
    /// from the requesting window on this event loop.
    pub fn token(self) -> u64 {
        self.0
    }
    /// Reconstitute an adapter token; the backend validates its ownership.
    pub fn from_token(token: u64) -> Self {
        Self(token)
    }
}

/// An offer addressed to a particular window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Offer(pub(crate) u64);
impl Offer {
    /// Opaque identity for adapters; offers are validated against their window.
    pub fn token(self) -> u64 {
        self.0
    }
    /// Reconstitute an adapter token; the backend validates its ownership.
    pub fn from_token(token: u64) -> Self {
        Self(token)
    }
}

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
    pub const COPY: Self = Self { copy: true, move_: false };
    pub const MOVE: Self = Self { copy: false, move_: true };
    pub const BOTH: Self = Self { copy: true, move_: true };
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
    pub(crate) fn valid(&self) -> bool {
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
        position: crate::dpi::LogicalPosition<f64>,
        mimes: Vec<String>,
    },
    Motion {
        offer: Offer,
        position: crate::dpi::LogicalPosition<f64>,
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_source_refuses_invalid_mime_actions_and_oversized_bytes() {
        let mut source = Source {
            mime: "text/plain;charset=utf-8".into(),
            bytes: Arc::from(&b"text"[..]),
            actions: Actions::BOTH,
        };
        assert!(source.valid());
        for mime in ["", "text/plain\0extra", "text/plain\nheader"] {
            source.mime = mime.into();
            assert!(!source.valid());
        }
        source.mime = "x".repeat(256);
        assert!(!source.valid());
        source.mime = "text/plain".into();
        source.actions = Actions { copy: false, move_: false };
        assert!(!source.valid());
        source.actions = Actions::COPY;
        source.bytes = Arc::from(vec![0; Source::MAX_BYTES + 1]);
        assert!(!source.valid());
    }
}

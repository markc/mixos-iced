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
    /// Retain the source after delivery.
    Copy,
    /// Remove the source after successful native completion.
    Move,
}

/// Source or target capabilities. At least one action must be enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Actions {
    /// Allow copying.
    pub copy: bool,
    /// Allow moving.
    pub move_: bool,
}
impl Actions {
    /// Copy only.
    pub const COPY: Self = Self {
        copy: true,
        move_: false,
    };
    /// Move only.
    pub const MOVE: Self = Self {
        copy: false,
        move_: true,
    };
    /// Copy or Move.
    pub const BOTH: Self = Self {
        copy: true,
        move_: true,
    };
    /// Whether an action belongs to this set.
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
    /// Offered MIME type, at most 255 bytes with no control characters.
    pub mime: String,
    /// Immutable encoded payload.
    pub bytes: Arc<[u8]>,
    /// Supported operations.
    pub actions: Actions,
}
impl Source {
    /// Maximum encoded size for a native transfer.
    pub const MAX_BYTES: usize = 16 * 1024 * 1024;
    /// Validate bounded MIME, bytes and supported actions.
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
    /// The native backend has issued start_drag for a valid held press.
    Started(Gesture),
    /// The press was stale, released or belonged to another window.
    Rejected(Gesture),
    /// An offered payload entered the target window.
    Enter {
        /// Opaque offer identity.
        offer: Offer,
        /// Position in the target's logical coordinates.
        position: crate::Point,
        /// Offered MIME types.
        mimes: Vec<String>,
    },
    /// The offered pointer moved within the target window.
    Motion {
        /// Opaque offer identity.
        offer: Offer,
        /// Position in the target's logical coordinates.
        position: crate::Point,
    },
    /// The compositor selected an operation, or no matching operation.
    Action {
        /// Opaque offer identity.
        offer: Offer,
        /// Current negotiated operation.
        action: Option<Action>,
    },
    /// An undropped offer left the target.
    Leave(Offer),
    /// A physical drop occurred. This does not confirm transfer success.
    Drop(Offer),
    /// Bytes are delivered once, at EOF. The caller must decode/apply them
    /// before calling finish_drag_offer; a physical drop alone is insufficient.
    Data {
        /// Opaque offer identity.
        offer: Offer,
        /// Received MIME type.
        mime: String,
        /// Complete bytes received at EOF.
        bytes: Arc<[u8]>,
        /// Negotiated operation.
        action: Action,
    },
    /// A transfer or offer request failed.
    Failed(Offer),
    /// The target acknowledged success through native dnd_finished.
    Finished {
        /// The source's consumed press token.
        gesture: Gesture,
        /// Completed operation.
        action: Action,
    },
    /// The source was cancelled, without successful completion.
    Cancelled(Gesture),
}

/// Backend is unavailable, or the queued request itself is invalid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// This backend does not implement native DnD.
    Unsupported,
    /// A malformed request or unavailable window.
    Invalid,
}

/// A request queued onto the owning native window backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Start from a still-held press belonging to this window.
    Start(Gesture, Source),
    /// Choose or reject a MIME type and negotiate target actions.
    Accept(Offer, Option<String>, Actions, Action),
    /// Receive a dropped offer through its native pipe.
    Receive(Offer, String),
    /// Acknowledge application success, or reject the dropped offer.
    Finish(Offer, bool),
    /// Cancel the source associated with this press.
    Cancel(Gesture),
}

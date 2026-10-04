//! Selection provenance (the compositor side of the smithay selection hook).
//!
//! The `SelectionHandler::SelectionUserData` every compositor-owned selection
//! carries. A bare `u64` capture generation (with `u64::MAX` as an "owned by
//! X11" sentinel) is not enough: the cross-seat relay needs to know WHERE a
//! selection came from, so that re-mirroring a persisted or relayed selection
//! never bounces back as a fresh copy. One type carries both: the origin and
//! the generation.
//!
//! This lives at layer 0 so `dispatcher` (the `SelectionHandler` impl), the
//! clipboard persistence path and the Bus relay share one definition. The switch
//! from `u64` belongs to the crate that implements `SelectionHandler`.

/// Which seat a client selection was made on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SeatKind {
    /// The input seat (`seat0`).
    Primary,
    /// The agent seat (`agent`).
    Agent,
}

/// Where a compositor-held selection came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SelectionOrigin {
    /// Mirrored from a client selection on this seat.
    Seat(SeatKind),
    /// Owned by an X11 client, served through the XWM (the old `u64::MAX` sentinel).
    X11,
    /// The compositor's persisted copy, installed by `selection_source_destroyed`
    /// after the owning client went away.
    Persisted,
    /// Re-mirrored by the compositor from another seat's compositor-owned
    /// selection. The relay must not re-mirror this one again.
    Relay,
}

/// The user data on every compositor-provided selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SelectionUserData {
    pub origin: SelectionOrigin,
    /// The capture generation the selection was installed against, so a read that
    /// raced a newer copy is refused instead of served stale.
    pub generation: u64,
}

impl SelectionUserData {
    pub fn new(origin: SelectionOrigin, generation: u64) -> Self {
        Self { origin, generation }
    }

    /// Whether the cross-seat relay may mirror this selection to another seat:
    /// a relayed copy never is, so two seats cannot ping-pong one selection.
    pub fn relayable(&self) -> bool {
        self.origin != SelectionOrigin::Relay
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relayed_selections_are_not_relayed_again() {
        assert!(SelectionUserData::new(SelectionOrigin::Seat(SeatKind::Primary), 1).relayable());
        assert!(SelectionUserData::new(SelectionOrigin::Persisted, 2).relayable());
        assert!(SelectionUserData::new(SelectionOrigin::X11, 3).relayable());
        assert!(!SelectionUserData::new(SelectionOrigin::Relay, 4).relayable());
    }
}

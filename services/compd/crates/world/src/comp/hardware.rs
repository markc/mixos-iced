// SPDX-License-Identifier: MIT OR Apache-2.0
//! Bounded observations from the native libseat/libinput owners only.
//! No key values are retained. These facts prove native event arrival/routing,
//! not audible sound, application delivery or a device's physical provenance.

use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Keyboard,
    Pointer,
    Paused,
    Active,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Keyboard => "keyboard",
            Self::Pointer => "pointer",
            Self::Paused => "paused",
            Self::Active => "active",
        }
    }
    fn index(self) -> usize {
        match self {
            Self::Keyboard => 0,
            Self::Pointer => 1,
            Self::Paused => 2,
            Self::Active => 3,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Event {
    pub sequence: u64,
    pub device: Option<String>,
    observed_at: Instant,
}

#[derive(Default)]
pub struct Witness {
    sequence: u64,
    events: [Option<Event>; 4],
}

impl Witness {
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
    pub fn latest(&self, kind: Kind) -> Option<&Event> {
        self.events[kind.index()].as_ref()
    }
    pub fn latest_before(&self, kind: Kind, after: u64, deadline: Instant) -> Option<&Event> {
        self.latest(kind)
            .filter(|event| event.sequence > after && event.observed_at <= deadline)
    }
    pub fn note(&mut self, kind: Kind, device: Option<&str>) {
        self.note_at(kind, device, Instant::now());
    }
    fn note_at(&mut self, kind: Kind, device: Option<&str>, observed_at: Instant) {
        let Some(sequence) = self.sequence.checked_add(1) else {
            return;
        };
        self.sequence = sequence;
        let device = device
            .filter(|name| {
                name.len() <= 128
                    && name.strip_prefix("event").is_some_and(|number| {
                        !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
                    })
            })
            .map(str::to_owned);
        self.events[kind.index()] = Some(Event {
            sequence,
            device,
            observed_at,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_edges_survive_other_kinds_without_unbounded_history() {
        let mut witness = Witness::default();
        assert!(witness.latest(Kind::Paused).is_none());
        witness.note(Kind::Paused, None);
        for _ in 0..10000 {
            witness.note(Kind::Pointer, Some("event7"));
        }
        assert_eq!(witness.latest(Kind::Paused).unwrap().sequence, 1);
        assert_eq!(witness.latest(Kind::Pointer).unwrap().sequence, 10001);
        witness.note(Kind::Active, None);
        assert!(
            witness.latest(Kind::Active).unwrap().sequence
                > witness.latest(Kind::Paused).unwrap().sequence
        );
        assert!(witness.latest(Kind::Keyboard).is_none());
    }
    #[test]
    fn exhausted_sequences_never_replay_and_device_bytes_are_bounded() {
        let mut witness = Witness::default();
        witness.note(Kind::Keyboard, Some(&"x".repeat(1000)));
        assert!(witness.latest(Kind::Keyboard).unwrap().device.is_none());
        witness.sequence = u64::MAX;
        witness.note(Kind::Keyboard, Some("replacement"));
        assert_eq!(witness.latest(Kind::Keyboard).unwrap().sequence, 1);
    }
    #[test]
    fn malformed_devices_keep_the_edge_without_substituting_an_identity() {
        let mut witness = Witness::default();
        witness.note(Kind::Keyboard, Some("event007"));
        assert_eq!(
            witness.latest(Kind::Keyboard).unwrap().device.as_deref(),
            Some("event007")
        );
        let prefix = format!("event{}", "1".repeat(123));
        for invalid in [
            format!("{prefix}2"),
            format!("{prefix}3"),
            "💡".repeat(128),
            "event".into(),
            "event7\n".into(),
            "event７".into(),
            "../event7".into(),
        ] {
            let after = witness.sequence();
            witness.note(Kind::Keyboard, Some(&invalid));
            let event = witness.latest(Kind::Keyboard).unwrap();
            assert_eq!(event.sequence, after + 1);
            assert!(
                event.device.is_none(),
                "must not substitute a prefix for {invalid:?}"
            );
        }
        witness.note(Kind::Pointer, Some(&prefix));
        assert_eq!(
            witness.latest(Kind::Pointer).unwrap().device.as_deref(),
            Some(prefix.as_str())
        );
    }
    #[test]
    fn delayed_settlement_only_accepts_edges_observed_by_the_deadline() {
        use std::time::Duration;
        let admitted = Instant::now();
        let deadline = admitted + Duration::from_millis(100);
        let mut witness = Witness::default();
        assert!(witness.latest_before(Kind::Keyboard, 0, deadline).is_none());
        witness.note_at(
            Kind::Keyboard,
            Some("event7"),
            admitted - Duration::from_millis(1),
        );
        assert!(
            witness.latest_before(Kind::Keyboard, 0, deadline).is_some(),
            "snapshot-to-admission edge survives"
        );
        assert!(
            witness.latest_before(Kind::Keyboard, 1, deadline).is_none(),
            "old edge does not replay"
        );
        witness.note_at(Kind::Keyboard, Some("event7"), deadline);
        assert!(
            witness.latest_before(Kind::Keyboard, 1, deadline).is_some(),
            "deadline edge survives delayed settlement"
        );
        witness.note_at(
            Kind::Pointer,
            Some("event8"),
            deadline + Duration::from_millis(1),
        );
        assert!(
            witness.latest_before(Kind::Pointer, 0, deadline).is_none(),
            "late edge cannot produce PASS"
        );
    }
}

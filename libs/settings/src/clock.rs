// SPDX-License-Identifier: MIT OR Apache-2.0
//! One host CLOCK_MONOTONIC domain, explicitly fenced by boot identity.
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    pub boot_id: String,
    pub clock_id: u32,
    pub nanoseconds: u64,
}

/// Optional observation envelope, never part of snapshot or durable receipts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Commit {
    pub operation_id: String,
    pub identity: crate::consumer::SnapshotIdentity,
    pub changed: bool,
    pub validation_started: Option<Stamp>,
    pub commit_started: Option<Stamp>,
    pub accepted: Option<Stamp>,
}

/// Failure produces missing evidence, never zero or wall-clock substitution.
pub fn now() -> Option<Stamp> {
    static BOOT: OnceLock<Option<String>> = OnceLock::new();
    let boot_id = BOOT
        .get_or_init(|| {
            let value = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
            let value = value.trim();
            (!value.is_empty()).then(|| value.to_owned())
        })
        .as_ref()?
        .clone();
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: the stack timespec is valid and exclusively borrowed for this call.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time) } != 0 {
        return None;
    }
    let seconds = u64::try_from(time.tv_sec).ok()?;
    let nanoseconds = u64::try_from(time.tv_nsec).ok()?;
    if nanoseconds >= 1_000_000_000 {
        return None;
    }
    Some(Stamp {
        boot_id,
        clock_id: u32::try_from(libc::CLOCK_MONOTONIC).ok()?,
        nanoseconds: seconds
            .checked_mul(1_000_000_000)?
            .checked_add(nanoseconds)?,
    })
}

pub fn elapsed(from: &Stamp, to: &Stamp) -> Option<u64> {
    if from.boot_id != to.boot_id || from.clock_id != to.clock_id {
        return None;
    }
    to.nanoseconds.checked_sub(from.nanoseconds)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn foreign_boot_clock_and_reverse_order_are_not_intervals() {
        let a = Stamp {
            boot_id: "a".into(),
            clock_id: 1,
            nanoseconds: 20,
        };
        let mut b = a.clone();
        b.nanoseconds = 30;
        assert_eq!(elapsed(&a, &b), Some(10));
        assert_eq!(elapsed(&b, &a), None);
        b.boot_id = "b".into();
        assert_eq!(elapsed(&a, &b), None);
        b.boot_id = "a".into();
        b.clock_id = 2;
        assert_eq!(elapsed(&a, &b), None);
    }
}

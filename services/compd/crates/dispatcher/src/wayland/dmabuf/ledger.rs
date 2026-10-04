//! Observed linux-dmabuf import outcomes, served under `dmabuf.*`.
//!
//! What the advertised format table claims and what an import actually did
//! are different facts; only the second is true. Every
//! `zwp_linux_buffer_params_v1` import compd answers is counted here, and
//! every refusal keeps its format, modifier and reason, so a "works on AMD,
//! blank window on the VM" bug reads from `comp.props.get dmabuf`.
//!
//! Single-threaded: imports resolve on the loop thread (the rim's
//! `pending_dmabuf` drain), so no lock. In memory only: a restart starts from
//! zero.

use std::collections::VecDeque;
use std::time::Duration;

use smithay::backend::allocator::Format;
use smithay::utils::{Clock, Monotonic};

/// How many refusals the ledger keeps, newest last.
pub const FAILURE_RING: usize = 16;

/// Why an import was refused. Wire spellings.
pub mod reason {
    /// The format/modifier pair is not one the compositing renderer can
    /// import (nor in the advertised set).
    pub const INVALID_METADATA: &str = "invalid_metadata";
    /// The GLES test import rejected the buffer: the driver said no.
    pub const GLES_REJECTED: &str = "gles_rejected";
}

/// One refused import.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailureRecord {
    /// DRM fourcc as its four characters (`"AR24"`); a non-printable byte
    /// reads `?`.
    pub format: String,
    /// DRM format modifier, `0x` + 16 hex digits.
    pub modifier: String,
    pub reason: &'static str,
    /// The refusing check's own message.
    pub detail: String,
    /// CLOCK_MONOTONIC µs when the import was refused.
    pub at_us: u64,
}

#[derive(Debug, Default)]
pub struct Ledger {
    pub accepted: u64,
    pub failed: u64,
    failures: VecDeque<FailureRecord>,
}

impl Ledger {
    pub fn record_accepted(&mut self) {
        self.accepted = self.accepted.saturating_add(1);
    }

    pub fn record_failed(&mut self, format: Format, reason: &'static str, detail: impl Into<String>) {
        let now: Duration = Clock::<Monotonic>::new().now().into();
        let record = FailureRecord {
            format: fourcc_text(format.code as u32),
            modifier: format!("{:#018x}", u64::from(format.modifier)),
            reason,
            detail: detail.into(),
            at_us: u64::try_from(now.as_micros()).unwrap_or(u64::MAX),
        };
        self.failed = self.failed.saturating_add(1);
        if self.failures.len() == FAILURE_RING {
            self.failures.pop_front();
        }
        self.failures.push_back(record);
    }

    /// The kept refusals, oldest first.
    pub fn failures(&self) -> impl Iterator<Item = &FailureRecord> {
        self.failures.iter()
    }
}

fn fourcc_text(code: u32) -> String {
    code.to_le_bytes()
        .iter()
        .map(|&byte| {
            if byte.is_ascii_graphic() || byte == b' ' {
                char::from(byte)
            } else {
                '?'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::backend::allocator::{Fourcc, Modifier};

    #[test]
    fn a_refusal_records_format_modifier_and_reason() {
        let mut ledger = Ledger::default();
        ledger.record_accepted();
        ledger.record_failed(
            Format { code: Fourcc::Xrgb8888, modifier: Modifier::from(0x0100_0000_0000_0001_u64) },
            reason::GLES_REJECTED,
            "EGL_BAD_MATCH",
        );
        assert_eq!((ledger.accepted, ledger.failed), (1, 1));
        let record = ledger.failures().next().unwrap();
        assert_eq!(record.format, "XR24");
        assert_eq!(record.modifier, "0x0100000000000001");
        assert_eq!(record.reason, "gles_rejected");
        assert!(record.at_us > 0);
    }

    #[test]
    fn the_ring_keeps_the_newest_refusals_and_the_count_keeps_them_all() {
        let mut ledger = Ledger::default();
        let format = Format { code: Fourcc::Argb8888, modifier: Modifier::Linear };
        for n in 0..(FAILURE_RING + 3) {
            ledger.record_failed(format, reason::INVALID_METADATA, n.to_string());
        }
        assert_eq!(ledger.failed, (FAILURE_RING + 3) as u64);
        let kept: Vec<_> = ledger.failures().collect();
        assert_eq!(kept.len(), FAILURE_RING);
        assert_eq!(kept[0].detail, "3");
        assert_eq!(kept[0].modifier, "0x0000000000000000");
    }

    #[test]
    fn a_non_printable_fourcc_reads_as_question_marks() {
        assert_eq!(fourcc_text(u32::from_le_bytes(*b"AR24")), "AR24");
        assert_eq!(fourcc_text(u32::from_le_bytes([b'A', 0, 0xff, b'4'])), "A??4");
    }
}

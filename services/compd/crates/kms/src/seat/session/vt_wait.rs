//! Wait, as an event, for this process's VT to become the active one before
//! the seat is opened (see `factory`).
//!
//! The VT is the one standard input is (a unit with `TTYPath=/dev/ttyN` and
//! `StandardInput=tty`, as compd's VT unit runs): character major
//! 4, minor 1–63. Anything else (a pipe, a pty, no tty: a nested or
//! dev run) does not wait. The kernel notifies `/sys/class/tty/tty0/active`
//! (POLLPRI) on every console switch — the attribute systemd-logind watches
//! the same way — so the wait is a blocking poll with no timeout: no clock,
//! no restart loop. Nothing here switches VT; only a human does.

use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::io::AsRawFd;

const ACTIVE: &str = "/sys/class/tty/tty0/active";

/// The VT number standard input is, if it is one.
pub(super) fn own_vt() -> Option<u32> {
    let meta = std::fs::metadata("/dev/stdin").ok()?;
    if !meta.file_type().is_char_device() {
        return None;
    }
    vt_of(meta.rdev())
}

/// A character device number's VT, if it is one (major 4, minor 1..=63; the
/// minors from 64 are serial ports, 0 is the current console).
pub fn vt_of(rdev: u64) -> Option<u32> {
    let major = ((rdev >> 8) & 0xfff) | ((rdev >> 32) & !0xfff);
    let minor = (rdev & 0xff) | ((rdev >> 12) & !0xff);
    (major == 4 && (1..=63).contains(&minor)).then_some(minor as u32)
}

/// `tty0/active`'s content (`tty5\n`) as a VT number.
pub fn parse_active(text: &str) -> Option<u32> {
    text.trim().strip_prefix("tty")?.parse().ok()
}

/// Read the active VT once, if the attribute can be read and parsed.
pub(super) fn active_vt() -> Option<u32> {
    parse_active(&std::fs::read_to_string(ACTIVE).ok()?)
}

/// Where the active VT is read from and waited on.
pub trait ActiveVt {
    /// The active VT now.
    fn read(&mut self) -> Option<u32>;
    /// Block until it may have changed.
    fn wait(&mut self) -> std::io::Result<()>;
}

/// Read until `own` is the active VT, waiting on the source between reads.
/// Returns how many waits it took (0: active already). A source that cannot
/// be read or waited on ends the wait: the seat open then fails or succeeds on
/// its own, as before this existed.
pub fn wait_until_active(own: u32, source: &mut impl ActiveVt) -> usize {
    let mut waits = 0;
    loop {
        match source.read() {
            Some(active) if active == own => return waits,
            None => return waits,
            Some(_) => {}
        }
        if let Err(error) = source.wait() {
            warn!("VT activation wait failed ({error}); opening the seat anyway");
            return waits;
        }
        waits += 1;
    }
}

/// `/sys/class/tty/tty0/active`, read from the start each time and polled
/// for POLLPRI (sysfs notification).
struct SysfsActive(std::fs::File);

impl ActiveVt for SysfsActive {
    fn read(&mut self) -> Option<u32> {
        let mut text = String::new();
        self.0.seek(SeekFrom::Start(0)).ok()?;
        self.0.read_to_string(&mut text).ok()?;
        parse_active(&text)
    }

    fn wait(&mut self) -> std::io::Result<()> {
        let mut fd = libc::pollfd { fd: self.0.as_raw_fd(), events: libc::POLLPRI | libc::POLLERR, revents: 0 };
        loop {
            // SAFETY: one valid pollfd for an fd this struct owns; no timeout.
            let ready = unsafe { libc::poll(&mut fd, 1, -1) };
            if ready >= 0 {
                return Ok(());
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
}

/// Block until this process's VT is the active one (no-op off a VT).
pub fn wait_for_own_vt() {
    let Some(own) = own_vt() else { return };
    let Ok(file) = std::fs::File::open(ACTIVE) else {
        warn!("no {ACTIVE}: cannot wait for VT{own} activation; opening the seat anyway");
        return;
    };
    let mut source = SysfsActive(file);
    if source.read() != Some(own) {
        info!("waiting for VT activation (seat inactive): VT{own} is not the active VT; nothing here switches it");
    }
    let waits = wait_until_active(own, &mut source);
    if waits > 0 {
        info!("VT{own} is active (after {waits} console switch notification(s)): opening the seat");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scripted active-VT source: each wait advances to the next reading.
    struct Script {
        readings: Vec<Option<u32>>,
        at: usize,
        waits: usize,
    }

    impl ActiveVt for Script {
        fn read(&mut self) -> Option<u32> {
            self.readings[self.at.min(self.readings.len() - 1)]
        }
        fn wait(&mut self) -> std::io::Result<()> {
            self.waits += 1;
            self.at += 1;
            Ok(())
        }
    }

    /// The seat is opened only once the session's own VT is the active one:
    /// at once when it already is; after the switch notifications otherwise
    /// (other VTs in between do not end the wait).
    #[test]
    fn the_seat_opens_only_when_its_vt_is_active() {
        let mut already = Script { readings: vec![Some(4)], at: 0, waits: 0 };
        assert_eq!(wait_until_active(4, &mut already), 0);
        let mut later = Script { readings: vec![Some(5), Some(1), Some(5), Some(4)], at: 0, waits: 0 };
        assert_eq!(wait_until_active(4, &mut later), 3);
        assert_eq!(later.waits, 3, "one wait per non-matching reading, none after the match");
        let mut unreadable = Script { readings: vec![Some(5), None], at: 0, waits: 0 };
        assert_eq!(wait_until_active(4, &mut unreadable), 1, "an unreadable source ends the wait");
    }

    #[test]
    fn device_numbers_and_the_active_attribute_parse() {
        // tty4: major 4, minor 4 (old-style encoding).
        assert_eq!(vt_of((4 << 8) | 4), Some(4));
        assert_eq!(vt_of(4 << 8), None, "tty0 is the current console, not a VT of its own");
        assert_eq!(vt_of((4 << 8) | 64), None, "ttyS0 shares major 4");
        assert_eq!(vt_of((136 << 8) | 1), None, "a pty");
        assert_eq!(parse_active("tty5\n"), Some(5));
        assert_eq!(parse_active("ttyS0"), None);
    }
}

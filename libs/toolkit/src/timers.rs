// SPDX-License-Identifier: MIT OR Apache-2.0
//! One-shot timers for an application's own debounces and expiries:
//! [`Timers`], a handle to one thread that arms, re-arms and cancels
//! deadlines by key and fires the due keys on a stream.
//!
//! The thread waits on a channel with the nearest deadline as its timeout:
//! it wakes only when a timer is armed or due, never on a period. Re-arming
//! a key replaces its deadline, which is what a debounce is; cancelling
//! removes it. Keys are the caller's own (`TabId`, an enum, a `u32` —
//! anything hashable and sendable).
//!
//! The crate owns no clocks of its own ([`crate::toast`] expects the app to
//! sweep on its own redraws): `Timers` is opt-in machinery for the app that
//! wants wake-ups without an async runtime, started once and cloned by
//! handle. Dropping the last handle stops the thread.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::mpsc;
use std::time::{Duration, Instant};

// The `futures` crate as re-exported by iced's futures crate: the streams
// an iced host already runs are these types, so the returned receiver
// plugs into its subscriptions unchanged.
use iced_futures::futures::channel::mpsc::UnboundedReceiver;

enum Command<K> {
    Arm(K, Instant),
    Cancel(K),
}

/// Handle to the timer thread. Clone it freely; the thread stops when the
/// last handle and the returned stream's sender side are both gone.
#[derive(Clone)]
pub struct Timers<K> {
    tx: mpsc::Sender<Command<K>>,
}

impl<K: Clone + Eq + Hash + Send + 'static> Timers<K> {
    /// Start the thread named `name`; fired keys arrive on the returned
    /// stream. Feed that stream into the application's subscriptions or
    /// event loop however the host allows.
    pub fn start(name: &str) -> (Timers<K>, UnboundedReceiver<K>) {
        let (tx, rx) = mpsc::channel();
        let (fire_tx, fire_rx) = iced_futures::futures::channel::mpsc::unbounded();
        let spawned = std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || run(rx, fire_tx));
        if let Err(error) = spawned {
            // A failed thread spawn is out of memory or a resource limit:
            // fail loudly rather than arm timers that never fire.
            panic!("toolkit timers: cannot start the timer thread: {error}");
        }
        (Timers { tx }, fire_rx)
    }

    /// Fire `key` after `ms`, replacing any pending deadline for it.
    pub fn arm(&self, key: K, ms: u64) {
        let _ = self.tx.send(Command::Arm(key, Instant::now() + Duration::from_millis(ms)));
    }

    /// Remove `key`'s pending deadline, if any.
    pub fn cancel(&self, key: K) {
        let _ = self.tx.send(Command::Cancel(key));
    }
}

fn run<K: Clone + Eq + Hash>(
    rx: mpsc::Receiver<Command<K>>,
    fire: iced_futures::futures::channel::mpsc::UnboundedSender<K>,
) {
    let mut pending: HashMap<K, Instant> = HashMap::new();
    loop {
        let next = pending.values().min().copied();
        let command = match next {
            None => match rx.recv() {
                Ok(c) => Some(c),
                Err(_) => return,
            },
            Some(at) => match rx.recv_timeout(at.saturating_duration_since(Instant::now())) {
                Ok(c) => Some(c),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            },
        };
        match command {
            Some(Command::Arm(key, at)) => {
                pending.insert(key, at);
            }
            Some(Command::Cancel(key)) => {
                pending.remove(&key);
            }
            None => {
                let now = Instant::now();
                let due: Vec<K> =
                    pending.iter().filter(|(_, at)| **at <= now).map(|(k, _)| k.clone()).collect();
                for key in due {
                    pending.remove(&key);
                    if fire.unbounded_send(key).is_err() {
                        return;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_futures::futures::StreamExt;

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    enum Key {
        Save,
        Lint(usize),
        StatusExpiry,
    }

    #[test]
    fn rearming_debounces_and_cancel_removes() {
        let (timers, mut fired) = Timers::<Key>::start("toolkit-timers-test");
        timers.arm(Key::Save, 40);
        timers.arm(Key::Save, 60);
        timers.arm(Key::Lint(7), 10);
        timers.arm(Key::StatusExpiry, 20);
        timers.cancel(Key::StatusExpiry);
        let first = iced_futures::futures::executor::block_on(fired.next());
        let second = iced_futures::futures::executor::block_on(fired.next());
        // The Save re-arm replaced the 40 ms deadline with 60 ms, so the
        // lint fires first; the cancelled status expiry never fires.
        assert_eq!((first, second), (Some(Key::Lint(7)), Some(Key::Save)));
        timers.arm(Key::StatusExpiry, 5);
        assert_eq!(
            iced_futures::futures::executor::block_on(fired.next()),
            Some(Key::StatusExpiry),
            "the cancelled key never fired"
        );
    }

    #[test]
    fn dropping_the_handle_stops_the_thread() {
        let (timers, fired) = Timers::<u8>::start("toolkit-timers-test");
        let thread = std::thread::spawn(move || {
            timers.arm(0, 5);
            // rx stays alive until `fired` drops; the arming command is
            // consumed and the timer fires into a dropped stream, ending
            // the run loop.
            drop(fired);
        });
        thread.join().unwrap();
    }
}

//! The post-activation settle deadline: "first flip proven", per pipe.
//!
//! The safeguard exists because a pipe that has never flipped gets no per-CRTC
//! vblank, so only the redraw ping renders it. The blunt form — an unconditional
//! 30fps redraw for five seconds after every display activation — costs five
//! seconds of forced frames per boot, resume and hotplug, whether or not
//! anything needed them.
//!
//! Instead the safeguard asks the one question it exists for — has EACH activated
//! pipe flipped since its activation? — on a short deadline, and retires once
//! every one has, which on a healthy activation is the very first check. Proof is
//! per pipe ([`Schedule::flips`], counted only by that pipe's own vblank): a busy
//! sibling's flips must not retire the deadline for a new output still black.
//! A pipe that is pruned before it flips counts as settled (there is nothing
//! left to prove). Bounded by [`SETTLE`]; a pipe that never flips by then is
//! logged as a fault, not kicked forever.

use protocols::redraw::schedule::schedule::Schedule;
use world::state::Loop;
use smithay::reexports::calloop::LoopHandle;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// How long a fresh activation may go without its first flip before it is
/// kicked, and the spacing of kicks while it still has not flipped.
pub const PROOF: Duration = Duration::from_millis(100);

/// How long past the most recent activation the deadline stays up at most.
pub const SETTLE: Duration = Duration::from_secs(5);

/// The current window: its end, and each activated pipe with its flip count at
/// activation. Re-arming extends the window and adds (or re-baselines) pipes
/// rather than registering a second timer: one hotplug reconcile can light
/// several pipes, and each should join the window, not multiply it.
struct Window {
    until: Instant,
    pipes: Vec<(String, u64)>,
}

static WINDOW: Mutex<Option<Window>> = Mutex::new(None);

/// Whether a timer source is currently registered against `WINDOW`.
static RUNNING: AtomicBool = AtomicBool::new(false);

/// Start, or extend, the settle window for the pipes `keys` just activated.
/// Safe to call from anywhere an activation completes; idempotent within a
/// window. No keys: nothing to prove, nothing armed.
pub fn arm(loop_handle: &LoopHandle<'static, Loop>, schedule: &mut Schedule, keys: Vec<String>) {
    if keys.is_empty() {
        return;
    }
    if let Ok(mut window) = WINDOW.lock() {
        let window = window.get_or_insert_with(|| Window { until: Instant::now(), pipes: Vec::new() });
        window.until = Instant::now() + SETTLE;
        for key in keys {
            // Registered first: a pipe the schedule does not know (a session
            // resume clears them all) would otherwise read as pruned, i.e. proven.
            schedule.register(&key);
            let baseline = schedule.flips(&key).unwrap_or(0);
            window.pipes.retain(|(k, _)| *k != key);
            window.pipes.push((key, baseline));
        }
    }
    // Already running — updating the window was the whole update.
    if RUNNING.swap(true, Ordering::Relaxed) {
        return;
    }
    // `TimeoutAction::Drop` retires the source from inside its own callback, so
    // there is no token to hand back, store, or go stale.
    let source = Timer::from_duration(PROOF);
    let result = loop_handle.insert_source(source, |_, _, state: &mut Loop| {
        // The guard lives only in this block: `clear()` below locks again.
        let (verdict, waiting) = {
            let mut guard = match WINDOW.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            match guard.as_mut() {
                Some(window) => {
                    // A pipe proves itself once; drop it from the window.
                    window.pipes.retain(|(key, baseline)| {
                        !proven(state.state.redraw.flips(key), *baseline)
                    });
                    let waiting: Vec<String> = window.pipes.iter().map(|(k, _)| k.clone()).collect();
                    (verdict(waiting.is_empty(), Instant::now() >= window.until), waiting)
                }
                // Unreadable or cleared: retire rather than pin the timer on.
                None => (Verdict::Proven, Vec::new()),
            }
        };
        match verdict {
            Verdict::Proven => {
                RUNNING.store(false, Ordering::Relaxed);
                clear();
                info!("settle: first flip proven on every activated pipe");
                TimeoutAction::Drop
            }
            Verdict::GaveUp => {
                RUNNING.store(false, Ordering::Relaxed);
                clear();
                warn!("settle: no page flip within {:?} of activation on {:?}", SETTLE, waiting);
                TimeoutAction::Drop
            }
            Verdict::Kick => {
                state
                    .state
                    .redraw
                    .force_for(protocols::redraw::schedule::schedule::RedrawReason::Settle);
                TimeoutAction::ToDuration(PROOF)
            }
        }
    });
    match result {
        Ok(_) => info!("settle: first-flip deadline armed ({:?}, up to {:?})", PROOF, SETTLE),
        // Not fatal: the reconcile already forced a render of the new pipe, and the
        // stall rescue (`watchdog.idle`) catches one that stays behind its epoch.
        Err(e) => {
            RUNNING.store(false, Ordering::Relaxed);
            warn!("settle: first-flip deadline registration failed: {e}");
        }
    }
}

fn clear() {
    if let Ok(mut window) = WINDOW.lock() {
        *window = None;
    }
}

/// A pipe has proven its first flip since `baseline`; a pipe the schedule no
/// longer knows (pruned) has nothing left to prove.
fn proven(flips: Option<u64>, baseline: u64) -> bool {
    flips.is_none_or(|flips| flips != baseline)
}

#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    /// Every activated pipe flipped: retire.
    Proven,
    /// A pipe still has not flipped and the window is over: retire, loudly.
    GaveUp,
    /// A pipe still has not flipped: force one more redraw and look again.
    Kick,
}

fn verdict(all_proven: bool, expired: bool) -> Verdict {
    if all_proven {
        Verdict::Proven
    } else if expired {
        Verdict::GaveUp
    } else {
        Verdict::Kick
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proof_is_this_pipes_own_flip() {
        assert!(proven(Some(11), 10));
        assert!(!proven(Some(10), 10), "no flip of its own yet");
        assert!(proven(None, 10), "a pruned pipe has nothing left to prove");
    }

    /// Finding E: a sibling's flips cannot settle a new pipe.
    #[test]
    fn a_busy_sibling_does_not_settle_a_new_pipe() {
        let mut s = Schedule::new();
        s.register("primary");
        s.register("secondary");
        let pipes = vec![("secondary".to_string(), s.flips("secondary").unwrap())];
        s.queued("primary");
        s.vblank("primary");
        s.queued("primary");
        s.vblank("primary");
        let waiting: Vec<&String> = pipes
            .iter()
            .filter(|(k, b)| !proven(s.flips(k), *b))
            .map(|(k, _)| k)
            .collect();
        assert_eq!(waiting, ["secondary"]);
        assert_eq!(verdict(waiting.is_empty(), false), Verdict::Kick);
        s.vblank("secondary");
        assert!(pipes.iter().all(|(k, b)| proven(s.flips(k), *b)));
    }

    #[test]
    fn no_flip_kicks_until_the_window_ends() {
        assert_eq!(verdict(false, false), Verdict::Kick);
        assert_eq!(verdict(false, true), Verdict::GaveUp);
        assert_eq!(verdict(true, true), Verdict::Proven);
    }
}

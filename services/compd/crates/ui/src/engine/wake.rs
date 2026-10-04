//! Event-driven iced wakeups. Widget wakes carry a per-runtime pending bit;
//! renderer notifications share the same registered compositor-loop source.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, RwLock};

use smithay::reexports::calloop::LoopHandle;
use smithay::reexports::calloop::ping::make_ping;

type Wake = Arc<dyn Fn() + Send + Sync>;

fn loop_waker() -> &'static RwLock<Option<Wake>> {
    static WAKER: OnceLock<RwLock<Option<Wake>>> = OnceLock::new();
    WAKER.get_or_init(|| RwLock::new(None))
}

/// Register before entering the loop. `request_frame` must schedule an iced
/// redraw; merely waking the poller would leave a clean output parked.
pub fn register<D: 'static>(
    handle: &LoopHandle<'static, D>,
    request_frame: fn(&mut D),
) -> Result<(), String> {
    let (ping, source) = make_ping().map_err(|error| format!("iced wake source: {error}"))?;
    handle
        .insert_source(source, move |_, _, data| request_frame(data))
        .map_err(|error| format!("iced wake source: {error}"))?;
    *loop_waker()
        .write()
        .unwrap_or_else(|error| error.into_inner()) = Some(Arc::new(move || ping.ping()));
    Ok(())
}

/// Called after recording dirty state, including from renderer worker threads.
pub(super) fn notify() {
    let wake = loop_waker()
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    if let Some(wake) = wake {
        wake();
    }
}

pub(super) struct RuntimeWake {
    pending: Arc<AtomicBool>,
    waker: iced_core::shell::Waker,
}

impl RuntimeWake {
    pub fn new() -> Self {
        Self::with_waker(notify)
    }

    fn with_waker(wake: impl Fn() + Send + Sync + 'static) -> Self {
        let pending = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&pending);
        let waker = iced_core::shell::Waker::new(move || {
            flag.store(true, Ordering::Release);
            wake();
        });
        Self { pending, waker }
    }

    pub fn pending(&self) -> bool {
        self.pending.load(Ordering::Acquire)
    }
    pub fn take(&self) -> bool {
        self.pending.swap(false, Ordering::AcqRel)
    }
    pub fn waker(&self) -> &iced_core::shell::Waker {
        &self.waker
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::reexports::calloop::EventLoop;
    use std::time::Duration;

    #[test]
    fn widget_wake_without_input_wakes_loop_and_retains_pending_event() {
        let mut event_loop: EventLoop<'static, u32> = EventLoop::try_new().unwrap();
        let (ping, source) = make_ping().unwrap();
        let wake = RuntimeWake::with_waker(move || ping.ping());
        event_loop
            .handle()
            .insert_source(source, |_, _, wakes: &mut u32| {
                *wakes += 1;
            })
            .unwrap();
        let mut wakes = 0;
        let waker = wake.waker().clone();
        let worker = std::thread::spawn(move || waker.wake());
        // The timeouts bound failure; there are no timers or input sources.
        event_loop
            .dispatch(Duration::from_secs(1), &mut wakes)
            .unwrap();
        assert!(wake.pending());
        worker.join().unwrap();
        assert_eq!(wakes, 1);
        assert!(wake.take());
        assert!(!wake.take());
    }
}

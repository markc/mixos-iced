// SPDX-License-Identifier: MIT OR Apache-2.0
//! Bounded, read-only observation of frames submitted by the native runtime.
//!
//! One handle belongs to one window incarnation. Receipts are physical history,
//! separate from settings authority and Bus registration. Waiting neither sends
//! a widget message nor requests a redraw; only an exact window and installed
//! view stamp can satisfy it. The two retained slots are not a history archive.

pub use crate::iced::window::presentation::{
    FrameBinding, FrameObservation, FrameOutcome, FrameStamp,
};
use crate::iced::window::{Id, presentation::FrameObserver};
use std::{
    sync::{Arc, Mutex, Weak},
    time::Instant,
};
use tokio::sync::Notify;

struct Shared {
    state: Mutex<State>,
    changed: Notify,
}

#[derive(Default)]
struct State {
    window: Option<Id>,
    closed: bool,
    generation: Option<u64>,
    revision: u64,
    waiter: bool,
    last_observation: Option<FrameObservation>,
    last_presented: Option<FrameObservation>,
}

/// Copied evidence; current registration never relabels historical receipts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub window: Option<Id>,
    pub closed: bool,
    pub live_generation: Option<u64>,
    pub lifecycle_revision: u64,
    pub last_observation: Option<FrameObservation>,
    pub last_presented: Option<FrameObservation>,
}

/// An owner-specific native lifecycle fence. It cannot be authored from JSON.
#[derive(Clone)]
pub struct Fence {
    owner: Weak<Shared>,
    revision: u64,
}

impl std::fmt::Debug for Fence {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Fence")
            .field("revision", &self.revision)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub struct Expected {
    pub window: Id,
    pub stamp: FrameStamp,
    pub fence: Fence,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitError {
    Busy,
    TimedOut,
    Closed,
    LifecycleChanged,
    WrongWindow,
    Unsupported,
    Exhausted,
}

/// Clones retain one stable runtime observer and one bounded observation owner.
#[derive(Clone)]
pub struct Handle {
    shared: Arc<Shared>,
    observer: FrameObserver,
}

impl Default for Handle {
    fn default() -> Self {
        Self::new()
    }
}

impl Handle {
    pub fn new() -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            changed: Notify::new(),
        });
        let observed = Arc::clone(&shared);
        let observer = FrameObserver::new(move |receipt| observed.observe(receipt));
        Self { shared, observer }
    }

    /// Pure getter, sampled alongside the immutable view before it is drawn.
    pub fn binding(&self, stamp: FrameStamp) -> FrameBinding {
        FrameBinding {
            stamp,
            observer: self.observer.clone(),
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        let state = self.shared.state.lock().unwrap();
        Snapshot {
            window: state.window,
            closed: state.closed,
            live_generation: state.generation,
            lifecycle_revision: state.revision,
            last_observation: state.last_observation,
            last_presented: state.last_presented,
        }
    }

    pub fn fence(&self) -> Fence {
        Fence {
            owner: Arc::downgrade(&self.shared),
            revision: self.shared.state.lock().unwrap().revision,
        }
    }

    /// Called by the existing native owner before admitting evidence requests.
    /// Registration loss invalidates waits while retaining pixel history.
    pub fn set_live_generation(&self, generation: Option<u64>) {
        let mut state = self.shared.state.lock().unwrap();
        if state.closed || state.generation == generation {
            return;
        }
        state.generation = generation;
        match state.revision.checked_add(1) {
            Some(next) => state.revision = next,
            None => state.closed = true,
        }
        drop(state);
        self.shared.changed.notify_waiters();
    }

    /// Wake and retire waits before the application drains native reply tasks.
    pub fn close(&self) {
        self.shared.state.lock().unwrap().closed = true;
        self.shared.changed.notify_waiters();
    }

    /// One cancellation-safe waiter, using the caller's absolute deadline.
    /// Capacity, discard and failed submission await a later natural frame;
    /// unsupported presentation or exhausted IDs fail explicitly.
    pub async fn wait(
        &self,
        expected: Expected,
        deadline: Instant,
    ) -> Result<FrameObservation, WaitError> {
        let _slot = {
            let mut state = self.shared.state.lock().unwrap();
            if state.waiter {
                return Err(WaitError::Busy);
            }
            if deadline <= Instant::now() {
                return Err(WaitError::TimedOut);
            }
            self.check_scope(&state, &expected)?;
            state.waiter = true;
            WaitSlot(Arc::clone(&self.shared))
        };
        loop {
            let notified = self.shared.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let state = self.shared.state.lock().unwrap();
                self.check_scope(&state, &expected)?;
                if deadline <= Instant::now() {
                    return Err(WaitError::TimedOut);
                }
                let matches = |receipt: &FrameObservation| {
                    receipt.window == expected.window && receipt.stamp == expected.stamp
                };
                if let Some(receipt) = state
                    .last_presented
                    .as_ref()
                    .filter(|receipt| matches(receipt))
                {
                    return Ok(*receipt);
                }
                if let Some(receipt) = state
                    .last_observation
                    .as_ref()
                    .filter(|receipt| matches(receipt))
                {
                    match receipt.outcome {
                        FrameOutcome::Unsupported => return Err(WaitError::Unsupported),
                        FrameOutcome::Exhausted => return Err(WaitError::Exhausted),
                        _ => {}
                    }
                }
            }
            tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), notified)
                .await
                .map_err(|_| WaitError::TimedOut)?;
        }
    }

    fn check_scope(&self, state: &State, expected: &Expected) -> Result<(), WaitError> {
        if state.closed {
            return Err(WaitError::Closed);
        }
        if !Weak::ptr_eq(&expected.fence.owner, &Arc::downgrade(&self.shared))
            || expected.fence.revision != state.revision
        {
            return Err(WaitError::LifecycleChanged);
        }
        if state.window.is_some_and(|window| window != expected.window) {
            return Err(WaitError::WrongWindow);
        }
        Ok(())
    }
}

struct WaitSlot(Arc<Shared>);
impl Drop for WaitSlot {
    fn drop(&mut self) {
        self.0.state.lock().unwrap().waiter = false;
    }
}

impl Shared {
    fn observe(&self, receipt: FrameObservation) {
        let mut state = self.state.lock().unwrap();
        if state.closed || state.window.is_some_and(|window| window != receipt.window) {
            return;
        }
        state.window = Some(receipt.window);
        if receipt.outcome == FrameOutcome::Closed {
            state.closed = true;
        }
        let current = state.last_observation.as_ref().is_none_or(|previous| {
            let stamp = |value: FrameStamp| (value.activation_epoch, value.local_revision);
            stamp(receipt.stamp) > stamp(previous.stamp)
                || (receipt.stamp == previous.stamp
                    && match (receipt.request_id, previous.request_id) {
                        (Some(next), Some(old)) => next >= old,
                        _ => true,
                    })
        });
        let mut changed = state.closed;
        if current && state.last_observation != Some(receipt) {
            state.last_observation = Some(receipt);
            changed = true;
        }
        if matches!(receipt.outcome, FrameOutcome::Presented { .. })
            && receipt.request_id.is_some()
            && state
                .last_presented
                .as_ref()
                .is_none_or(|previous| receipt.request_id > previous.request_id)
        {
            state.last_presented = Some(receipt);
            changed = true;
        }
        drop(state);
        if changed {
            self.changed.notify_waiters();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn stamp(epoch: u64) -> FrameStamp {
        FrameStamp {
            activation_epoch: epoch,
            local_revision: 0,
        }
    }
    fn expected(handle: &Handle, window: Id, epoch: u64) -> Expected {
        Expected {
            window,
            stamp: stamp(epoch),
            fence: handle.fence(),
        }
    }
    fn presented() -> FrameOutcome {
        FrameOutcome::Presented {
            clock_id: Some(1),
            seconds: 2,
            nanoseconds: 3,
            refresh_ns: 4,
            output_sequence: 5,
            flags: 0,
        }
    }
    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(5)
    }

    #[test]
    fn binding_and_snapshot_are_pure_and_preserve_the_observer_owner() {
        let handle = Handle::new();
        let before = handle.snapshot();
        assert!(
            handle
                .binding(stamp(1))
                .same_presentation(&handle.clone().binding(stamp(1)))
        );
        assert!(
            !handle
                .binding(stamp(1))
                .same_presentation(&Handle::new().binding(stamp(1)))
        );
        assert_eq!(handle.snapshot(), before);
    }

    #[tokio::test]
    async fn late_feedback_never_certifies_or_regresses_a_new_stamp_or_window() {
        let handle = Handle::new();
        let window = Id::unique();
        let old = handle.binding(stamp(1));
        let current = handle.binding(stamp(2));
        current.observe(window, Some(2), presented());
        old.observe(window, Some(1), presented());
        assert_eq!(handle.snapshot().last_presented.unwrap().stamp, stamp(2));
        assert_eq!(
            handle
                .wait(expected(&handle, window, 2), deadline())
                .await
                .unwrap()
                .request_id,
            Some(2)
        );
        current.observe(Id::unique(), Some(3), presented());
        current.observe(window, Some(4), FrameOutcome::Discarded);
        assert_eq!(
            handle.snapshot().last_presented.unwrap().request_id,
            Some(2)
        );
        assert_eq!(
            handle.snapshot().last_observation.unwrap().outcome,
            FrameOutcome::Discarded
        );
        assert_eq!(
            handle
                .wait(expected(&handle, Id::unique(), 2), deadline())
                .await,
            Err(WaitError::WrongWindow)
        );
        assert_eq!(
            handle
                .wait(
                    expected(&handle, window, 1),
                    Instant::now() + Duration::from_millis(10)
                )
                .await,
            Err(WaitError::TimedOut)
        );
    }

    #[tokio::test]
    async fn cancelled_wait_releases_the_only_slot_and_notify_completes_its_replacement() {
        let handle = Handle::new();
        let window = Id::unique();
        let mut first = Box::pin(handle.wait(expected(&handle, window, 1), deadline()));
        assert!(crate::iced::futures::poll!(first.as_mut()).is_pending());
        assert_eq!(
            handle.wait(expected(&handle, window, 1), deadline()).await,
            Err(WaitError::Busy)
        );
        drop(first);
        let mut next = Box::pin(handle.wait(expected(&handle, window, 1), deadline()));
        assert!(crate::iced::futures::poll!(next.as_mut()).is_pending());
        handle
            .binding(stamp(1))
            .observe(window, Some(1), presented());
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), next)
                .await
                .unwrap()
                .unwrap()
                .stamp,
            stamp(1)
        );
    }

    #[tokio::test]
    async fn lifecycle_loss_wakes_waits_without_relabelling_historical_pixels() {
        let handle = Handle::new();
        let window = Id::unique();
        handle.set_live_generation(Some(1));
        handle
            .binding(stamp(1))
            .observe(window, Some(1), presented());
        let mut wait = Box::pin(handle.wait(expected(&handle, window, 2), deadline()));
        assert!(crate::iced::futures::poll!(wait.as_mut()).is_pending());
        handle.set_live_generation(None);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), wait)
                .await
                .unwrap(),
            Err(WaitError::LifecycleChanged)
        );
        assert_eq!(handle.snapshot().live_generation, None);
        assert_eq!(handle.snapshot().last_presented.unwrap().stamp, stamp(1));
        let foreign = Expected {
            window,
            stamp: stamp(1),
            fence: Handle::new().fence(),
        };
        assert_eq!(
            handle.wait(foreign, deadline()).await,
            Err(WaitError::LifecycleChanged)
        );
    }

    #[tokio::test]
    async fn retirement_and_unavailability_are_explicit_and_late_callbacks_are_ignored() {
        let handle = Handle::new();
        let window = Id::unique();
        let binding = handle.binding(stamp(1));
        binding.observe(window, None, FrameOutcome::Unsupported);
        assert_eq!(
            handle.wait(expected(&handle, window, 1), deadline()).await,
            Err(WaitError::Unsupported)
        );
        binding.observe(window, Some(1), presented());
        let mut wait = Box::pin(handle.wait(expected(&handle, window, 2), deadline()));
        assert!(crate::iced::futures::poll!(wait.as_mut()).is_pending());
        binding.observe(window, None, FrameOutcome::Closed);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), wait)
                .await
                .unwrap(),
            Err(WaitError::Closed)
        );
        let closed = handle.snapshot();
        handle
            .binding(stamp(2))
            .observe(window, Some(2), presented());
        assert_eq!(handle.snapshot(), closed);
        assert_eq!(
            handle.wait(expected(&handle, window, 1), deadline()).await,
            Err(WaitError::Closed)
        );
    }
}

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
    owner: Option<u64>,
    state: Mutex<State>,
    changed: Notify,
    observations: tokio::sync::watch::Sender<Snapshot>,
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
    last_observation_revision: Option<u64>,
    last_presented_revision: Option<u64>,
    scoped_observer: Option<FrameObserver>,
}

/// Copied evidence; current registration never relabels historical receipts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// Minted once by the actual Handle, even before its first callback.
    pub owner: Option<u64>,
    pub window: Option<Id>,
    pub closed: bool,
    pub live_generation: Option<u64>,
    pub lifecycle_revision: u64,
    pub last_observation: Option<FrameObservation>,
    pub last_presented: Option<FrameObservation>,
    pub last_observation_revision: Option<u64>,
    pub last_presented_revision: Option<u64>,
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
}

impl Default for Handle {
    fn default() -> Self {
        Self::new()
    }
}

impl Handle {
    pub fn new() -> Self {
        static OWNERS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let owner = OWNERS.fetch_update(std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed, |value| value.checked_add(1)).ok();
        let (observations, _) = tokio::sync::watch::channel(Snapshot {
            owner,
            window: None, closed: false, live_generation: None,
            lifecycle_revision: 0, last_observation: None, last_presented: None,
            last_observation_revision: None, last_presented_revision: None,
        });
        let shared = Arc::new(Shared {
            owner,
            state: Mutex::new(State::default()),
            changed: Notify::new(),
            observations,
        });
        Self { shared }
    }

    /// Pure getter, sampled alongside the immutable view before it is drawn.
    pub fn binding(&self, stamp: FrameStamp) -> FrameBinding {
        let observed = Arc::clone(&self.shared);
        FrameBinding {
            stamp,
            observer: FrameObserver::with_capture(move || observed.capture()),
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        let state = self.shared.state.lock().unwrap();
        Snapshot {
            owner: self.shared.owner,
            window: state.window,
            closed: state.closed,
            live_generation: state.generation,
            lifecycle_revision: state.revision,
            last_observation: state.last_observation,
            last_presented: state.last_presented,
            last_observation_revision: state.last_observation_revision,
            last_presented_revision: state.last_presented_revision,
        }
    }

    /// Existing native owners multiplex this bounded latest-value watch in
    /// their own select loop. Subscription creates no task, timer or redraw.
    pub fn subscribe_observations(&self) -> tokio::sync::watch::Receiver<Snapshot> {
        self.shared.observations.subscribe()
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
        state.scoped_observer = None;
        match state.revision.checked_add(1) {
            Some(next) => state.revision = next,
            None => state.closed = true,
        }
        self.shared.publish_locked(&state);
        drop(state);
        self.shared.changed.notify_waiters();
    }

    /// Wake and retire waits before the application drains native reply tasks.
    pub fn close(&self) {
        let mut state = self.shared.state.lock().unwrap();
        if state.closed {return;}
        state.closed = true;
        self.shared.publish_locked(&state);
        drop(state);
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
                .filter(|receipt| matches(receipt) && state.last_presented_revision == Some(state.revision))
                {
                    return Ok(*receipt);
                }
                if let Some(receipt) = state
                    .last_observation
                    .as_ref()
                    .filter(|receipt| matches(receipt) && state.last_observation_revision == Some(state.revision))
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
    fn publish_locked(&self, state: &State) {
        // Publishing while retaining the state lock preserves callback order.
        // Receivers only clone copied metadata; they never acquire this lock.
        self.observations.send_if_modified(|value| {
            let next = Snapshot {
                owner: self.owner,
                window: state.window, closed: state.closed, live_generation: state.generation,
                lifecycle_revision: state.revision,
                last_observation: state.last_observation, last_presented: state.last_presented,
                last_observation_revision: state.last_observation_revision,
                last_presented_revision: state.last_presented_revision,
            };
            if *value == next {false} else {*value = next; true}
        });
    }
    fn capture(self: &Arc<Self>) -> FrameObserver {
        let mut state = self.state.lock().unwrap();
        if let Some(observer) = &state.scoped_observer {
            return observer.clone();
        }
        let revision = state.revision;
        let observed = Arc::downgrade(self);
        let observer = FrameObserver::new(move |receipt| {
            if let Some(observed) = observed.upgrade() {
                observed.observe(receipt, revision);
            }
        });
        state.scoped_observer = Some(observer.clone());
        observer
    }
    fn observe(&self, receipt: FrameObservation, revision: u64) {
        let mut state = self.state.lock().unwrap();
        if state.closed
            || state.revision != revision
            || state.window.is_some_and(|window| window != receipt.window)
        {
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
            state.last_observation_revision = Some(revision);
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
            state.last_presented_revision = Some(revision);
            changed = true;
        }
        if changed {self.publish_locked(&state);}
        drop(state);
        if changed {
            self.changed.notify_waiters();
        }
    }
}

/// Production readback of copied native evidence. This never samples transport,
/// binds current settings to historical pixels or requests a frame.
#[cfg(any(feature = "describe", feature = "acceptance", feature = "settings-native"))]
pub fn observation_json(receipt: FrameObservation) -> serde_json::Value {
    use serde_json::json;
    let outcome = match receipt.outcome {
        FrameOutcome::Presented {
            clock_id,
            seconds,
            nanoseconds,
            refresh_ns,
            output_sequence,
            flags,
        } => {
            json!({"kind":"presented","clock_id":clock_id,"seconds":seconds,"nanoseconds":nanoseconds,"refresh_ns":refresh_ns,"output_sequence":output_sequence,"flags":flags})
        }
        other => json!({"kind":match other {
            FrameOutcome::Discarded=>"discarded",FrameOutcome::Unsupported=>"unsupported",FrameOutcome::Capacity=>"capacity",FrameOutcome::Exhausted=>"exhausted",FrameOutcome::Closed=>"closed",FrameOutcome::SubmissionFailed=>"submission_failed",FrameOutcome::Presented{..}=>unreachable!(),
        }}),
    };
    json!({"window":receipt.window.raw(),"stamp":{"activation_epoch":receipt.stamp.activation_epoch,"local_revision":receipt.stamp.local_revision},"request_id":receipt.request_id,"outcome":outcome})
}

#[cfg(any(feature = "describe", feature = "acceptance", feature = "settings-native"))]
pub fn snapshot_json(snapshot: &Snapshot) -> serde_json::Value {
    serde_json::json!({"owner":snapshot.owner,"window":snapshot.window.map(Id::raw),"closed":snapshot.closed,"live_generation":snapshot.live_generation,"lifecycle_revision":snapshot.lifecycle_revision,"last_observation":snapshot.last_observation.map(observation_json),"last_presented":snapshot.last_presented.map(observation_json),"last_observation_revision":snapshot.last_observation_revision,"last_presented_revision":snapshot.last_presented_revision})
}

/// Decode copied native metadata without certifying its source. The owning
/// broker receiver must authenticate registration and real surface lifetime
/// before passing this value to the participant registry.
#[cfg(feature = "settings-native")]
pub fn decode_snapshot_json(value:&serde_json::Value)->Option<Snapshot> {
    fn optional(value:&serde_json::Value)->Option<Option<u64>> {if value.is_null() {Some(None)} else {value.as_u64().map(Some)}}
    fn receipt(value:&serde_json::Value)->Option<Option<FrameObservation>> {
        if value.is_null() {return Some(None);}
        let raw = &value["outcome"];
        let outcome = match raw["kind"].as_str()? {
            "presented" => {
                let nanoseconds = u32::try_from(raw["nanoseconds"].as_u64()?).ok()?;
                if nanoseconds >= 1_000_000_000 {return None;}
                FrameOutcome::Presented {clock_id:optional(&raw["clock_id"])?.map(u32::try_from).transpose().ok()?,
                    seconds:raw["seconds"].as_u64()?, nanoseconds,
                    refresh_ns:u32::try_from(raw["refresh_ns"].as_u64()?).ok()?,
                    output_sequence:raw["output_sequence"].as_u64()?, flags:u32::try_from(raw["flags"].as_u64()?).ok()?}
            }
            "discarded"=>FrameOutcome::Discarded,"unsupported"=>FrameOutcome::Unsupported,
            "capacity"=>FrameOutcome::Capacity,"exhausted"=>FrameOutcome::Exhausted,
            "closed"=>FrameOutcome::Closed,"submission_failed"=>FrameOutcome::SubmissionFailed,_=>return None,
        };
        Some(Some(FrameObservation {window:Id::from_raw(value["window"].as_u64()?),
            stamp:FrameStamp {activation_epoch:value["stamp"]["activation_epoch"].as_u64()?,local_revision:value["stamp"]["local_revision"].as_u64()?},
            request_id:optional(&value["request_id"])?,outcome}))
    }
    Some(Snapshot {owner:optional(&value["owner"])?,window:optional(&value["window"])?.map(Id::from_raw),closed:value["closed"].as_bool()?,
        live_generation:optional(&value["live_generation"])?,lifecycle_revision:value["lifecycle_revision"].as_u64()?,
        last_observation:receipt(&value["last_observation"])?,last_presented:receipt(&value["last_presented"])?,
        last_observation_revision:optional(&value["last_observation_revision"])?,
        last_presented_revision:optional(&value["last_presented_revision"])?})
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
    fn retired_binding_callback_cannot_populate_a_replacement_generation() {
        let handle = Handle::new();
        let window = Id::unique();
        handle.set_live_generation(Some(1));
        let retained_view = handle.binding(stamp(1));
        let old = retained_view.captured();
        assert!(old.same_presentation(&retained_view));
        handle.set_live_generation(None);
        handle.set_live_generation(Some(2));
        old.observe(window, Some(10), presented());
        assert!(handle.snapshot().last_presented.is_none());
        assert!(!old.same_presentation(&retained_view));
        retained_view
            .captured()
            .observe(window, Some(11), presented());
        assert_eq!(
            handle.snapshot().last_presented.unwrap().request_id,
            Some(11)
        );
        assert_eq!(
            handle.snapshot().last_presented.unwrap().stamp,
            stamp(1),
            "fresh ownership never relabels unchanged rendered pixels"
        );
    }

    #[test]
    fn observation_watch_is_idle_and_preserves_historical_receipt_generation() {
        let handle = Handle::new();
        assert!(handle.snapshot().owner.is_some());
        assert_eq!(handle.snapshot().owner,handle.clone().snapshot().owner);
        assert_ne!(handle.snapshot().owner,Handle::new().snapshot().owner);
        let mut changes = handle.subscribe_observations();
        assert!(!changes.has_changed().unwrap());
        let window = Id::unique();
        handle.set_live_generation(Some(1));
        changes.borrow_and_update();
        let binding = handle.binding(stamp(1));
        binding.captured().observe(window,Some(1),presented());
        assert!(changes.has_changed().unwrap());
        let old = changes.borrow_and_update().clone();
        assert_eq!(old.last_presented_revision,Some(old.lifecycle_revision));
        handle.set_live_generation(Some(2));
        let current = changes.borrow_and_update().clone();
        assert_eq!(current.last_presented,old.last_presented);
        assert_eq!(current.last_presented_revision,old.last_presented_revision);
        assert_ne!(current.last_presented_revision,Some(current.lifecycle_revision));
        let _ = handle.snapshot();
        let _ = handle.binding(stamp(1));
        assert!(!changes.has_changed().unwrap(),"lookups never publish or manufacture a frame");
        binding.captured().observe(window,Some(2),presented());
        let fresh = changes.borrow_and_update().clone();
        assert_eq!(fresh.last_presented_revision,Some(fresh.lifecycle_revision));
        assert_eq!(fresh.last_presented.unwrap().request_id,Some(2));
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

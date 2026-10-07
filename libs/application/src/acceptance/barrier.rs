// SPDX-License-Identifier: MIT OR Apache-2.0
//! A bounded native operation barrier.
//!
//! [`barrier`] installs one process-wide hold point: the product's real
//! operation worker calls [`Hook::reach`] at an approved point; a matching
//! arm transitions the hold to `Reached` and hands the operation its sole
//! [`Permit`]. The acceptance worker releases the hold (or it is cancelled,
//! expires or closes), and only then does the operation proceed. A hold that
//! ends without a release is a failed acceptance condition and unblocks the
//! operation as cancellation; it never silently performs a destructive
//! operation after the fixture lost control.
//!
//! No UI code sets `Reached` or fakes busy state, and the barrier owns no
//! client, runtime, thread, operation payload or product busy state. The
//! synchronous wait uses a condition variable with an absolute deadline on
//! the already existing operation thread; the asynchronous waits use
//! wakeups on the existing worker runtime.

use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tokio::sync::Notify;

/// The absolute lifetime of one armed hold, from arm to close.
pub const LIFETIME: Duration = Duration::from_secs(10);
/// The maximum length of run and token strings, in bytes.
pub const MAX_STRING: usize = 64;
/// The maximum size of an operation observation, in bytes.
pub const MAX_OBSERVATION: usize = 4_096;

/// The state of the process-wide hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// No hold is installed.
    Idle,
    /// The hold is armed and waiting for the operation to reach its point.
    Armed,
    /// The operation reached the point and holds the permit.
    Reached,
    /// The fixture released the operation.
    Released,
    /// The hold was cancelled (its permit was dropped without a release).
    Cancelled,
    /// The hold exceeded its absolute lifetime.
    Expired,
    /// The hold was closed by shutdown, loss of the controlling
    /// registration generation, or controller drop.
    Closed,
}

/// Why a hold was closed or cancelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClosedReason {
    /// The controller was explicitly closed or dropped.
    Closed,
    /// The process is shutting down.
    Shutdown,
    /// The controlling registration generation was lost.
    LostGeneration,
    /// The armed hold exceeded its absolute lifetime.
    Expired,
    /// The permit was dropped without a release.
    PermitDropped,
}

/// A hold ended without a release: the operation must treat its wait as
/// cancellation, not permission to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancelled {
    /// Why the hold ended.
    pub reason: ClosedReason,
}

/// The per-process acceptance run identity, from the fixture launch
/// configuration.
#[derive(Debug, Clone)]
pub struct Run {
    id: String,
    instance: u64,
}

impl Run {
    /// A run with the given fixture id and per-process instance nonce.
    pub fn new(id: impl Into<String>, instance: u64) -> Result<Self, Error> {
        let id = id.into();
        if id.is_empty() || id.len() > MAX_STRING {
            return Err(Error::BadRun { bytes: id.len() });
        }
        Ok(Self { id, instance })
    }

    /// The run id.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The instance nonce.
    pub fn instance(&self) -> u64 {
        self.instance
    }
}

/// A fixture-chosen hold token.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Token(String);

impl Token {
    /// A token of at most [`MAX_STRING`] bytes.
    pub fn try_new(value: impl Into<String>) -> Result<Self, Error> {
        let value = value.into();
        if value.is_empty() || value.len() > MAX_STRING {
            return Err(Error::BadToken { bytes: value.len() });
        }
        Ok(Self(value))
    }

    /// The token string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The run, instance and connection-generation fence carried by every
/// mutating request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fence {
    /// The run id, which must match the barrier's.
    pub run: String,
    /// The instance nonce, which must match the barrier's.
    pub instance: u64,
    /// The client connection generation at the time of the request.
    pub generation: u64,
}

/// An arm request: install the hold for `point` under `token`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arm {
    /// The run/instance/generation fence.
    pub fence: Fence,
    /// The registered point to hold.
    pub point: String,
    /// The fixture-chosen token for this hold.
    pub token: Token,
}

/// A bounded observation captured from the actual operation when it reaches
/// the point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation(String);

impl Observation {
    /// An observation of at most [`MAX_OBSERVATION`] bytes.
    pub fn try_new(value: impl Into<String>) -> Result<Self, Error> {
        let value = value.into();
        if value.len() > MAX_OBSERVATION {
            return Err(Error::ObservationTooLarge { bytes: value.len() });
        }
        Ok(Self(value))
    }

    /// The observation.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The retained receipt of the last hold transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    /// The hold token.
    pub token: Token,
    /// The hold state at the time of the receipt.
    pub state: State,
    /// The held point.
    pub point: Option<String>,
    /// The observation captured at the reach, if any.
    pub observation: Option<Observation>,
    /// The instance nonce of the hold.
    pub instance: u64,
    /// The generation that armed the hold.
    pub generation: u64,
}

/// A barrier error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The controller is closed and refuses new holds.
    Closed { reason: ClosedReason },
    /// Another hold is active.
    Busy,
    /// One `wait_reached` waiter is already registered.
    WaitBusy,
    /// The point is not registered.
    UnknownPoint,
    /// The run or instance does not match the barrier's.
    WrongFence,
    /// The generation is not the one that armed the active hold.
    StaleGeneration,
    /// The run id is empty or exceeds the byte limit.
    BadRun { bytes: usize },
    /// The token is empty or exceeds the byte limit.
    BadToken { bytes: usize },
    /// The observation exceeds the byte limit.
    ObservationTooLarge { bytes: usize },
    /// No hold matches the token.
    UnknownToken,
    /// The point was already reached by another operation.
    AlreadyReached,
    /// The hold has not reached its point yet.
    NotReached,
    /// The hold ended without a release.
    Cancelled { reason: ClosedReason },
}

struct Hold {
    state: State,
    token: Token,
    point: String,
    observation: Option<Observation>,
    instance: u64,
    generation: u64,
    armed_at: Instant,
    hold_generation: u64,
    reason: Option<ClosedReason>,
}

struct Core {
    run: Run,
    points: &'static [&'static str],
    hold: Option<Hold>,
    retained: Option<Receipt>,
    wait_slot_taken: bool,
    closed: Option<ClosedReason>,
    next_hold_generation: u64,
}

struct Shared {
    core: Mutex<Core>,
    condvar: Condvar,
    notify: Notify,
    lifetime: Duration,
}

impl Shared {
    fn receipt(&self, guard: &MutexGuard<'_, Core>) -> Option<Receipt> {
        let hold = guard.hold.as_ref()?;
        Some(Receipt {
            token: hold.token.clone(),
            state: hold.state,
            point: Some(hold.point.clone()),
            observation: hold.observation.clone(),
            instance: hold.instance,
            generation: hold.generation,
        })
    }

    fn wake(&self) {
        self.condvar.notify_all();
        self.notify.notify_waiters();
    }

    /// Cancels the matching hold; a no-op when it does not exist or has
    /// already left the armed/reached states.
    fn cancel_hold(&self, hold_generation: u64, reason: ClosedReason) {
        let mut guard = self.core.lock().unwrap();
        let Some(hold) = guard.hold.as_mut() else {
            return;
        };
        if hold.hold_generation != hold_generation
            || !matches!(hold.state, State::Armed | State::Reached)
        {
            return;
        }

        hold.state = match reason {
            ClosedReason::Expired => State::Expired,
            ClosedReason::PermitDropped => State::Cancelled,
            ClosedReason::Closed | ClosedReason::Shutdown | ClosedReason::LostGeneration => {
                State::Closed
            }
        };
        hold.reason = Some(reason);
        guard.retained = self.receipt(&guard);
        drop(guard);
        self.wake();
    }

    fn deadline(&self, guard: &MutexGuard<'_, Core>) -> Option<Instant> {
        guard
            .hold
            .as_ref()
            .map(|hold| hold.armed_at + self.lifetime)
    }
}

/// The fixture side of the barrier: arms, waits for, releases and closes the
/// single process-wide hold.
#[derive(Clone)]
pub struct Controller {
    shared: Arc<Shared>,
    liveness: Arc<Liveness>,
}

struct Liveness {
    shared: Arc<Shared>,
}

impl Drop for Liveness {
    fn drop(&mut self) {
        // The last controller closes the active hold before it is gone.
        let mut guard = self.shared.core.lock().unwrap();
        guard.closed = Some(ClosedReason::Closed);
        let generation = guard
            .hold
            .as_ref()
            .map(|hold| hold.hold_generation)
            .unwrap_or(0);
        drop(guard);
        self.shared.cancel_hold(generation, ClosedReason::Closed);
    }
}

impl Controller {
    /// Arms the hold for `request.point` under `request.token`. One hold is
    /// active at a time; a second arm while one is armed or reached is
    /// refused, and a new arm never inherits a previous release.
    pub fn arm(&self, request: Arm) -> Result<Receipt, Error> {
        let mut guard = self.shared.core.lock().unwrap();

        if let Some(reason) = guard.closed {
            return Err(Error::Closed { reason });
        }
        if request.fence.run != guard.run.id || request.fence.instance != guard.run.instance {
            return Err(Error::WrongFence);
        }
        if !guard.points.contains(&request.point.as_str()) {
            return Err(Error::UnknownPoint);
        }
        if let Some(hold) = &guard.hold
            && matches!(hold.state, State::Armed | State::Reached)
        {
            return Err(Error::Busy);
        }

        guard.next_hold_generation += 1;
        guard.hold = Some(Hold {
            state: State::Armed,
            token: request.token,
            point: request.point,
            observation: None,
            instance: request.fence.instance,
            generation: request.fence.generation,
            armed_at: Instant::now(),
            hold_generation: guard.next_hold_generation,
            reason: None,
        });
        let receipt = self.shared.receipt(&guard).unwrap();
        guard.retained = Some(receipt.clone());
        drop(guard);
        self.shared.wake();

        Ok(receipt)
    }

    /// Waits until the operation reaches the armed point, returning the
    /// reached receipt with the captured observation, or the terminal
    /// failure. One waiter is allowed at a time.
    pub async fn wait_reached(&self, token: &Token) -> Result<Receipt, Error> {
        // Reserve the single waiter slot; released when this future ends,
        // even if it is cancelled.
        let _slot = {
            let mut guard = self.shared.core.lock().unwrap();
            let Some(hold) = guard.hold.as_mut() else {
                return Err(Error::UnknownToken);
            };
            if &hold.token != token {
                return Err(Error::UnknownToken);
            }
            match hold.state {
                State::Armed => {}
                State::Reached => return Ok(guard.retained.clone().unwrap()),
                terminal => return Err(terminal_error(&hold, terminal)),
            }
            if guard.wait_slot_taken {
                return Err(Error::WaitBusy);
            }
            guard.wait_slot_taken = true;
            WaitSlot {
                shared: Arc::clone(&self.shared),
            }
        };

        loop {
            let notified = self.shared.notify.notified();
            {
                let guard = self.shared.core.lock().unwrap();
                let Some(hold) = guard.hold.as_ref() else {
                    return Err(Error::UnknownToken);
                };
                match hold.state {
                    State::Reached => return Ok(guard.retained.clone().unwrap()),
                    State::Armed => {}
                    terminal => return Err(terminal_error(&hold, terminal)),
                }
            }
            notified.await;
        }
    }

    /// Releases the held operation. Release before reach fails; duplicate
    /// release of the same retained terminal token is idempotent; a wrong or
    /// stale token fails.
    pub fn release(&self, token: &Token) -> Result<Receipt, Error> {
        let mut guard = self.shared.core.lock().unwrap();
        let Some(hold) = guard.hold.as_mut() else {
            return Err(Error::UnknownToken);
        };
        if &hold.token != token {
            return Err(Error::UnknownToken);
        }

        match hold.state {
            State::Armed => Err(Error::NotReached),
            State::Reached => {
                hold.state = State::Released;
                guard.retained = self.shared.receipt(&guard);
                let receipt = guard.retained.clone().unwrap();
                drop(guard);
                self.shared.wake();
                Ok(receipt)
            }
            State::Released => Ok(guard.retained.clone().unwrap()),
            State::Cancelled | State::Expired | State::Closed => Err(Error::UnknownToken),
            State::Idle => unreachable!("a hold is never idle"),
        }
    }

    /// The retained last receipt of the hold matching `token`.
    pub fn snapshot(&self, token: &Token) -> Result<Receipt, Error> {
        let guard = self.shared.core.lock().unwrap();
        let Some(hold) = guard.hold.as_ref() else {
            return Err(Error::UnknownToken);
        };
        if &hold.token != token {
            return Err(Error::UnknownToken);
        }
        Ok(guard.retained.clone().unwrap())
    }

    /// The existing Bus worker select arm: watches the active hold's state
    /// changes and its one absolute lifetime deadline. Polling or cancelling
    /// and re-polling this future never extends the deadline. It closes the
    /// hold as expired when the deadline passes.
    pub async fn drive(&mut self) {
        let deadline = {
            let guard = self.shared.core.lock().unwrap();
            self.shared.deadline(&guard).filter(|_| {
                guard
                    .hold
                    .as_ref()
                    .is_some_and(|hold| matches!(hold.state, State::Armed | State::Reached))
            })
        };
        let Some(deadline) = deadline else {
            return;
        };

        tokio::select! {
            _ = self.changed() => {}
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {
                self.close(ClosedReason::Expired);
            }
        }
    }

    async fn changed(&self) {
        loop {
            let notified = self.shared.notify.notified();
            {
                let guard = self.shared.core.lock().unwrap();
                if guard.hold.is_none() {
                    return;
                }
            }
            notified.await;
            return;
        }
    }

    /// Closes the active hold and wakes every waiter. Waiters are released
    /// as cancellation, never as an automatic pass.
    pub fn close(&self, reason: ClosedReason) {
        let mut guard = self.shared.core.lock().unwrap();
        guard.closed = Some(reason);
        let generation = guard
            .hold
            .as_ref()
            .map(|hold| hold.hold_generation)
            .unwrap_or(0);
        drop(guard);
        self.shared.cancel_hold(generation, reason);
    }
}

/// The operation side of the barrier: called by the actual operation worker
/// at an approved point.
#[derive(Clone)]
pub struct Hook {
    shared: Arc<Shared>,
}

impl Hook {
    /// Reports that the actual operation reached `point` with the given
    /// observation. Without a matching armed hold it returns `None`
    /// immediately and the operation proceeds. A matching arm atomically
    /// transitions to `Reached` and returns its sole [`Permit`]; a second
    /// matching reach is refused.
    pub fn reach(
        &self,
        point: &'static str,
        observation: Observation,
    ) -> Result<Option<Permit>, Error> {
        let mut guard = self.shared.core.lock().unwrap();
        let Some(hold) = guard.hold.as_mut() else {
            return Ok(None);
        };

        match hold.state {
            State::Armed if hold.point == point => {
                hold.state = State::Reached;
                hold.observation = Some(observation);
                guard.retained = self.shared.receipt(&guard);
                let permit = Permit {
                    shared: Arc::clone(&self.shared),
                    hold_generation: hold.hold_generation,
                };
                drop(guard);
                self.shared.wake();
                Ok(Some(permit))
            }
            State::Reached if hold.point == point => Err(Error::AlreadyReached),
            _ => Ok(None),
        }
    }
}

/// The sole hold permit of one matching reach.
pub struct Permit {
    shared: Arc<Shared>,
    hold_generation: u64,
}

impl Permit {
    /// Blocks the already existing operation thread until the hold is
    /// released, using a condition variable with the hold's absolute
    /// deadline. A hold that ends without a release returns cancellation.
    pub fn wait_blocking(self) -> Result<(), Cancelled> {
        let deadline = {
            let guard = self.shared.core.lock().unwrap();
            self.shared
                .deadline(&guard)
                .expect("a permit exists only while its hold is active")
        };

        let mut guard = self.shared.core.lock().unwrap();
        loop {
            match self.state(&guard) {
                Wait::Continue => {
                    let now = Instant::now();
                    if now >= deadline {
                        drop(guard);
                        self.expire();
                        return Err(Cancelled {
                            reason: ClosedReason::Expired,
                        });
                    }
                    let (next, _timeout) = self
                        .shared
                        .condvar
                        .wait_timeout(guard, deadline - now)
                        .unwrap();
                    guard = next;
                }
                Wait::Released => return Ok(()),
                Wait::Failed(reason) => return Err(Cancelled { reason }),
            }
        }
    }

    /// Waits for the release on the existing worker runtime; see
    /// [`Permit::wait_blocking`].
    pub async fn wait(self) -> Result<(), Cancelled> {
        let shared = Arc::clone(&self.shared);
        let hold_generation = self.hold_generation;
        let deadline = {
            let guard = shared.core.lock().unwrap();
            shared
                .deadline(&guard)
                .expect("a permit exists only while its hold is active")
        };

        tokio::select! {
            result = async {
                loop {
                    let notified = shared.notify.notified();
                    match state_of(&shared, hold_generation) {
                        Wait::Continue => {}
                        Wait::Released => return Ok(()),
                        Wait::Failed(reason) => return Err(Cancelled { reason }),
                    }
                    notified.await;
                }
            } => result,
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {
                shared.cancel_hold(hold_generation, ClosedReason::Expired);
                Err(Cancelled { reason: ClosedReason::Expired })
            }
        }
    }

    fn state(&self, guard: &MutexGuard<'_, Core>) -> Wait {
        guard
            .hold
            .as_ref()
            .filter(|hold| hold.hold_generation == self.hold_generation)
            .map(state_wait)
            .unwrap_or(Wait::Failed(ClosedReason::Closed))
    }

    fn expire(&self) {
        self.shared
            .cancel_hold(self.hold_generation, ClosedReason::Expired);
    }
}

enum Wait {
    Continue,
    Released,
    Failed(ClosedReason),
}

fn state_of(shared: &Shared, hold_generation: u64) -> Wait {
    let guard = shared.core.lock().unwrap();
    guard
        .hold
        .as_ref()
        .filter(|hold| hold.hold_generation == hold_generation)
        .map(state_wait)
        .unwrap_or(Wait::Failed(ClosedReason::Closed))
}

fn state_wait(hold: &Hold) -> Wait {
    match hold.state {
        State::Armed | State::Reached => Wait::Continue,
        State::Released => Wait::Released,
        State::Cancelled => Wait::Failed(ClosedReason::PermitDropped),
        State::Expired => Wait::Failed(ClosedReason::Expired),
        State::Closed => Wait::Failed(hold.reason.unwrap_or(ClosedReason::Closed)),
        State::Idle => Wait::Failed(ClosedReason::Closed),
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        // Dropping an unreleased permit cancels the hold.
        self.shared
            .cancel_hold(self.hold_generation, ClosedReason::PermitDropped);
    }
}

fn terminal_error(hold: &Hold, state: State) -> Error {
    match state {
        State::Cancelled => Error::Cancelled {
            reason: ClosedReason::PermitDropped,
        },
        State::Expired => Error::Cancelled {
            reason: ClosedReason::Expired,
        },
        State::Closed => Error::Cancelled {
            reason: hold.reason.unwrap_or(ClosedReason::Closed),
        },
        _ => Error::UnknownToken,
    }
}

struct WaitSlot {
    shared: Arc<Shared>,
}

impl Drop for WaitSlot {
    fn drop(&mut self) {
        self.shared.core.lock().unwrap().wait_slot_taken = false;
    }
}

/// Installs one process-wide operation barrier for the given points and run,
/// returning the fixture [`Controller`] and the operation [`Hook`].
pub fn barrier(points: &'static [&'static str], run: Run) -> (Controller, Hook) {
    barrier_with(points, run, LIFETIME)
}

/// [`barrier`] with an explicit hold lifetime, for deterministic tests.
#[doc(hidden)]
pub fn barrier_with(
    points: &'static [&'static str],
    run: Run,
    lifetime: Duration,
) -> (Controller, Hook) {
    let shared = Arc::new(Shared {
        core: Mutex::new(Core {
            run,
            points,
            hold: None,
            retained: None,
            wait_slot_taken: false,
            closed: None,
            next_hold_generation: 0,
        }),
        condvar: Condvar::new(),
        notify: Notify::new(),
        lifetime,
    });
    let liveness = Arc::new(Liveness {
        shared: Arc::clone(&shared),
    });
    (
        Controller {
            shared: Arc::clone(&shared),
            liveness,
        },
        Hook { shared },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> (Controller, Hook, Run) {
        let run = Run::new("fixture-run", 7).unwrap();
        let (controller, hook) = barrier_with(
            &["dopus.before_execute"],
            run.clone(),
            Duration::from_secs(10),
        );
        (controller, hook, run)
    }

    fn arm(run: &Run, point: &str, token: &str) -> Arm {
        Arm {
            fence: Fence {
                run: run.id().to_owned(),
                instance: run.instance(),
                generation: 1,
            },
            point: point.to_owned(),
            token: Token::try_new(token).unwrap(),
        }
    }

    fn observation() -> Observation {
        Observation::try_new("copy a -> b").unwrap()
    }

    #[test]
    fn reach_without_an_arm_proceeds_immediately() {
        let (controller, hook, run) = pair();
        assert_eq!(
            hook.reach("dopus.before_execute", observation()).unwrap(),
            None
        );
        assert!(controller.snapshot(&Token::try_new("t").unwrap()).is_err());
        let _ = run;
    }

    #[test]
    fn arm_reach_release_is_the_complete_hold() {
        let (controller, hook, run) = pair();
        let token = Token::try_new("hold-1").unwrap();
        let receipt = controller
            .arm(arm(&run, "dopus.before_execute", "hold-1"))
            .unwrap();
        assert_eq!(receipt.state, State::Armed);
        assert_eq!(receipt.token, token);

        // A second arm while the hold is active is refused.
        assert_eq!(
            controller.arm(arm(&run, "dopus.before_execute", "hold-2")),
            Err(Error::Busy)
        );
        // An unknown point and a wrong fence are refused up front.
        assert_eq!(
            controller.arm(arm(&run, "cap.after_minimize", "hold-2")),
            Err(Error::UnknownPoint)
        );
        let mut wrong = arm(&run, "dopus.before_execute", "hold-2");
        wrong.fence.instance = 9;
        assert_eq!(controller.arm(wrong), Err(Error::WrongFence));

        // Release before reach must fail.
        assert_eq!(controller.release(&token), Err(Error::NotReached));

        let permit = hook
            .reach("dopus.before_execute", observation())
            .unwrap()
            .expect("a matching arm holds the operation");
        assert_eq!(
            hook.reach("dopus.before_execute", observation()),
            Err(Error::AlreadyReached)
        );
        assert_eq!(controller.snapshot(&token).unwrap().state, State::Reached);

        let released = controller.release(&token).unwrap();
        assert_eq!(released.state, State::Released);
        assert_eq!(
            released.observation.as_ref().unwrap().as_str(),
            "copy a -> b"
        );
        // Duplicate release of the retained terminal token is idempotent.
        assert_eq!(controller.release(&token).unwrap(), released);

        // The release unblocks the operation.
        assert_eq!(permit.wait_blocking(), Ok(()));
    }

    #[test]
    fn unreached_points_do_not_hold_the_operation() {
        let (controller, hook, run) = pair();
        let token = Token::try_new("hold-1").unwrap();
        controller
            .arm(arm(&run, "dopus.before_execute", "hold-1"))
            .unwrap();
        // A different point proceeds; the armed point still holds later.
        assert_eq!(hook.reach("other.point", observation()).unwrap(), None);
        assert_eq!(controller.snapshot(&token).unwrap().state, State::Armed);
        controller.release(&token).unwrap_err();
    }

    #[test]
    fn a_new_arm_never_inherits_a_previous_release() {
        let (controller, hook, run) = pair();
        controller
            .arm(arm(&run, "dopus.before_execute", "first"))
            .unwrap();
        let permit = hook.reach("dopus.before_execute", observation()).unwrap();
        controller
            .release(&Token::try_new("first").unwrap())
            .unwrap();
        assert_eq!(permit.unwrap().wait_blocking(), Ok(()));

        // A fresh arm holds a fresh operation; the old token is stale.
        controller
            .arm(arm(&run, "dopus.before_execute", "second"))
            .unwrap();
        assert_eq!(
            controller.release(&Token::try_new("first").unwrap()),
            Err(Error::UnknownToken)
        );
        let permit = hook.reach("dopus.before_execute", observation()).unwrap();
        controller
            .release(&Token::try_new("second").unwrap())
            .unwrap();
        assert_eq!(permit.unwrap().wait_blocking(), Ok(()));
    }

    #[test]
    fn dropping_an_unreleased_permit_cancels_the_hold() {
        let (controller, hook, run) = pair();
        let token = Token::try_new("hold-1").unwrap();
        controller
            .arm(arm(&run, "dopus.before_execute", "hold-1"))
            .unwrap();

        let permit = hook
            .reach("dopus.before_execute", observation())
            .unwrap()
            .unwrap();
        // The operation aborts without waiting: the unreleased permit
        // cancels the hold rather than leaving it armed forever.
        drop(permit);

        assert_eq!(controller.snapshot(&token).unwrap().state, State::Cancelled);
        assert_eq!(controller.release(&token), Err(Error::UnknownToken));
    }

    #[test]
    fn an_absolute_lifetime_expires_a_held_operation() {
        let run = Run::new("fixture-run", 7).unwrap();
        let (controller, hook) = barrier_with(
            &["dopus.before_execute"],
            run.clone(),
            Duration::from_millis(60),
        );
        controller
            .arm(arm(&run, "dopus.before_execute", "hold-1"))
            .unwrap();
        let permit = hook
            .reach("dopus.before_execute", observation())
            .unwrap()
            .unwrap();

        let started = Instant::now();
        let result = permit.wait_blocking();
        assert!(started.elapsed() >= Duration::from_millis(60));
        assert_eq!(
            result,
            Err(Cancelled {
                reason: ClosedReason::Expired
            })
        );
        assert_eq!(
            controller
                .snapshot(&Token::try_new("hold-1").unwrap())
                .unwrap()
                .state,
            State::Expired
        );
    }

    #[test]
    fn shutdown_closes_the_hold_and_wakes_the_operation() {
        let (controller, hook, run) = pair();
        let token = Token::try_new("hold-1").unwrap();
        controller
            .arm(arm(&run, "dopus.before_execute", "hold-1"))
            .unwrap();
        let permit = hook.reach("dopus.before_execute", observation()).unwrap();

        let handle = std::thread::spawn(move || permit.unwrap().wait_blocking());
        controller.close(ClosedReason::Shutdown);

        assert_eq!(
            handle.join().unwrap(),
            Err(Cancelled {
                reason: ClosedReason::Shutdown
            })
        );
        assert_eq!(controller.snapshot(&token).unwrap().state, State::Closed);
        // A closed controller refuses new arms.
        assert_eq!(
            controller.arm(arm(&run, "dopus.before_execute", "next")),
            Err(Error::Closed {
                reason: ClosedReason::Shutdown
            })
        );
    }

    #[test]
    fn string_and_observation_limits_are_enforced() {
        assert!(Run::new("", 1).is_err());
        assert!(Run::new("x".repeat(65), 1).is_err());
        assert!(Token::try_new("").is_err());
        assert!(Token::try_new("x".repeat(65)).is_err());
        assert!(Observation::try_new("x".repeat(4_097)).is_err());
        assert!(Observation::try_new("x".repeat(4_096)).is_ok());
    }

    #[test]
    fn dropped_controller_closes_an_active_hold() {
        let run = Run::new("fixture-run", 7).unwrap();
        let (controller, hook) = barrier_with(
            &["dopus.before_execute"],
            run.clone(),
            Duration::from_secs(10),
        );
        controller
            .arm(arm(&run, "dopus.before_execute", "hold-1"))
            .unwrap();
        let permit = hook.reach("dopus.before_execute", observation()).unwrap();

        let handle = std::thread::spawn(move || permit.unwrap().wait_blocking());
        drop(controller);

        assert_eq!(
            handle.join().unwrap(),
            Err(Cancelled {
                reason: ClosedReason::Closed
            })
        );
    }
}

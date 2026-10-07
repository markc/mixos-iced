// SPDX-License-Identifier: MIT OR Apache-2.0
//! Bounded delivery and admission primitives shared by native applications.
//!
//! [`Outbox`] retains work an actor cannot deliver yet: a bounded reliable
//! FIFO plus a fixed number of replaceable slots. [`Admission`] bounds
//! accepted work as a whole; every acquired [`Permit`] is either explicitly
//! [`finish`](Permit::finish)ed as a handled terminal outcome or counted as
//! abandoned when dropped unfinished. Both helpers are synchronous,
//! transport- and renderer-neutral and require only `std`; actors keep their
//! own select loops, receipts and shutdown sequencing.
//!
//! An outbox is mutated by one owner after the transport reports readiness.
//! No item is ever parked inside an async send future, where cancelling that
//! future inside `select` could destroy the sole retained value. The actor
//! drives the sender's `poll_ready` through `std::future::poll_fn` in its own
//! select, and recovers `try_send` failures with `is_full()`/`into_inner()`:
//!
//! ```ignore
//! ready = std::future::poll_fn(|cx| gui.poll_ready(cx)),
//!     if !outbox.is_empty() => {
//!     match ready {
//!         Ok(()) => match outbox.flush_with(|item| match gui.try_send(item) {
//!             Ok(()) => Ok(()),
//!             Err(err) if err.is_full() => Err(SendError::Full(err.into_inner())),
//!             Err(err) => Err(SendError::Closed(err.into_inner())),
//!         }) {
//!             Flush::Empty | Flush::Full => {}
//!             Flush::Closed => { /* owned shutdown; retire via drain */ }
//!         },
//!         Err(_) => { /* receiver gone; retire via drain */ }
//!     }
//! }
//! ```
//!
//! The helpers are deliberately ignorant of protocols, deadlines and per-app
//! classification. Hosts decide which deliveries are replaceable slot state
//! (a coalesced Settings wake, the latest connection snapshot) and which are
//! reliable FIFO entries (accepted commands, handoff results, replies the GUI
//! is awaiting), and keep origin checks, receipts and shutdown deadlines
//! local. See `docs/dev/application-native-queues.md`.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

/// One retained delivery: either a reliable FIFO entry or a marker naming one
/// of the fixed replaceable slots.
///
/// The deque order is the delivery order. A slot's marker is appended when
/// the slot is first occupied and never moves afterwards; replacing the value
/// does not move the marker, so continuously refreshed state keeps its
/// original position instead of being pushed behind newer reliable work.
enum Entry<T> {
    Reliable(T),
    Slot(usize),
}

/// A bounded retained outbox for deliveries an actor cannot send yet.
///
/// Reliable entries share one FIFO limited to `reliable_capacity`; the
/// `SLOTS` fixed slots hold replaceable values such as coalesced Settings
/// wakes. Total retention is at most `reliable_capacity + SLOTS`. Nothing is
/// discarded by the helper itself: full and closed receivers hand the item
/// back at its original queue position, and [`drain`](Self::drain) retires
/// whatever remains when the actor shuts down.
pub struct Outbox<T, const SLOTS: usize> {
    order: VecDeque<Entry<T>>,
    slots: [Option<T>; SLOTS],
    reliable_capacity: usize,
    reliable: usize,
}

impl<T, const SLOTS: usize> Outbox<T, SLOTS> {
    /// Create an empty outbox retaining at most `reliable_capacity` reliable
    /// entries in addition to its `SLOTS` replaceable slots.
    pub fn new(reliable_capacity: usize) -> Self {
        Self {
            order: VecDeque::new(),
            slots: std::array::from_fn(|_| None),
            reliable_capacity,
            reliable: 0,
        }
    }

    /// Reserve a reliable FIFO entry, or return the value unchanged when the
    /// reliable capacity is exhausted. A returned value is not load-shedding:
    /// the caller must decide whether to refuse, retire or store it.
    pub fn push(&mut self, value: T) -> Result<(), T> {
        if self.reliable >= self.reliable_capacity {
            return Err(value);
        }
        self.reliable += 1;
        self.order.push_back(Entry::Reliable(value));
        Ok(())
    }

    /// Store or replace the value in fixed slot `slot`, returning the value
    /// it superseded (or the input unchanged for an index at or past
    /// `SLOTS`). The first insertion fixes the slot's queue position;
    /// replacement updates the value without moving that position.
    pub fn replace(&mut self, slot: usize, value: T) -> Result<Option<T>, T> {
        let Some(target) = self.slots.get_mut(slot) else {
            return Err(value);
        };
        match target {
            Some(previous) => Ok(Some(std::mem::replace(previous, value))),
            None => {
                *target = Some(value);
                self.order.push_back(Entry::Slot(slot));
                Ok(None)
            }
        }
    }

    /// Whether no reliable entries and no occupied slots are retained.
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// The number of retained deliveries, reliable entries and occupied
    /// slots together.
    pub fn len(&self) -> usize {
        self.order.len()
    }

    /// The number of retained reliable entries.
    pub fn reliable_len(&self) -> usize {
        self.reliable
    }

    /// Retire deliveries for which `keep` returns false, preserving every
    /// surviving entry's position. The owner must record terminal outcomes
    /// before discarding work; this method drops only the selected payloads
    /// and releases their queue capacity. A removed slot can be occupied again
    /// at the back, while surviving slots keep their original markers.
    pub fn retain(&mut self, mut keep: impl FnMut(&T) -> bool) {
        let Self { order, slots, reliable, .. } = self;
        order.retain(|entry| match entry {
            Entry::Reliable(value) => {
                let retained = keep(value);
                if !retained {
                    *reliable -= 1;
                }
                retained
            }
            Entry::Slot(index) => {
                let retained = keep(slots[*index].as_ref().expect("an occupied slot marker names an occupied slot"));
                if !retained {
                    slots[*index] = None;
                }
                retained
            }
        });
    }

    /// Hand each retained delivery to `send` in queue order.
    ///
    /// `Ok(())` means the sender took ownership. On the first `Full` or
    /// `Closed` error the item is restored at its original front position
    /// and flushing stops; the helper never discards an item because the
    /// receiver is full or gone. `Empty` means every retained delivery was
    /// handed to the sender. Callers must only invoke this after the
    /// transport reports readiness, and must never hold a taken item across
    /// an await point.
    pub fn flush_with(&mut self, mut send: impl FnMut(T) -> Result<(), SendError<T>>) -> Flush {
        loop {
            match self.order.front() {
                None => return Flush::Empty,
                Some(Entry::Reliable(_)) => {
                    let value = match self.order.pop_front() {
                        Some(Entry::Reliable(value)) => value,
                        _ => unreachable!("the front entry was just a reliable value"),
                    };
                    match send(value) {
                        Ok(()) => self.reliable -= 1,
                        Err(SendError::Full(value)) => {
                            self.order.push_front(Entry::Reliable(value));
                            return Flush::Full;
                        }
                        Err(SendError::Closed(value)) => {
                            self.order.push_front(Entry::Reliable(value));
                            return Flush::Closed;
                        }
                    }
                }
                Some(Entry::Slot(slot)) => {
                    let slot = *slot;
                    let value = self.slots[slot]
                        .take()
                        .expect("an occupied slot marker names an occupied slot");
                    match send(value) {
                        Ok(()) => {
                            self.order.pop_front();
                        }
                        Err(SendError::Full(value)) => {
                            self.slots[slot] = Some(value);
                            return Flush::Full;
                        }
                        Err(SendError::Closed(value)) => {
                            self.slots[slot] = Some(value);
                            return Flush::Closed;
                        }
                    }
                }
            }
        }
    }

    /// Retire every retained delivery in queue order, emptying the outbox.
    ///
    /// The actor uses this to release remaining work explicitly when the
    /// receiver is closed or its shutdown deadline expires; the helper itself
    /// never discards anything.
    pub fn drain(&mut self) -> impl Iterator<Item = T> + '_ {
        Drain { outbox: self }
    }
}

struct Drain<'a, T, const SLOTS: usize> {
    outbox: &'a mut Outbox<T, SLOTS>,
}

impl<T, const SLOTS: usize> Iterator for Drain<'_, T, SLOTS> {
    type Item = T;

    fn next(&mut self) -> Option<T> {
        match self.outbox.order.pop_front()? {
            Entry::Reliable(value) => {
                self.outbox.reliable -= 1;
                Some(value)
            }
            Entry::Slot(slot) => Some(
                self.outbox.slots[slot]
                    .take()
                    .expect("an occupied slot marker names an occupied slot"),
            ),
        }
    }
}

/// Why a send attempt inside [`Outbox::flush_with`] could not deliver the
/// item: the receiver had no capacity, or it is closed. The item is always
/// carried back; flush restores it at its original position.
#[derive(Debug, PartialEq, Eq)]
pub enum SendError<T> {
    /// The receiver's capacity is exhausted; the item was not delivered.
    Full(T),
    /// The receiver is closed; the item can never be delivered.
    Closed(T),
}

/// The outcome of one [`Outbox::flush_with`] pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flush {
    /// Every retained delivery was handed to the sender (or the outbox was
    /// already empty).
    Empty,
    /// The receiver ran out of capacity; the unsent item and everything
    /// behind it remain retained in order.
    Full,
    /// The receiver is closed; the unsendable item and everything behind it
    /// remain retained for the actor to retire with [`Outbox::drain`].
    Closed,
}

/// A shared budget for accepted work: commands awaiting the GUI, retained
/// responses and running or unreaped response tasks together.
///
/// Synchronous and nonblocking; the internal mutex is only ever held across
/// a counter update, never across host code or an await. A poisoned lock is
/// recovered like [`crate::message::Once`].
#[derive(Clone)]
pub struct Admission(Arc<Mutex<State>>);

/// One reserved unit of accepted work, owned by the actor from acceptance
/// through reaping. Deliberately neither `Clone` nor `Copy`: an acquired
/// permit is finished or abandoned exactly once.
pub struct Permit {
    admission: Option<Admission>,
}

/// Snapshot of one [`Admission`] pool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Counts {
    /// The configured acquisition limit.
    pub limit: usize,
    /// Permits currently acquired and not yet finished or abandoned.
    pub active: usize,
    /// Permits explicitly [`finish`](Permit::finish)ed as handled outcomes.
    pub finished: u64,
    /// Permits dropped unfinished, including panics and aborted tasks.
    pub abandoned: u64,
}

struct State {
    limit: usize,
    active: usize,
    finished: u64,
    abandoned: u64,
}

impl Admission {
    /// Create a pool admitting at most `limit` permits at once.
    pub fn new(limit: usize) -> Self {
        Self(Arc::new(Mutex::new(State {
            limit,
            active: 0,
            finished: 0,
            abandoned: 0,
        })))
    }

    /// Reserve one unit of work if capacity remains, else `None`. The permit
    /// stays live across GUI waits, retained replies, response tasks and
    /// completed-but-unreaped task outputs until it is
    /// [`finish`](Permit::finish)ed by the owner.
    pub fn try_acquire(&self) -> Option<Permit> {
        let mut state = self.lock();
        if state.active >= state.limit {
            return None;
        }
        state.active += 1;
        Some(Permit {
            admission: Some(self.clone()),
        })
    }

    /// A consistent snapshot of the pool counters.
    pub fn counts(&self) -> Counts {
        let state = self.lock();
        Counts {
            limit: state.limit,
            active: state.active,
            finished: state.finished,
            abandoned: state.abandoned,
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Permit {
    /// Record an explicitly handled terminal outcome and release the slot.
    ///
    /// A failed response is finished after its failure is recorded; finishing
    /// claims a handled outcome, not a successful reply. Consuming `self`
    /// makes a second finish impossible.
    pub fn finish(mut self) {
        let admission = self
            .admission
            .take()
            .expect("an acquired permit is finished exactly once");
        let mut state = admission.lock();
        state.active -= 1;
        state.finished = state.finished.saturating_add(1);
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        // Releasing an unfinished permit means the work ended without an
        // explicit terminal outcome: an aborted task, a panic, an owner drop.
        if let Some(admission) = self.admission.take() {
            let mut state = admission.lock();
            state.active -= 1;
            state.abandoned = state.abandoned.saturating_add(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    /// A non-Clone payload that counts its own drops.
    struct Tracked(u32, Arc<AtomicUsize>);
    impl Tracked {
        fn new(value: u32, drops: &Arc<AtomicUsize>) -> Self {
            Self(value, Arc::clone(drops))
        }
        fn value(&self) -> u32 {
            self.0
        }
    }
    impl Drop for Tracked {
        fn drop(&mut self) {
            self.1.fetch_add(1, Ordering::SeqCst);
        }
    }
    // `Arc<AtomicUsize>` is not comparable, so equality compares the payload
    // value only; `Tracked` stays deliberately non-Clone.
    impl PartialEq for Tracked {
        fn eq(&self, other: &Self) -> bool {
            self.0 == other.0
        }
    }
    impl std::fmt::Debug for Tracked {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "Tracked({})", self.0)
        }
    }

    /// A sender with simulated finite receiver capacity.
    struct Receiver {
        capacity: usize,
        delivered: Vec<u32>,
    }
    impl Receiver {
        fn send(&mut self, item: u32) -> Result<(), SendError<u32>> {
            if self.capacity == 0 {
                return Err(SendError::Full(item));
            }
            self.capacity -= 1;
            self.delivered.push(item);
            Ok(())
        }
    }

    #[test]
    fn reliable_capacity_saturates_and_rejects_input_unchanged() {
        let mut outbox = Outbox::<u32, 2>::new(3);
        assert_eq!(outbox.push(1), Ok(()));
        assert_eq!(outbox.push(2), Ok(()));
        assert_eq!(outbox.push(3), Ok(()));
        assert_eq!(outbox.len(), 3);
        assert_eq!(outbox.reliable_len(), 3);
        assert_eq!(outbox.push(4), Err(4));
        assert_eq!(outbox.len(), 3);
        assert_eq!(outbox.reliable_len(), 3);
        // Slots are independent of the reliable capacity.
        assert_eq!(outbox.replace(0, 10), Ok(None));
        assert_eq!(outbox.replace(1, 11), Ok(None));
        assert_eq!(outbox.len(), 5);
        assert_eq!(outbox.reliable_len(), 3);
    }

    #[test]
    fn slots_are_fixed_and_reject_indices_at_or_past_the_bound() {
        let mut outbox = Outbox::<u32, 2>::new(0);
        assert_eq!(outbox.replace(0, 10), Ok(None));
        assert_eq!(outbox.replace(1, 11), Ok(None));
        assert_eq!(outbox.replace(2, 12), Err(12));
        assert_eq!(outbox.len(), 2);
    }

    #[test]
    fn zero_slots_rejects_every_replacement() {
        let mut outbox = Outbox::<u32, 0>::new(2);
        assert_eq!(outbox.replace(0, 9), Err(9));
        assert_eq!(outbox.push(1), Ok(()));
        assert_eq!(outbox.push(2), Ok(()));
        assert_eq!(outbox.push(3), Err(3));
    }

    #[test]
    fn replacement_updates_a_slot_without_moving_its_queue_position() {
        let mut outbox = Outbox::<u32, 1>::new(2);
        assert_eq!(outbox.replace(0, 100), Ok(None));
        assert_eq!(outbox.push(200), Ok(()));
        assert_eq!(outbox.replace(0, 101), Ok(Some(100)));
        assert_eq!(outbox.push(300), Ok(()));
        // The wake keeps its original front position but carries the latest
        // value; the reliable entries queued behind it follow in order.
        let mut receiver = Receiver {
            capacity: 3,
            delivered: Vec::new(),
        };
        assert_eq!(outbox.flush_with(|item| receiver.send(item)), Flush::Empty);
        assert_eq!(receiver.delivered, [101, 200, 300]);
        assert!(outbox.is_empty());
    }

    #[test]
    fn retirement_releases_capacity_and_preserves_surviving_slot_positions() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut outbox = Outbox::<Tracked, 2>::new(3);
        outbox.replace(0, Tracked::new(10, &drops)).unwrap();
        outbox.push(Tracked::new(1, &drops)).unwrap();
        outbox.replace(1, Tracked::new(20, &drops)).unwrap();
        outbox.push(Tracked::new(2, &drops)).unwrap();
        outbox.push(Tracked::new(3, &drops)).unwrap();
        outbox.retain(|item| !matches!(item.value(), 1 | 3 | 20));
        assert_eq!(drops.load(Ordering::SeqCst), 3);
        assert_eq!(outbox.len(), 2);
        assert_eq!(outbox.reliable_len(), 1);
        outbox.push(Tracked::new(4, &drops)).unwrap();
        outbox.push(Tracked::new(5, &drops)).unwrap();
        // The existing slot stays in front; only the retired slot gets a
        // new position. No command can displace that retained wake.
        drop(outbox.replace(0, Tracked::new(11, &drops)).unwrap());
        outbox.replace(1, Tracked::new(21, &drops)).unwrap();
        assert_eq!(outbox.drain().map(|item| item.value()).collect::<Vec<_>>(), [11, 2, 4, 5, 21]);
        assert_eq!(outbox.reliable_len(), 0);
        assert!(outbox.is_empty());
        assert_eq!(drops.load(Ordering::SeqCst), 9);
    }

    #[test]
    fn replacement_returns_the_superseded_value_for_explicit_retirement() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut outbox = Outbox::<Tracked, 1>::new(0);
        assert_eq!(outbox.replace(0, Tracked::new(1, &drops)), Ok(None));
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        let superseded = outbox
            .replace(0, Tracked::new(2, &drops))
            .expect("slot 0 exists");
        assert_eq!(superseded.expect("the slot was occupied").value(), 1);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        let retired = outbox.drain().map(|item| item.value()).collect::<Vec<_>>();
        assert_eq!(retired, [2]);
        assert_eq!(drops.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn rejected_reliable_and_slot_inputs_return_ownership_unchanged() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut outbox = Outbox::<Tracked, 1>::new(0);
        // A full reliable FIFO hands a non-Clone input back to its owner.
        let reliable = outbox
            .push(Tracked::new(1, &drops))
            .expect_err("the reliable capacity is zero");
        assert_eq!(reliable.value(), 1);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        // An invalid slot index hands a non-Clone input back too; neither
        // rejection is load-shedding.
        let slotted = outbox
            .replace(1, Tracked::new(2, &drops))
            .expect_err("only slot 0 exists");
        assert_eq!(slotted.value(), 2);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        // The owner still holds both values and retires them itself.
        drop(reliable);
        drop(slotted);
        assert_eq!(drops.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn sustained_traffic_cannot_starve_an_already_queued_wake() {
        let mut outbox = Outbox::<u32, 1>::new(4);
        assert_eq!(outbox.replace(0, 1), Ok(None)); // the retained Settings wake
        for value in 2..=5 {
            assert_eq!(outbox.push(value), Ok(()));
        }
        // Continuous replacement never pushes the wake back: the first
        // replacement supersedes the initially retained wake, and each later
        // one supersedes its immediate predecessor in place.
        assert_eq!(outbox.replace(0, 6), Ok(Some(1)));
        for wake in 7..=20 {
            assert_eq!(outbox.replace(0, wake), Ok(Some(wake - 1)));
        }
        assert_eq!(outbox.len(), 5);
        assert_eq!(outbox.reliable_len(), 4);
        let mut receiver = Receiver {
            capacity: 5,
            delivered: Vec::new(),
        };
        assert_eq!(outbox.flush_with(|item| receiver.send(item)), Flush::Empty);
        // ...and the latest wake goes out first, exactly once.
        assert_eq!(receiver.delivered, [20, 2, 3, 4, 5]);
        // A publish while the UI drains its Mailbox leaves another wake.
        assert_eq!(outbox.replace(0, 21), Ok(None));
        let mut receiver = Receiver {
            capacity: 1,
            delivered: Vec::new(),
        };
        assert_eq!(outbox.flush_with(|item| receiver.send(item)), Flush::Empty);
        assert_eq!(receiver.delivered, [21]);
    }

    #[test]
    fn reliable_fifo_survives_a_full_send_part_way_through() {
        let mut outbox = Outbox::<u32, 1>::new(3);
        assert_eq!(outbox.push(1), Ok(()));
        assert_eq!(outbox.push(2), Ok(()));
        assert_eq!(outbox.push(3), Ok(()));
        // The receiver takes one item and then reports full.
        let mut taken = 0;
        assert_eq!(
            outbox.flush_with(|item| {
                if taken == 0 {
                    taken += 1;
                    Ok(())
                } else {
                    Err(SendError::Full(item))
                }
            }),
            Flush::Full
        );
        assert_eq!(outbox.len(), 2);
        assert_eq!(outbox.reliable_len(), 2);
        let mut receiver = Receiver {
            capacity: 2,
            delivered: Vec::new(),
        };
        assert_eq!(outbox.flush_with(|item| receiver.send(item)), Flush::Empty);
        assert_eq!(receiver.delivered, [2, 3]);
    }

    #[test]
    fn full_restores_every_pending_item_exactly_once() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut outbox = Outbox::<Tracked, 1>::new(2);
        assert!(outbox.push(Tracked::new(1, &drops)).is_ok());
        assert_eq!(outbox.replace(0, Tracked::new(2, &drops)), Ok(None));
        // Cancelled readiness and full receivers: nothing is delivered or
        // lost, however many times the actor attempts the flush.
        for _ in 0..3 {
            assert_eq!(
                outbox.flush_with(|item| Err(SendError::Full(item))),
                Flush::Full
            );
            assert_eq!(outbox.len(), 2);
            assert_eq!(outbox.reliable_len(), 1);
            assert_eq!(drops.load(Ordering::SeqCst), 0);
        }
        // Capacity arrives; both items are delivered once, in queue order.
        let delivered = outbox.drain().map(|item| item.value()).collect::<Vec<_>>();
        assert_eq!(delivered, [1, 2]);
        assert_eq!(drops.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_sole_retained_item_survives_full_and_closed_without_loss() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut outbox = Outbox::<Tracked, 0>::new(1);
        assert!(outbox.push(Tracked::new(7, &drops)).is_ok());
        assert_eq!(
            outbox.flush_with(|item| Err(SendError::Full(item))),
            Flush::Full
        );
        assert_eq!(
            outbox.flush_with(|item| Err(SendError::Closed(item))),
            Flush::Closed
        );
        assert_eq!(outbox.len(), 1);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        let retired = outbox.drain().map(|item| item.value()).collect::<Vec<_>>();
        assert_eq!(retired, [7]);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn closed_restores_the_unsent_item_and_drain_retires_the_rest() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut outbox = Outbox::<Tracked, 1>::new(3);
        assert!(outbox.push(Tracked::new(1, &drops)).is_ok());
        assert!(outbox.push(Tracked::new(2, &drops)).is_ok());
        assert_eq!(outbox.replace(0, Tracked::new(3, &drops)), Ok(None));
        let mut sent = Vec::new();
        assert_eq!(
            outbox.flush_with(|item| {
                if sent.is_empty() {
                    sent.push(item.value());
                    Ok(())
                } else {
                    Err(SendError::Closed(item))
                }
            }),
            Flush::Closed
        );
        assert_eq!(sent, [1]);
        // Closure stops flushing, it does not start a ready-loop: the front
        // item is restored and everything behind it is still retained.
        assert_eq!(outbox.len(), 2);
        assert_eq!(outbox.reliable_len(), 1);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        // The actor retires the remaining work explicitly.
        let retired = outbox.drain().map(|item| item.value()).collect::<Vec<_>>();
        assert_eq!(retired, [2, 3]);
        assert!(outbox.is_empty());
        assert_eq!(drops.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn freed_receiver_capacity_delivers_a_retained_wake_with_no_other_event() {
        let mut outbox = Outbox::<u32, 1>::new(0);
        assert_eq!(outbox.replace(0, 42), Ok(None));
        let mut receiver = Receiver {
            capacity: 0,
            delivered: Vec::new(),
        };
        assert_eq!(outbox.flush_with(|item| receiver.send(item)), Flush::Full);
        assert_eq!(receiver.delivered, Vec::<u32>::new());
        // The GUI drains its channel; nothing else wakes the actor, but the
        // wake is still retained and ready.
        receiver.capacity = 1;
        assert_eq!(outbox.flush_with(|item| receiver.send(item)), Flush::Empty);
        assert_eq!(receiver.delivered, [42]);
        assert!(outbox.is_empty());
    }

    #[test]
    fn full_restores_the_front_item_in_place_whatever_its_kind() {
        let mut outbox = Outbox::<u32, 1>::new(2);
        assert_eq!(outbox.replace(0, 1), Ok(None));
        assert_eq!(outbox.push(2), Ok(()));
        // The slot marker is the front entry and the receiver is full.
        assert_eq!(
            outbox.flush_with(|item| Err(SendError::Full(item))),
            Flush::Full
        );
        assert_eq!(outbox.len(), 2);
        // One slot of capacity frees: the slot value, still at the front,
        // goes first and the reliable entry stays behind it.
        let mut receiver = Receiver {
            capacity: 1,
            delivered: Vec::new(),
        };
        assert_eq!(outbox.flush_with(|item| receiver.send(item)), Flush::Full);
        assert_eq!(receiver.delivered, [1]);
        assert_eq!(outbox.len(), 1);
        receiver.capacity = 1;
        assert_eq!(outbox.flush_with(|item| receiver.send(item)), Flush::Empty);
        assert_eq!(receiver.delivered, [1, 2]);
    }

    #[test]
    fn closed_restores_a_slot_front_value_and_drain_retires_it_once() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut outbox = Outbox::<Tracked, 1>::new(1);
        assert_eq!(outbox.replace(0, Tracked::new(1, &drops)), Ok(None));
        assert!(outbox.push(Tracked::new(2, &drops)).is_ok());
        // The slot marker is the front entry and the receiver is closed: the
        // slot value is restored in place and nothing is dropped or lost.
        assert_eq!(
            outbox.flush_with(|item| Err(SendError::Closed(item))),
            Flush::Closed
        );
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        assert_eq!(outbox.len(), 2);
        assert_eq!(outbox.reliable_len(), 1);
        let retired = outbox.drain().map(|item| item.value()).collect::<Vec<_>>();
        assert_eq!(retired, [1, 2]);
        assert!(outbox.is_empty());
        assert_eq!(drops.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn reliable_and_slotted_items_deliver_in_queue_order() {
        let mut outbox = Outbox::<u32, 1>::new(2);
        assert_eq!(outbox.push(1), Ok(()));
        assert_eq!(outbox.replace(0, 2), Ok(None));
        assert_eq!(outbox.push(3), Ok(()));
        let mut receiver = Receiver {
            capacity: 3,
            delivered: Vec::new(),
        };
        assert_eq!(outbox.flush_with(|item| receiver.send(item)), Flush::Empty);
        assert_eq!(receiver.delivered, [1, 2, 3]);
        // Draining after a completed flush retires nothing.
        assert_eq!(outbox.drain().count(), 0);
    }

    #[test]
    fn drain_retires_remaining_work_in_queue_order() {
        let mut outbox = Outbox::<u32, 2>::new(2);
        assert_eq!(outbox.replace(0, 10), Ok(None));
        assert_eq!(outbox.push(20), Ok(()));
        assert_eq!(outbox.replace(1, 30), Ok(None));
        assert_eq!(outbox.push(40), Ok(()));
        let retired = outbox.drain().collect::<Vec<_>>();
        assert_eq!(retired, [10, 20, 30, 40]);
        assert!(outbox.is_empty());
        assert_eq!(outbox.len(), 0);
        assert_eq!(outbox.reliable_len(), 0);
    }

    #[test]
    fn retained_entries_never_exceed_reliable_capacity_plus_slots() {
        let mut outbox = Outbox::<u32, 3>::new(4);
        for value in 0..40 {
            let _ = outbox.push(value);
            for slot in 0..3 {
                let _ = outbox.replace(slot, value + 100);
            }
            assert!(outbox.len() <= 4 + 3);
            assert!(outbox.reliable_len() <= 4);
        }
        assert_eq!(outbox.len(), 7);
        assert_eq!(outbox.reliable_len(), 4);
        let mut receiver = Receiver {
            capacity: 7,
            delivered: Vec::new(),
        };
        assert_eq!(outbox.flush_with(|item| receiver.send(item)), Flush::Empty);
        assert_eq!(receiver.delivered.len(), 7);
    }

    #[test]
    fn flushing_an_empty_outbox_never_invokes_the_sender() {
        let mut outbox = Outbox::<u32, 2>::new(4);
        assert!(outbox.is_empty());
        assert_eq!(
            outbox.flush_with(|_| panic!("the sender must not run on an empty outbox")),
            Flush::Empty
        );
    }

    #[test]
    fn admission_stays_exhausted_while_permits_live_anywhere() {
        let admission = Admission::new(2);
        let pending = admission.try_acquire().expect("the accepted record");
        let running = admission.try_acquire().expect("the running task");
        // Pending records, running tasks and completed-but-unreaped outputs
        // all hold their permit: the pool stays exhausted.
        assert!(admission.try_acquire().is_none());
        assert_eq!(
            admission.counts(),
            Counts {
                limit: 2,
                active: 2,
                finished: 0,
                abandoned: 0
            }
        );
        // Reaping one outcome opens exactly one slot.
        running.finish();
        assert_eq!(
            admission.counts(),
            Counts {
                limit: 2,
                active: 1,
                finished: 1,
                abandoned: 0
            }
        );
        let replacement = admission.try_acquire().expect("one slot reopened");
        assert!(admission.try_acquire().is_none());
        // An unfinished drop is abandonment, never a finished outcome.
        drop(pending);
        assert_eq!(
            admission.counts(),
            Counts {
                limit: 2,
                active: 1,
                finished: 1,
                abandoned: 1
            }
        );
        replacement.finish();
        assert_eq!(
            admission.counts(),
            Counts {
                limit: 2,
                active: 0,
                finished: 2,
                abandoned: 1
            }
        );
    }

    #[test]
    fn permits_travel_through_tasks_and_finish_at_the_owner() {
        let admission = Admission::new(1);
        let permit = admission.try_acquire().expect("accepted work");
        // The permit moves into the response task and back out through the
        // reaped output, where the actor records the terminal outcome.
        let reap = std::thread::spawn(move || (permit, 42u32));
        let (permit, result) = reap.join().expect("the task did not panic");
        assert_eq!(result, 42);
        assert_eq!(
            admission.counts(),
            Counts {
                limit: 1,
                active: 1,
                finished: 0,
                abandoned: 0
            }
        );
        permit.finish();
        assert_eq!(
            admission.counts(),
            Counts {
                limit: 1,
                active: 0,
                finished: 1,
                abandoned: 0
            }
        );
    }

    #[test]
    fn panics_and_unfinished_drops_count_as_abandoned_never_finished() {
        let admission = Admission::new(2);
        let held = admission.try_acquire().expect("held permit");
        let panicking = admission.try_acquire().expect("task permit");
        // A response task panics: its permit is dropped during unwind.
        let task = std::thread::spawn(move || {
            drop(panicking);
            panic!("the response task aborted");
        });
        assert!(task.join().is_err());
        assert_eq!(
            admission.counts(),
            Counts {
                limit: 2,
                active: 1,
                finished: 0,
                abandoned: 1
            }
        );
        // The panic did not claim a success.
        held.finish();
        assert_eq!(
            admission.counts(),
            Counts {
                limit: 2,
                active: 0,
                finished: 1,
                abandoned: 1
            }
        );
    }

    #[test]
    fn a_poisoned_pool_recovers_like_message_once() {
        let admission = Admission::new(4);
        let poisoned = admission.clone();
        let holder = std::thread::spawn(move || {
            let _guard = poisoned
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            panic!("poison the pool lock");
        });
        assert!(holder.join().is_err());
        // Acquisition and counts keep working through the poisoned lock.
        let permit = admission.try_acquire().expect("recovers from poisoning");
        assert_eq!(
            admission.counts(),
            Counts {
                limit: 4,
                active: 1,
                finished: 0,
                abandoned: 0
            }
        );
        permit.finish();
        assert_eq!(
            admission.counts(),
            Counts {
                limit: 4,
                active: 0,
                finished: 1,
                abandoned: 0
            }
        );
    }

    #[test]
    fn finished_and_abandoned_counters_saturate_instead_of_wrapping() {
        let admission = Admission::new(2);
        {
            let mut state = admission
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.finished = u64::MAX;
        }
        let permit = admission.try_acquire().expect("capacity remains");
        permit.finish();
        assert_eq!(admission.counts().finished, u64::MAX);
        // The abandoned counter saturates the same way.
        {
            let mut state = admission
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.abandoned = u64::MAX;
        }
        let permit = admission.try_acquire().expect("capacity remains");
        drop(permit);
        assert_eq!(
            admission.counts(),
            Counts {
                limit: 2,
                active: 0,
                finished: u64::MAX,
                abandoned: u64::MAX
            }
        );
    }
}

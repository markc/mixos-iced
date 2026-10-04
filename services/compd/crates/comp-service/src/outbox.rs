// `offer_next` folds the event-sequence counter and the watermark store into
// the producer, so the engine's observation code needs no Bus state of its
// own.

//! The bounded observation lane between the engine (producer) and the
//! publisher task (consumer). One offer is fixed-cost and allocation-free:
//! a full lane evicts its oldest record and the loss rides, as an interval,
//! on the next survivor, so the publisher can publish a gap before it.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use crossbeam_channel::{Receiver, Sender, TryRecvError, TrySendError};
use tokio::sync::Notify;

use comp_model::observation::{EventSeq, LossCause, LossInterval, ObservationRecord};

/// The lane's capacity.
pub const OUTBOX_CAPACITY: usize = 256;

pub struct ObservationOutbox {
    pub records: Receiver<OutboxRecord>,
    pub capacity: usize,
}

pub struct OutboxRecord {
    pub record: ObservationRecord,
    pub preceding_loss: Option<LossInterval>,
}

/// The engine's end of the lane.
pub struct ObservationProducer {
    record_sender: Sender<OutboxRecord>,
    record_eviction: Receiver<OutboxRecord>,
    pending_loss: Option<LossInterval>,
    lost_count: Arc<AtomicU64>,
    notifier: Arc<Notify>,
    event_seq: EventSeq,
    event_seq_watermark: Arc<AtomicU64>,
}

impl ObservationProducer {
    /// Fixed-cost boundary: bounded channels allocate their storage at
    /// construction. One offer performs at most two sends and one eviction;
    /// it never grows a collection, serialises JSON, waits, polls, locks or
    /// loops over the queue.
    pub fn offer(&mut self, record: ObservationRecord) {
        let record = OutboxRecord {
            record,
            preceding_loss: self.pending_loss.take(),
        };
        match self.record_sender.try_send(record) {
            Ok(()) => self.notifier.notify_one(),
            Err(TrySendError::Disconnected(record)) => {
                self.fold_lost_record(record, LossCause::PublisherLoss);
            }
            Err(TrySendError::Full(record)) => {
                self.replace_one_oldest(record);
            }
        }
    }

    /// Number the next record with the shared event sequence (published as
    /// `port.event_seq`) and offer it. `None` once the sequence is
    /// exhausted: a sequence never repeats.
    pub fn offer_next(&mut self, build: impl FnOnce(u64) -> ObservationRecord) -> Option<u64> {
        let sequence = self.event_seq.next_seq()?;
        self.event_seq_watermark.store(sequence, Ordering::Release);
        self.offer(build(sequence));
        Some(sequence)
    }

    /// The last sequence handed out.
    pub fn event_seq(&self) -> u64 {
        self.event_seq.current()
    }

    fn replace_one_oldest(&mut self, mut record: OutboxRecord) {
        if let Some(loss) = record.preceding_loss.take() {
            self.merge_pending_loss(loss);
        }
        match self.record_eviction.try_recv() {
            Ok(evicted) => {
                self.fold_lost_record(evicted, LossCause::OutboxOverflow);
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.fold_lost_record(record, LossCause::PublisherLoss);
                return;
            }
        }

        record.preceding_loss = self.pending_loss.take();
        match self.record_sender.try_send(record) {
            Ok(()) => self.notifier.notify_one(),
            Err(TrySendError::Disconnected(record)) => {
                self.fold_lost_record(record, LossCause::PublisherLoss);
            }
            Err(TrySendError::Full(record)) => {
                debug_assert!(false, "one consumer or one eviction leaves one outbox slot");
                self.fold_lost_record(record, LossCause::OutboxOverflow);
            }
        }
    }

    fn fold_lost_record(&mut self, record: OutboxRecord, cause: LossCause) {
        if let Some(loss) = record.preceding_loss {
            self.merge_pending_loss(loss);
        }
        self.merge_pending_loss(LossInterval::from_record(&record.record, cause));
        self.lost_count.fetch_add(1, Ordering::AcqRel);
    }

    fn merge_pending_loss(&mut self, loss: LossInterval) {
        if let Some(pending) = self.pending_loss.as_mut() {
            pending.merge(loss);
        } else {
            self.pending_loss = Some(loss);
        }
    }

    pub fn notifier(&self) -> Arc<Notify> {
        Arc::clone(&self.notifier)
    }
}

/// A lane of [`OUTBOX_CAPACITY`]. `event_seq` is the shared watermark
/// (`port.event_seq`); `lost_count` counts every record that never reached
/// the Bus.
pub fn outbox(
    lost_count: Arc<AtomicU64>,
    event_seq: Arc<AtomicU64>,
) -> (ObservationProducer, ObservationOutbox) {
    outbox_with_capacity(lost_count, event_seq, OUTBOX_CAPACITY)
}

pub fn outbox_with_capacity(
    lost_count: Arc<AtomicU64>,
    event_seq: Arc<AtomicU64>,
    capacity: usize,
) -> (ObservationProducer, ObservationOutbox) {
    assert!(capacity > 0, "observation data lane must have capacity");
    let (record_sender, records) = crossbeam_channel::bounded(capacity);
    let notifier = Arc::new(Notify::new());
    (
        ObservationProducer {
            record_sender,
            record_eviction: records.clone(),
            pending_loss: None,
            lost_count,
            notifier,
            event_seq: EventSeq::starting_after(event_seq.load(Ordering::Acquire)),
            event_seq_watermark: event_seq,
        },
        ObservationOutbox { records, capacity },
    )
}

/// A lane of `capacity` with fresh counters (tests).
pub fn test_outbox(lost_count: Arc<AtomicU64>, capacity: usize) -> (ObservationProducer, ObservationOutbox) {
    outbox_with_capacity(lost_count, Arc::new(AtomicU64::new(0)), capacity)
}

#[cfg(test)]
mod tests {
    use std::{
        alloc::{GlobalAlloc, Layout, System},
        cell::Cell,
    };

    use comp_model::observation::{FOCUS_TOPIC_SUFFIX, PROPS_TOPIC_SUFFIX, PropValue};

    use super::*;

    thread_local! {
        static TRACKED_ALLOCATIONS: Cell<Option<usize>> = const { Cell::new(None) };
    }

    struct TestAllocator;

    #[global_allocator]
    static TEST_ALLOCATOR: TestAllocator = TestAllocator;

    unsafe impl GlobalAlloc for TestAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let _ = TRACKED_ALLOCATIONS.try_with(|count| {
                if let Some(current) = count.get() {
                    count.set(Some(current.saturating_add(1)));
                }
            });
            // SAFETY: this wrapper preserves System's allocation contract.
            unsafe { System.alloc(layout) }
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            let _ = TRACKED_ALLOCATIONS.try_with(|count| {
                if let Some(current) = count.get() {
                    count.set(Some(current.saturating_add(1)));
                }
            });
            // SAFETY: this wrapper preserves System's allocation contract.
            unsafe { System.alloc_zeroed(layout) }
        }

        unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
            // SAFETY: pointer/layout came from this System-backed allocator.
            unsafe { System.dealloc(pointer, layout) }
        }

        unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
            let _ = TRACKED_ALLOCATIONS.try_with(|count| {
                if let Some(current) = count.get() {
                    count.set(Some(current.saturating_add(1)));
                }
            });
            // SAFETY: pointer/layout came from System and size is forwarded.
            unsafe { System.realloc(pointer, layout, size) }
        }
    }

    fn allocations_during(run: impl FnOnce()) -> usize {
        TRACKED_ALLOCATIONS.with(|count| {
            assert_eq!(count.replace(Some(0)), None);
        });
        run();
        TRACKED_ALLOCATIONS.with(|count| count.replace(None).expect("tracking was armed"))
    }

    #[test]
    fn bounded_single_lane_carries_every_evicted_record_and_counts_it_once() {
        let lost = Arc::new(AtomicU64::new(0));
        let (mut producer, outbox) = test_outbox(Arc::clone(&lost), 2);
        assert_eq!(outbox.records.capacity(), Some(2));
        for sequence in 1..=6 {
            if sequence % 2 == 0 {
                producer.offer(ObservationRecord::FocusChanged {
                    keyboard: Some(sequence),
                    previous: None,
                    exclusive_latch: None,
                    event_seq: sequence,
                });
            } else {
                producer.offer(ObservationRecord::PropsChanged {
                    path: "input.corners.enabled".into(),
                    old: PropValue::Bool(true),
                    new: PropValue::Bool(false),
                    unix_ms: 0,
                    cause: "props.set",
                    event_seq: sequence,
                });
            }
        }
        assert_eq!(lost.load(Ordering::Acquire), 4);
        let first = outbox.records.recv().expect("first survivor");
        let second = outbox.records.recv().expect("second survivor");
        assert_eq!(first.record.event_seq(), 5);
        assert_eq!(second.record.event_seq(), 6);
        let first_loss = first
            .preceding_loss
            .expect("loss rides with first survivor");
        let second_loss = second
            .preceding_loss
            .expect("loss rides with second survivor");
        assert_eq!(
            (first_loss.first_lost_seq, first_loss.last_lost_seq),
            (1, 3)
        );
        assert_eq!(
            (second_loss.first_lost_seq, second_loss.last_lost_seq),
            (2, 4)
        );
        assert_eq!(
            first_loss.topics.iter().collect::<Vec<_>>(),
            [PROPS_TOPIC_SUFFIX]
        );
        assert_eq!(
            second_loss.topics.iter().collect::<Vec<_>>(),
            [FOCUS_TOPIC_SUFFIX]
        );
        assert_eq!(first_loss.cause, LossCause::OutboxOverflow);
        assert_eq!(second_loss.cause, LossCause::OutboxOverflow);
    }

    #[test]
    fn carried_loss_is_folded_when_its_survivor_is_later_evicted() {
        let lost = Arc::new(AtomicU64::new(0));
        let (mut producer, outbox) = test_outbox(Arc::clone(&lost), 1);
        for event_seq in 1..=4 {
            producer.offer(ObservationRecord::FocusChanged {
                keyboard: Some(event_seq),
                previous: None,
                exclusive_latch: None,
                event_seq,
            });
        }
        let survivor = outbox.records.recv().expect("newest record survives");
        assert_eq!(survivor.record.event_seq(), 4);
        let loss = survivor
            .preceding_loss
            .expect("the whole carried chain rides with the survivor");
        assert_eq!((loss.first_lost_seq, loss.last_lost_seq), (1, 3));
        assert_eq!(loss.topics.iter().collect::<Vec<_>>(), [FOCUS_TOPIC_SUFFIX]);
        assert_eq!(loss.cause, LossCause::OutboxOverflow);
        assert_eq!(lost.load(Ordering::Acquire), 3);
    }

    #[test]
    fn successful_overflow_and_carried_loss_offer_paths_are_allocation_free() {
        let lost = Arc::new(AtomicU64::new(0));
        let (mut producer, outbox) = test_outbox(Arc::clone(&lost), 1);
        let allocations = allocations_during(|| {
            for event_seq in 1..=1_024 {
                producer.offer(ObservationRecord::FocusChanged {
                    keyboard: Some(event_seq),
                    previous: None,
                    exclusive_latch: None,
                    event_seq,
                });
            }
        });

        assert_eq!(allocations, 0, "offer must remain allocation-free");
        assert_eq!(lost.load(Ordering::Acquire), 1_023);
        let survivor = outbox.records.recv().expect("one bounded-lane survivor");
        assert_eq!(survivor.record.event_seq(), 1_024);
        let loss = survivor.preceding_loss.expect("carried loss is retained");
        assert_eq!((loss.first_lost_seq, loss.last_lost_seq), (1, 1_023));
    }

    // New for compd: the shared sequence and its watermark.
    #[test]
    fn offer_next_numbers_records_and_moves_the_watermark() {
        let lost = Arc::new(AtomicU64::new(0));
        let watermark = Arc::new(AtomicU64::new(u64::MAX - 1));
        let (mut producer, outbox) = outbox_with_capacity(lost, Arc::clone(&watermark), 4);
        let focus = |event_seq| ObservationRecord::FocusChanged {
            keyboard: None,
            previous: None,
            exclusive_latch: None,
            event_seq,
        };
        assert_eq!(producer.offer_next(focus), Some(u64::MAX));
        assert_eq!(producer.offer_next(focus), None, "exhausted: never repeats");
        assert_eq!(watermark.load(Ordering::Acquire), u64::MAX);
        assert_eq!(producer.event_seq(), u64::MAX);
        assert_eq!(outbox.records.try_iter().map(|record| record.record.event_seq()).collect::<Vec<_>>(), [u64::MAX]);
    }
}

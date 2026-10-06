// SPDX-License-Identifier: MIT OR Apache-2.0

//! Opt-in bounded incoming lane for supervised subscription consumers.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::mpsc;

use crate::native_client::IncomingCommand;

/// An observation from an opt-in bounded supervised incoming lane.
#[derive(Debug)]
pub enum BoundedIncomingEvent {
    /// One broker command was retained by the bounded lane.
    Command(IncomingCommand),
    /// One or more commands were dropped because the consumer was behind.
    ///
    /// This marker is emitted once for the accumulated loss observed since
    /// the previous marker. Consumers should conservatively invalidate any
    /// state derived from subscription delivery before processing more data.
    Overflow { dropped: u64 },
}

#[derive(Default)]
struct OverflowState {
    total: AtomicU64,
    pending: std::sync::Mutex<u64>,
    wake: tokio::sync::Notify,
}

impl OverflowState {
    fn record(&self, count: u64) {
        saturating_add(&self.total, count);
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *pending = pending.saturating_add(count);
        drop(pending);
        self.wake.notify_one();
    }

    fn take_pending(&self) -> u64 {
        std::mem::take(
            &mut *self
                .pending
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
        )
    }
}

fn saturating_add(value: &AtomicU64, increment: u64) {
    let _ = value.fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
        Some(current.saturating_add(increment))
    });
}

/// Receiver for an opt-in bounded supervised incoming lane.
///
/// The producer never waits for capacity. When full, it drops the new command,
/// increments [`overflow_count`](Self::overflow_count), and makes one
/// [`BoundedIncomingEvent::Overflow`] observation available before the next
/// retained command is returned.
pub struct BoundedIncomingReceiver {
    receiver: mpsc::Receiver<IncomingCommand>,
    overflow: Arc<OverflowState>,
    deferred: Option<IncomingCommand>,
}

impl BoundedIncomingReceiver {
    /// Receive the next retained command or overflow marker.
    pub async fn recv(&mut self) -> Option<BoundedIncomingEvent> {
        let overflow = Arc::clone(&self.overflow);
        loop {
            let notified = overflow.wake.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let dropped = self.overflow.take_pending();
            if dropped != 0 {
                return Some(BoundedIncomingEvent::Overflow { dropped });
            }
            if let Some(command) = self.deferred.take() {
                return self.deliver(Some(command));
            }
            tokio::select! {
                biased;
                _ = &mut notified => continue,
                command = self.receiver.recv() => return self.deliver(command),
            }
        }
    }

    fn deliver(&mut self, command: Option<IncomingCommand>) -> Option<BoundedIncomingEvent> {
        // Linearise delivery against recording loss, independently of the
        // notification. Keep an already-dequeued command until invalidation.
        let mut pending = self
            .overflow
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let dropped = std::mem::take(&mut *pending);
        if dropped != 0 {
            self.deferred = command;
            Some(BoundedIncomingEvent::Overflow { dropped })
        } else {
            command.map(BoundedIncomingEvent::Command)
        }
    }

    /// Total number of commands dropped by this lane.
    pub fn overflow_count(&self) -> u64 {
        self.overflow.total.load(Ordering::Acquire)
    }
}

#[derive(Clone)]
pub(crate) struct BoundedIncomingSender {
    sender: mpsc::Sender<IncomingCommand>,
    overflow: Arc<OverflowState>,
}

impl BoundedIncomingSender {
    pub(crate) async fn closed(&self) {
        self.sender.closed().await;
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.sender.is_closed()
    }

    /// Returns `false` only when the consumer has gone away.
    pub(crate) fn try_send(&self, command: IncomingCommand) -> bool {
        match self.sender.try_send(command) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.record_overflow(1);
                true
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }

    pub(crate) fn record_overflow(&self, dropped: u64) {
        self.overflow.record(dropped);
    }
}

pub(crate) fn bounded_incoming_channel(
    capacity: usize,
) -> (BoundedIncomingSender, BoundedIncomingReceiver) {
    let (sender, receiver) = mpsc::channel(capacity);
    let overflow = Arc::new(OverflowState::default());
    (
        BoundedIncomingSender {
            sender,
            overflow: Arc::clone(&overflow),
        },
        BoundedIncomingReceiver {
            receiver,
            overflow,
            deferred: None,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::collections::BTreeMap;

    fn command(sequence: u64) -> IncomingCommand {
        IncomingCommand {
            generation: 0,
            from: "publisher".to_owned(),
            command: "topic.event".to_owned(),
            id: None,
            args: Value::Null,
            body: sequence.to_string(),
            headers: BTreeMap::new(),
        }
    }

    #[tokio::test]
    async fn loss_recorded_before_notification_precedes_a_dequeued_command() {
        let (sender, mut receiver) = bounded_incoming_channel(1);
        assert!(sender.try_send(command(1)));
        let retained = receiver.receiver.recv().await.unwrap();
        // Producer is paused between updating the counter and notifying.
        *sender.overflow.pending.lock().unwrap() = 1;
        assert!(matches!(
            receiver.deliver(Some(retained)),
            Some(BoundedIncomingEvent::Overflow { dropped: 1 })
        ));
        assert!(
            matches!(receiver.recv().await, Some(BoundedIncomingEvent::Command(command)) if command.body == "1")
        );
    }

    #[tokio::test]
    async fn delayed_overflow_record_wakes_an_already_waiting_empty_receiver() {
        let (sender, mut receiver) = bounded_incoming_channel(1);
        let mut waiting = Box::pin(receiver.recv());
        assert!(futures_util::poll!(waiting.as_mut()).is_pending());
        // Models a producer pre-empted after observing Full while the consumer
        // drains the queue before the producer records that drop.
        sender.record_overflow(1);
        assert!(matches!(
            tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
                .await
                .unwrap(),
            Some(BoundedIncomingEvent::Overflow { dropped: 1 })
        ));
    }

    #[tokio::test]
    async fn bounded_lane_drops_without_waiting_and_emits_one_overflow_marker() {
        let (sender, mut receiver) = bounded_incoming_channel(2);
        assert!(sender.try_send(command(1)));
        assert!(sender.try_send(command(2)));
        assert!(sender.try_send(command(3)));
        assert!(sender.try_send(command(4)));

        assert_eq!(receiver.overflow_count(), 2);
        assert!(matches!(
            receiver.recv().await,
            Some(BoundedIncomingEvent::Overflow { dropped: 2 })
        ));
        assert!(matches!(
            receiver.recv().await,
            Some(BoundedIncomingEvent::Command(command)) if command.body == "1"
        ));
        assert!(matches!(
            receiver.recv().await,
            Some(BoundedIncomingEvent::Command(command)) if command.body == "2"
        ));
    }

    #[tokio::test]
    async fn later_overflow_produces_a_new_marker_and_preserves_the_total() {
        let (sender, mut receiver) = bounded_incoming_channel(1);
        assert!(sender.try_send(command(1)));
        assert!(sender.try_send(command(2)));
        assert!(matches!(
            receiver.recv().await,
            Some(BoundedIncomingEvent::Overflow { dropped: 1 })
        ));
        assert!(matches!(
            receiver.recv().await,
            Some(BoundedIncomingEvent::Command(_))
        ));

        assert!(sender.try_send(command(3)));
        assert!(sender.try_send(command(4)));
        assert!(matches!(
            receiver.recv().await,
            Some(BoundedIncomingEvent::Overflow { dropped: 1 })
        ));
        assert_eq!(receiver.overflow_count(), 2);
    }
}

// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fixed-capacity settings delivery behind one host wake notification.
use super::{Event, ResourceOutcome};
use std::sync::{Arc, Mutex};

pub struct Mailbox<T>(Arc<Mutex<Pending<T>>>);
struct Pending<T> {
    notified: bool,
    lost: bool,
    events: Vec<Event<T>>,
}
impl<T> Default for Mailbox<T> {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(Pending {
            notified: false,
            lost: false,
            events: Vec::new(),
        })))
    }
}
impl<T> Clone for Mailbox<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}
impl<T> PartialEq for Mailbox<T> {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl<T> Eq for Mailbox<T> {}
impl<T> std::fmt::Debug for Mailbox<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SettingsMailbox { .. }")
    }
}
fn slot<T>(event: &Event<T>) -> u8 {
    match event {
        Event::Wake | Event::Refresh | Event::Lost => 0,
        Event::Delivery(_) => 1,
        Event::Rpc(..) => 2,
        Event::Prepared(_) => 3,
        Event::Fallback(..) => 4,
        Event::Resource(completion) => match &completion.outcome {
            ResourceOutcome::Fallback(..) => 4,
            ResourceOutcome::Prepared(_) | ResourceOutcome::Reprepared(..) => 3,
        },
        #[cfg(feature = "settings-cache")]
        Event::Saved(..) => 5,
        #[cfg(feature = "settings-cache")]
        Event::RetryCache => 6,
    }
}
impl<T> Mailbox<T> {
    /// True exactly when the host must enqueue/wake its UI. At most one event
    /// per slot and one notification exist. Superseded deliveries require a read.
    pub fn publish(&self, event: Event<T>) -> bool {
        let mut pending = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        pending.lost |= matches!(&event, Event::Lost);
        let key = slot(&event);
        if let Some(index) = pending.events.iter().position(|old| slot(old) == key) {
            if let (Event::Delivery(previous), Event::Delivery(next)) =
                (&pending.events[index], &event)
                && previous.same_message(next)
            {
                return false;
            }
            if matches!(&event, Event::Wake) && matches!(&pending.events[index], Event::Refresh) {
                return false;
            }
            pending.events.remove(index);
            pending.lost |= key == 1;
        }
        pending.events.push(event);
        let notify = !pending.notified;
        pending.notified = true;
        notify
    }
    /// Consume on the UI loop. Cloned messages draining the same handle are
    /// harmless. Release the lock before event handling or callbacks.
    pub fn take(&self) -> Vec<Event<T>> {
        let mut pending = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        pending.notified = false;
        let mut events = Vec::with_capacity(8);
        if std::mem::take(&mut pending.lost) {
            events.push(Event::Lost);
        }
        events.append(&mut pending.events);
        events
    }
}

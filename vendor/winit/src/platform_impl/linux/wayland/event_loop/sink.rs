//! An event loop's sink to deliver events from the Wayland event callbacks.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use crate::event::{DeviceEvent, DeviceId as RootDeviceId, Event, WindowEvent};
use crate::platform_impl::platform::DeviceId as PlatformDeviceId;
use crate::window::WindowId as RootWindowId;

use super::{DeviceId, WindowId};

/// An event loop's sink to deliver events from the Wayland event callbacks
/// to the winit's user.
#[derive(Default)]
pub struct EventSink {
    window_events: Vec<QueuedEvent>,
}

struct QueuedEvent {
    event: Event<()>,
    epoch: Option<(Arc<AtomicU64>, u64)>,
}

impl EventSink {
    pub fn new() -> Self {
        Default::default()
    }

    /// Return `true` if there're pending events.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.window_events.is_empty()
    }

    /// Add new device event to a queue.
    #[inline]
    pub fn push_device_event(&mut self, event: DeviceEvent, device_id: DeviceId) {
        self.window_events.push(QueuedEvent {
            event: Event::DeviceEvent {
                event,
                device_id: RootDeviceId(PlatformDeviceId::Wayland(device_id)),
            },
            epoch: None,
        });
    }

    /// Add new window event to a queue.
    #[inline]
    pub fn push_window_event(&mut self, event: WindowEvent, window_id: WindowId) {
        self.window_events.push(QueuedEvent {
            event: Event::WindowEvent { event, window_id: RootWindowId(window_id) },
            epoch: None,
        });
    }

    pub fn push_ime_event(
        &mut self,
        event: WindowEvent,
        window_id: WindowId,
        epoch: Arc<AtomicU64>,
        expected: u64,
    ) {
        self.window_events.push(QueuedEvent {
            event: Event::WindowEvent { event, window_id: RootWindowId(window_id) },
            epoch: Some((epoch, expected)),
        });
    }

    #[inline]
    pub fn append(&mut self, other: &mut Self) {
        self.window_events.append(&mut other.window_events);
    }

    #[inline]
    pub fn drain(&mut self) -> impl Iterator<Item = Event<()>> + '_ {
        self.window_events.drain(..).filter_map(|queued| {
            let valid = queued
                .epoch
                .as_ref()
                .is_none_or(|(epoch, expected)| epoch.load(Ordering::Acquire) == *expected);
            valid.then_some(queued.event)
        })
    }
}

#[cfg(test)]
mod epoch_delivery_tests {
    use super::*;
    use crate::event::Ime;

    #[test]
    fn retiring_an_epoch_discards_already_queued_ime_after_disabled() {
        let epoch = Arc::new(AtomicU64::new(1));
        let mut sink = EventSink::new();
        sink.push_ime_event(
            WindowEvent::Ime(Ime::Commit("old".into())),
            WindowId(1),
            epoch.clone(),
            1,
        );
        // A threaded setter has queued synthetic Disabled before this drain.
        epoch.store(2, Ordering::Release);
        assert_eq!(sink.drain().count(), 0);
    }

    #[test]
    fn epoch_is_rechecked_between_events_and_survives_sink_append() {
        let epoch = Arc::new(AtomicU64::new(1));
        let mut sink = EventSink::new();
        let mut other = EventSink::new();
        for text in ["first", "second"] {
            other.push_ime_event(
                WindowEvent::Ime(Ime::Commit(text.into())),
                WindowId(1),
                epoch.clone(),
                1,
            );
        }
        sink.append(&mut other);
        let mut events = sink.drain();
        assert!(events.next().is_some());
        epoch.store(2, Ordering::Release);
        assert!(events.next().is_none());
    }
}

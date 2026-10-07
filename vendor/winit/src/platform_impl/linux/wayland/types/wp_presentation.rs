// SPDX-License-Identifier: Apache-2.0
//! Same-queue presentation feedback with bounded native object/receipt lifetime.

use crate::event::WindowEvent;
use crate::platform_impl::wayland::{WindowId, state::WinitState};
use crate::presentation::{
    self, PresentationError, PresentationFeedback, PresentationId, PresentationOutcome,
};
use sctk::globals::GlobalData;
use sctk::reexports::client::globals::{BindError, GlobalList};
use sctk::reexports::client::protocol::wl_surface::WlSurface;
use sctk::reexports::client::{Connection, Dispatch, Proxy, QueueHandle, WEnum, delegate_dispatch};
use sctk::reexports::protocols::wp::presentation_time::client::{
    wp_presentation, wp_presentation_feedback,
};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};

const WINDOW_CAP: usize = 8;
const PROCESS_CAP: usize = 128;
static PROCESS: OnceLock<Arc<ProcessBudget>> = OnceLock::new();

#[derive(Default)]
struct ProcessBudget {
    count: AtomicUsize,
    epoch: AtomicU64,
    wakes: Mutex<Arc<[Weak<CapacityWake>]>>,
}

/// Owned only by its event loop. Native charges and the process registry hold
/// weak references, so retirement cannot keep a destroyed loop alive.
pub(crate) struct CapacityWake {
    ping: sctk::reexports::calloop::ping::Ping,
    pending: AtomicBool,
    #[cfg(test)]
    acknowledgements: AtomicUsize,
}

impl CapacityWake {
    pub(crate) fn acknowledge(&self) {
        let pending = self.pending.swap(false, Ordering::AcqRel);
        #[cfg(test)]
        if pending {
            self.acknowledgements.fetch_add(1, Ordering::Relaxed);
        }
        #[cfg(not(test))]
        let _ = pending;
    }

    #[cfg(test)]
    pub(crate) fn native_acknowledgements(&self) -> usize {
        self.acknowledgements.load(Ordering::Relaxed)
    }

    fn notify(&self) {
        if !self.pending.swap(true, Ordering::AcqRel) {
            self.ping.ping();
        }
    }
}

impl ProcessBudget {
    fn subscribe(&self, ping: sctk::reexports::calloop::ping::Ping) -> Arc<CapacityWake> {
        let wake = Arc::new(CapacityWake {
            ping,
            pending: AtomicBool::new(false),
            #[cfg(test)]
            acknowledgements: AtomicUsize::new(0),
        });
        let mut registry = self.wakes.lock().unwrap_or_else(PoisonError::into_inner);
        let mut live: Vec<_> =
            registry.iter().filter(|entry| entry.strong_count() != 0).cloned().collect();
        live.push(Arc::downgrade(&wake));
        *registry = live.into();
        wake
    }

    fn release(&self) {
        self.count.fetch_sub(1, Ordering::AcqRel);
        let _ = self.epoch.fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
            Some(value.saturating_add(1))
        });
        // Drop allocates nothing and holds no registry/window/event-sink lock
        // while pinging. Dead weak entries cannot resurrect their event loops.
        let wakes = self.wakes.lock().unwrap_or_else(PoisonError::into_inner).clone();
        for wake in wakes.iter().filter_map(Weak::upgrade) {
            wake.notify();
        }
    }

    fn capacity(&self, window: &WindowPresentation) -> presentation::PresentationCapacity {
        // Reading the epoch first ensures a concurrent release is either
        // visible as available now or creates a later generation and wake.
        let release_epoch = self.epoch.load(Ordering::Acquire);
        let available = !window.closed.load(Ordering::Acquire)
            && self.count.load(Ordering::Acquire) < PROCESS_CAP
            && window.outstanding.load(Ordering::Acquire) < WINDOW_CAP;
        presentation::PresentationCapacity { release_epoch, available }
    }
}

#[derive(Default)]
pub struct WindowPresentation {
    pub closed: AtomicBool,
    outstanding: AtomicUsize,
    #[cfg(test)]
    native_request: Mutex<Option<NativeRequest>>,
}

#[cfg(test)]
pub(crate) struct NativeRequest {
    id: sctk::reexports::client::backend::ObjectId,
    charge: std::sync::Weak<Charge>,
    seen: Arc<std::sync::atomic::AtomicU8>,
}

#[cfg(test)]
impl NativeRequest {
    pub(crate) fn charge_alive(&self) -> bool {
        self.charge.strong_count() != 0
    }
    pub(crate) fn discarded(&self) -> bool {
        self.seen.load(Ordering::Acquire) == 2
    }
    pub(crate) fn object_id(&self) -> sctk::reexports::client::backend::ObjectId {
        self.id.clone()
    }
}

#[cfg(test)]
pub(crate) fn native_process_count() -> usize {
    PROCESS.get().map_or(0, |budget| budget.count.load(Ordering::Acquire))
}

#[cfg(test)]
impl WindowPresentation {
    pub(crate) fn take_native_request(&self) -> Option<NativeRequest> {
        self.native_request.lock().unwrap().take()
    }
}

// Records only the real wire event. The original QueueProxyData receives the
// unchanged event and keeps ownership of typed dispatch and its native lease.
#[cfg(test)]
struct NativeTap {
    inner: Arc<dyn sctk::reexports::client::backend::ObjectData>,
    seen: Arc<std::sync::atomic::AtomicU8>,
}

#[cfg(test)]
impl sctk::reexports::client::backend::ObjectData for NativeTap {
    fn event(
        self: Arc<Self>,
        backend: &sctk::reexports::client::backend::Backend,
        msg: sctk::reexports::client::backend::protocol::Message<
            sctk::reexports::client::backend::ObjectId,
            std::os::fd::OwnedFd,
        >,
    ) -> Option<Arc<dyn sctk::reexports::client::backend::ObjectData>> {
        if let Some(event) = wp_presentation_feedback::WpPresentationFeedback::interface()
            .events
            .get(usize::from(msg.opcode))
        {
            match event.name {
                "presented" => self.seen.store(1, Ordering::Release),
                "discarded" => self.seen.store(2, Ordering::Release),
                _ => (),
            }
        }
        self.inner.clone().event(backend, msg)
    }
    fn destroyed(&self, id: sctk::reexports::client::backend::ObjectId) {
        self.inner.destroyed(id);
    }
    fn data_as_any(&self) -> &dyn std::any::Any {
        self.inner.data_as_any()
    }
}

struct Charge {
    process: Arc<ProcessBudget>,
    window: Arc<WindowPresentation>,
}

fn reserve(counter: &AtomicUsize, limit: usize) -> bool {
    counter
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
            (value < limit).then_some(value + 1)
        })
        .is_ok()
}

impl Charge {
    fn acquire(
        process: Arc<ProcessBudget>,
        window: Arc<WindowPresentation>,
    ) -> Result<Arc<Self>, PresentationError> {
        if window.closed.load(Ordering::Acquire) {
            return Err(PresentationError::Closed);
        }
        if !reserve(&process.count, PROCESS_CAP) {
            return Err(PresentationError::Capacity);
        }
        if !reserve(&window.outstanding, WINDOW_CAP) {
            process.release();
            return Err(PresentationError::Capacity);
        }
        Ok(Arc::new(Self { process, window }))
    }
}
impl Drop for Charge {
    fn drop(&mut self) {
        self.window.outstanding.fetch_sub(1, Ordering::AcqRel);
        self.process.release();
    }
}

#[derive(Clone)]
pub struct PresentationState {
    global: wp_presentation::WpPresentation,
    clock: Arc<Mutex<Option<u32>>>,
    process: Arc<ProcessBudget>,
}
pub struct FeedbackData {
    id: PresentationId,
    window_id: WindowId,
    clock: Arc<Mutex<Option<u32>>>,
    charge: Arc<Charge>,
    terminal: AtomicBool,
}

impl PresentationState {
    pub fn new(globals: &GlobalList, queue: &QueueHandle<WinitState>) -> Result<Self, BindError> {
        Ok(Self {
            global: globals.bind(queue, 1..=1, GlobalData)?,
            clock: Arc::new(Mutex::new(None)),
            process: PROCESS.get_or_init(|| Arc::new(ProcessBudget::default())).clone(),
        })
    }
    pub(crate) fn subscribe_capacity(
        &self,
        ping: sctk::reexports::calloop::ping::Ping,
    ) -> Arc<CapacityWake> {
        self.process.subscribe(ping)
    }

    pub fn capacity(
        &self,
        surface: &WlSurface,
        window: &WindowPresentation,
    ) -> Result<presentation::PresentationCapacity, PresentationError> {
        if !self.global.is_alive() || !surface.is_alive() || window.closed.load(Ordering::Acquire) {
            return Err(PresentationError::Closed);
        }
        Ok(self.process.capacity(window))
    }
    pub fn request(
        &self,
        surface: &WlSurface,
        queue: &QueueHandle<WinitState>,
        window_id: WindowId,
        window: Arc<WindowPresentation>,
    ) -> Result<PresentationId, PresentationError> {
        if !self.global.is_alive() || !surface.is_alive() {
            return Err(PresentationError::Closed);
        }
        let charge = Charge::acquire(self.process.clone(), window)?;
        let id = presentation::next_id()?;
        // Direct request on this surface's existing dispatch owner. The caller
        // performs its buffer commit immediately after this returns.
        let data = FeedbackData {
            id,
            window_id,
            clock: self.clock.clone(),
            charge,
            terminal: AtomicBool::new(false),
        };
        #[cfg(not(test))]
        let _feedback = self.global.feedback(surface, queue, data);
        #[cfg(test)]
        {
            // Match the generated constructor exactly, adding the tap at
            // construction rather than replacing data on a live backend.
            let weak_charge = Arc::downgrade(&data.charge);
            let window = data.charge.window.clone();
            let seen = Arc::new(std::sync::atomic::AtomicU8::new(0));
            let inner = queue
                .make_data::<wp_presentation_feedback::WpPresentationFeedback, FeedbackData>(data);
            let feedback: wp_presentation_feedback::WpPresentationFeedback = self
                .global
                .send_constructor(
                    wp_presentation::Request::Feedback { surface: surface.clone() },
                    Arc::new(NativeTap { inner, seen: seen.clone() }),
                )
                .expect("new native presentation feedback");
            let observation = NativeRequest { id: feedback.id(), charge: weak_charge, seen };
            *window.native_request.lock().unwrap() = Some(observation);
        }
        Ok(id)
    }
}

impl Dispatch<wp_presentation::WpPresentation, GlobalData, WinitState> for PresentationState {
    fn event(
        state: &mut WinitState,
        _: &wp_presentation::WpPresentation,
        event: wp_presentation::Event,
        _: &GlobalData,
        _: &Connection,
        _: &QueueHandle<WinitState>,
    ) {
        if let wp_presentation::Event::ClockId { clk_id } = event {
            if let Some(presentation) = &state.presentation {
                *presentation.clock.lock().unwrap() = Some(clk_id);
            }
        }
    }
}

impl Dispatch<wp_presentation_feedback::WpPresentationFeedback, FeedbackData, WinitState>
    for PresentationState
{
    fn event(
        state: &mut WinitState,
        _: &wp_presentation_feedback::WpPresentationFeedback,
        event: wp_presentation_feedback::Event,
        data: &FeedbackData,
        _: &Connection,
        _: &QueueHandle<WinitState>,
    ) {
        let outcome = match event {
            wp_presentation_feedback::Event::Presented {
                tv_sec_hi,
                tv_sec_lo,
                tv_nsec,
                refresh,
                seq_hi,
                seq_lo,
                flags,
            } => PresentationOutcome::Presented {
                clock_id: *data.clock.lock().unwrap(),
                seconds: (u64::from(tv_sec_hi) << 32) | u64::from(tv_sec_lo),
                nanoseconds: tv_nsec,
                refresh_ns: refresh,
                output_sequence: (u64::from(seq_hi) << 32) | u64::from(seq_lo),
                flags: match flags {
                    WEnum::Value(value) => value.bits(),
                    WEnum::Unknown(bits) => bits,
                },
            },
            wp_presentation_feedback::Event::Discarded => PresentationOutcome::Discarded,
            // SyncOutput retains no growing output list; unknown events are
            // neither terminal nor fabricated presentation observations.
            _ => return,
        };
        if data.terminal.swap(true, Ordering::AcqRel)
            || data.charge.window.closed.load(Ordering::Acquire)
        {
            return;
        }
        state.events_sink.push_window_event(
            WindowEvent::PresentationFeedback(PresentationFeedback::new(
                data.id,
                outcome,
                data.charge.clone(),
            )),
            data.window_id,
        );
    }
}

delegate_dispatch!(WinitState: [wp_presentation::WpPresentation: GlobalData] => PresentationState);
delegate_dispatch!(WinitState: [wp_presentation_feedback::WpPresentationFeedback: FeedbackData] => PresentationState);

#[cfg(test)]
mod tests {
    use super::*;

    fn listener(
        process: &ProcessBudget,
    ) -> (sctk::reexports::calloop::EventLoop<'static, usize>, Arc<CapacityWake>) {
        use sctk::reexports::calloop;
        let event_loop = calloop::EventLoop::try_new().unwrap();
        let (ping, source) = calloop::ping::make_ping().unwrap();
        let wake = process.subscribe(ping);
        let weak = Arc::downgrade(&wake);
        event_loop
            .handle()
            .insert_source(source, move |_, _, delivered: &mut usize| {
                if let Some(wake) = weak.upgrade() {
                    wake.acknowledge();
                }
                *delivered += 1;
            })
            .unwrap();
        (event_loop, wake)
    }

    #[test]
    fn closed_foreign_charges_wake_both_real_loops_once_without_retaining_them() {
        let process = Arc::new(ProcessBudget::default());
        let (mut first, first_wake) = listener(&process);
        let (mut second, second_wake) = listener(&process);
        let held: Vec<_> = (0..PROCESS_CAP)
            .map(|_| {
                let window = Arc::new(WindowPresentation::default());
                let charge = Charge::acquire(process.clone(), window.clone()).unwrap();
                window.closed.store(true, Ordering::Release);
                charge
            })
            .collect();
        let waiting = WindowPresentation::default();
        assert!(!process.capacity(&waiting).available);
        let clone = held[0].clone();
        drop(held);
        assert_eq!(process.count.load(Ordering::Acquire), 1);
        let capacity = process.capacity(&waiting);
        assert!(capacity.available);
        assert_eq!(capacity.release_epoch, (PROCESS_CAP - 1) as u64);
        let mut first_count = 0;
        let mut second_count = 0;
        first.dispatch(std::time::Duration::ZERO, &mut first_count).unwrap();
        second.dispatch(std::time::Duration::ZERO, &mut second_count).unwrap();
        assert_eq!((first_count, second_count), (1, 1));
        let retired = Arc::downgrade(&first_wake);
        drop(first_wake);
        drop(first);
        assert!(retired.upgrade().is_none(), "native charges cannot retain an event loop");
        drop(clone);
        second.dispatch(std::time::Duration::ZERO, &mut second_count).unwrap();
        assert_eq!(second_count, 2);
        assert_eq!(process.capacity(&waiting).release_epoch, PROCESS_CAP as u64);
        assert_eq!(process.count.load(Ordering::Acquire), 0);
        drop(second_wake);
        let (_, next_wake) = listener(&process);
        assert_eq!(process.wakes.lock().unwrap().len(), 1, "registration prunes dead weak loops");
        drop(next_wake);
    }

    #[test]
    fn foreign_release_after_ping_acknowledgement_is_not_lost() {
        use sctk::reexports::calloop;
        use std::sync::mpsc;
        use std::time::Duration;
        let process = Arc::new(ProcessBudget::default());
        let window = Arc::new(WindowPresentation::default());
        let first = Charge::acquire(process.clone(), window.clone()).unwrap();
        let second = Charge::acquire(process.clone(), window.clone()).unwrap();
        window.closed.store(true, Ordering::Release);
        let mut event_loop = calloop::EventLoop::try_new().unwrap();
        let (ping, source) = calloop::ping::make_ping().unwrap();
        let wake = process.subscribe(ping);
        let weak = Arc::downgrade(&wake);
        let (acknowledged, receive_ack) = mpsc::sync_channel(1);
        let (released, receive_release) = mpsc::sync_channel(1);
        let foreign = std::thread::spawn(move || {
            receive_ack.recv_timeout(Duration::from_secs(2)).unwrap();
            drop(second);
            released.send(()).unwrap();
        });
        event_loop
            .handle()
            .insert_source(source, move |_, _, delivered: &mut usize| {
                weak.upgrade().unwrap().acknowledge();
                if *delivered == 0 {
                    acknowledged.send(()).unwrap();
                    receive_release.recv_timeout(Duration::from_secs(2)).unwrap();
                }
                *delivered += 1;
            })
            .unwrap();
        drop(first);
        let mut count = 0;
        event_loop.dispatch(Duration::ZERO, &mut count).unwrap();
        assert_eq!(count, 1);
        foreign.join().unwrap();
        event_loop.dispatch(Duration::ZERO, &mut count).unwrap();
        assert_eq!(
            count, 2,
            "foreign release after acknowledgement must write a fresh actual ping"
        );
        assert_eq!(process.capacity(&WindowPresentation::default()).release_epoch, 2);
        assert_eq!(process.count.load(Ordering::Acquire), 0);
    }

    #[test]
    fn window_denial_releases_process_credit_but_does_not_claim_window_availability() {
        let process = Arc::new(ProcessBudget::default());
        let (mut event_loop, _wake) = listener(&process);
        let window = Arc::new(WindowPresentation::default());
        let mut held: Vec<_> = (0..WINDOW_CAP)
            .map(|_| Charge::acquire(process.clone(), window.clone()).unwrap())
            .collect();
        assert!(matches!(
            Charge::acquire(process.clone(), window.clone()),
            Err(PresentationError::Capacity)
        ));
        assert!(!process.capacity(&window).available);
        assert_eq!(process.capacity(&window).release_epoch, 1);
        let mut count = 0;
        event_loop.dispatch(std::time::Duration::ZERO, &mut count).unwrap();
        assert_eq!(count, 1);
        drop(held.pop().unwrap());
        assert!(process.capacity(&window).available);
        event_loop.dispatch(std::time::Duration::ZERO, &mut count).unwrap();
        assert_eq!(count, 2);
        window.closed.store(true, Ordering::Release);
        assert!(!process.capacity(&window).available);
    }

    #[test]
    fn release_epoch_saturates_without_reusing_an_earlier_generation() {
        let process = Arc::new(ProcessBudget::default());
        process.epoch.store(u64::MAX - 1, Ordering::Release);
        let window = Arc::new(WindowPresentation::default());
        drop(Charge::acquire(process.clone(), window.clone()).unwrap());
        assert_eq!(process.capacity(&window).release_epoch, u64::MAX);
        drop(Charge::acquire(process.clone(), window.clone()).unwrap());
        assert_eq!(process.capacity(&window).release_epoch, u64::MAX);
    }

    #[test]
    fn window_cap_rolls_back_process_and_close_does_not_release_unresolved_objects() {
        let process = Arc::new(ProcessBudget::default());
        let window = Arc::new(WindowPresentation::default());
        let held: Vec<_> = (0..WINDOW_CAP)
            .map(|_| Charge::acquire(process.clone(), window.clone()).unwrap())
            .collect();
        assert!(matches!(
            Charge::acquire(process.clone(), window.clone()),
            Err(PresentationError::Capacity)
        ));
        assert_eq!(process.count.load(Ordering::Acquire), WINDOW_CAP);
        window.closed.store(true, Ordering::Release);
        assert!(matches!(
            Charge::acquire(process.clone(), window.clone()),
            Err(PresentationError::Closed)
        ));
        assert_eq!(process.count.load(Ordering::Acquire), WINDOW_CAP);
        let receipt_lease = held[0].clone();
        drop(held);
        assert_eq!(process.count.load(Ordering::Acquire), 1);
        drop(receipt_lease);
        assert_eq!(process.count.load(Ordering::Acquire), 0);
        assert_eq!(window.outstanding.load(Ordering::Acquire), 0);
    }
    #[test]
    fn process_cap_survives_window_churn_and_retires_once() {
        let process = Arc::new(ProcessBudget::default());
        let held: Vec<_> = (0..PROCESS_CAP)
            .map(|_| {
                let window = Arc::new(WindowPresentation::default());
                let charge = Charge::acquire(process.clone(), window.clone()).unwrap();
                window.closed.store(true, Ordering::Release);
                charge
            })
            .collect();
        let next = Arc::new(WindowPresentation::default());
        assert!(matches!(
            Charge::acquire(process.clone(), next.clone()),
            Err(PresentationError::Capacity)
        ));
        assert_eq!(next.outstanding.load(Ordering::Acquire), 0);
        drop(held);
        let one = Charge::acquire(process.clone(), next).unwrap();
        let clone = one.clone();
        drop(one);
        assert_eq!(process.count.load(Ordering::Acquire), 1);
        drop(clone);
        assert_eq!(process.count.load(Ordering::Acquire), 0);
    }
}

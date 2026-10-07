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
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

const WINDOW_CAP: usize = 8;
const PROCESS_CAP: usize = 128;
static PROCESS: OnceLock<Arc<AtomicUsize>> = OnceLock::new();

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
    PROCESS.get().map_or(0, |counter| counter.load(Ordering::Acquire))
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
    process: Arc<AtomicUsize>,
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
        process: Arc<AtomicUsize>,
        window: Arc<WindowPresentation>,
    ) -> Result<Arc<Self>, PresentationError> {
        if window.closed.load(Ordering::Acquire) {
            return Err(PresentationError::Closed);
        }
        if !reserve(&process, PROCESS_CAP) {
            return Err(PresentationError::Capacity);
        }
        if !reserve(&window.outstanding, WINDOW_CAP) {
            process.fetch_sub(1, Ordering::AcqRel);
            return Err(PresentationError::Capacity);
        }
        Ok(Arc::new(Self { process, window }))
    }
}
impl Drop for Charge {
    fn drop(&mut self) {
        self.window.outstanding.fetch_sub(1, Ordering::AcqRel);
        self.process.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Clone)]
pub struct PresentationState {
    global: wp_presentation::WpPresentation,
    clock: Arc<Mutex<Option<u32>>>,
    process: Arc<AtomicUsize>,
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
            process: PROCESS.get_or_init(|| Arc::new(AtomicUsize::new(0))).clone(),
        })
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
            let inner = queue.make_data::<wp_presentation_feedback::WpPresentationFeedback, FeedbackData>(data);
            let feedback: wp_presentation_feedback::WpPresentationFeedback = self.global.send_constructor(
                wp_presentation::Request::Feedback { surface: surface.clone() },
                Arc::new(NativeTap { inner, seen: seen.clone() }),
            ).expect("new native presentation feedback");
            let observation = NativeRequest {
                id: feedback.id(),
                charge: weak_charge,
                seen,
            };
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
    #[test]
    fn window_cap_rolls_back_process_and_close_does_not_release_unresolved_objects() {
        let process = Arc::new(AtomicUsize::new(0));
        let window = Arc::new(WindowPresentation::default());
        let held: Vec<_> = (0..WINDOW_CAP)
            .map(|_| Charge::acquire(process.clone(), window.clone()).unwrap())
            .collect();
        assert!(matches!(
            Charge::acquire(process.clone(), window.clone()),
            Err(PresentationError::Capacity)
        ));
        assert_eq!(process.load(Ordering::Acquire), WINDOW_CAP);
        window.closed.store(true, Ordering::Release);
        assert!(matches!(
            Charge::acquire(process.clone(), window.clone()),
            Err(PresentationError::Closed)
        ));
        assert_eq!(process.load(Ordering::Acquire), WINDOW_CAP);
        let receipt_lease = held[0].clone();
        drop(held);
        assert_eq!(process.load(Ordering::Acquire), 1);
        drop(receipt_lease);
        assert_eq!(process.load(Ordering::Acquire), 0);
        assert_eq!(window.outstanding.load(Ordering::Acquire), 0);
    }
    #[test]
    fn process_cap_survives_window_churn_and_retires_once() {
        let process = Arc::new(AtomicUsize::new(0));
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
        assert_eq!(process.load(Ordering::Acquire), 1);
        drop(clone);
        assert_eq!(process.load(Ordering::Acquire), 0);
    }
}

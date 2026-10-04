//! This interface allows clients to monitor user idle status.
//!
//! ```
//! # extern crate wayland_server;
//! # #[macro_use] extern crate smithay;
//! # use smithay::wayland::compositor::{CompositorHandler, CompositorState, CompositorClientState};
//! use smithay::wayland::idle_notify::{IdleNotifierState, IdleNotifierHandler};
//! # use smithay::input::{Seat, SeatHandler, SeatState, pointer::CursorImageStatus};
//! # use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
//! # use smithay::wayland::pointer_constraints::PointerConstraintsHandler;
//!
//! struct State { idle_notifier: IdleNotifierState<Self> }
//! # let mut event_loop = smithay::reexports::calloop::EventLoop::<State>::try_new().unwrap();
//! # let mut display = wayland_server::Display::<State>::new().unwrap();
//! // Create the idle_notifier state
//! let idle_notifier = IdleNotifierState::<State>::new(
//!     &display.handle(),
//!     event_loop.handle(),
//! );
//!
//! let state = State { idle_notifier };
//!
//! // Implement the necessary trait
//! # impl CompositorHandler for State {
//! #     fn compositor_state(&mut self) -> &mut CompositorState { unimplemented!() }
//! #     fn client_compositor_state<'a>(&self, client: &'a wayland_server::Client) -> &'a CompositorClientState { unimplemented!() }
//! #     fn commit(&mut self, surface: &wayland_server::protocol::wl_surface::WlSurface) {}
//! # }
//! # impl SeatHandler for State {
//! #     type KeyboardFocus = WlSurface;
//! #     type PointerFocus = WlSurface;
//! #     type TouchFocus = WlSurface;
//! #     fn seat_state(&mut self) -> &mut SeatState<Self> { unimplemented!() }
//! #     fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) { unimplemented!() }
//! #     fn cursor_image(&mut self, seat: &Seat<Self>, image: CursorImageStatus) { unimplemented!() }
//! # }
//! # impl PointerConstraintsHandler for State {}
//! impl IdleNotifierHandler for State {
//!     fn idle_notifier_state(&mut self) -> &mut IdleNotifierState<Self> {
//!         &mut self.idle_notifier
//!     }
//! }
//!
//! smithay::delegate_dispatch2!(State);
//!
//! // On input you should notify the idle_notifier
//! // state.idle_notifier.notify_activity(&seat);
//! ```

use std::{
    collections::HashMap,
    marker::PhantomData,
    sync::{
        Arc, Mutex,
        atomic::{self, AtomicBool},
    },
    time::Duration,
};

use calloop::{LoopHandle, RegistrationToken, timer::TimeoutAction};
use wayland_protocols::ext::idle_notify::v1::server::{
    ext_idle_notification_v1::{self, ExtIdleNotificationV1},
    ext_idle_notifier_v1::{self, ExtIdleNotifierV1},
};
use wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
    backend::{ClientId, GlobalId},
    protocol::wl_seat::WlSeat,
};

use crate::{
    input::{Seat, SeatHandler},
    wayland::{Dispatch2, GlobalData, GlobalDispatch2},
};

/// Handler trait for ext-idle-notify
pub trait IdleNotifierHandler: Sized {
    /// [`IdleNotifierState`] getter
    fn idle_notifier_state(&mut self) -> &mut IdleNotifierState<Self>;
}

/// User data of the [`ExtIdleNotificationV1`] resource
#[derive(Debug)]
pub struct IdleNotificationUserData {
    seat: WlSeat,
    is_idle: AtomicBool,
    timeout: Duration,
    timer_token: Mutex<Option<RegistrationToken>>,

    /// If listener was created with `get_input_idle_notification`
    ignore_inhibitor: bool,
}

impl IdleNotificationUserData {
    fn take_timer_token(&self) -> Option<RegistrationToken> {
        self.timer_token.lock().unwrap().take()
    }

    fn set_timer_token(&self, idle: Option<RegistrationToken>) {
        *self.timer_token.lock().unwrap() = idle;
    }

    fn set_idle(&self, idle: bool) {
        self.is_idle.store(idle, atomic::Ordering::Release);
    }

    fn is_idle(&self) -> bool {
        self.is_idle.load(atomic::Ordering::Acquire)
    }
}

/// Where the idle timers are armed (compd patch). Upstream took a
/// `LoopHandle<'static, D>`, D being the dispatch state, and read that state
/// in the timer; every `LoopHandle` still implements this, so passing one
/// compiles as before, but its data type is now free: compd's calloop data is
/// `Wire<A>`, which the crate implementing the handler for `Dispatch` cannot
/// name. A timer only ever calls `fire`.
pub trait IdleTimerLoop: std::fmt::Debug {
    /// Call `fire` once, `after` from now. `None` when the timer could not be
    /// armed.
    fn arm(&self, after: Duration, fire: Box<dyn FnOnce() + 'static>) -> Option<RegistrationToken>;
    /// Drop a timer `arm` returned, fired or not.
    fn disarm(&self, token: RegistrationToken);
}

impl<Data: 'static> IdleTimerLoop for LoopHandle<'static, Data> {
    fn arm(&self, after: Duration, fire: Box<dyn FnOnce() + 'static>) -> Option<RegistrationToken> {
        let mut fire = Some(fire);
        self.insert_source(calloop::timer::Timer::from_duration(after), move |_, _, _| {
            if let Some(fire) = fire.take() {
                fire();
            }
            TimeoutAction::Drop
        })
        .ok()
    }

    fn disarm(&self, token: RegistrationToken) {
        self.remove(token);
    }
}

/// State of ext-idle-notify module
#[derive(Debug)]
pub struct IdleNotifierState<D> {
    global: GlobalId,
    notifications: HashMap<WlSeat, Vec<ExtIdleNotificationV1>>,
    timers: Box<dyn IdleTimerLoop>,
    /// Shared with every armed timer (compd patch): a timer decides from this
    /// flag, never from the loop's data.
    is_inhibited: Arc<AtomicBool>,
    _dispatch: PhantomData<fn() -> D>,
}

impl<D: IdleNotifierHandler> IdleNotifierState<D> {
    /// Create new [`ExtIdleNotifierV1`] global.
    pub fn new(display: &DisplayHandle, loop_handle: impl IdleTimerLoop + 'static) -> Self
    where
        D: GlobalDispatch<ExtIdleNotifierV1, GlobalData>,
        D: IdleNotifierHandler,
        D: 'static,
    {
        let global = display.create_global::<D, ExtIdleNotifierV1, _>(2, GlobalData);
        Self {
            global,
            notifications: HashMap::new(),
            timers: Box::new(loop_handle),
            is_inhibited: Arc::new(AtomicBool::new(false)),
            _dispatch: PhantomData,
        }
    }

    /// Inhibit entering idle state, eg. by the idle-inhibit protocol
    pub fn set_is_inhibited(&mut self, is_inhibited: bool) {
        if self.is_inhibited.load(atomic::Ordering::Acquire) == is_inhibited {
            return;
        }

        self.is_inhibited.store(is_inhibited, atomic::Ordering::Release);

        for notification in self.notifications() {
            let data = notification.data::<IdleNotificationUserData>().unwrap();

            if data.ignore_inhibitor {
                continue;
            }

            if is_inhibited {
                if data.is_idle() {
                    notification.resumed();
                    data.set_idle(false);
                }

                if let Some(token) = data.take_timer_token() {
                    self.timers.disarm(token);
                }
            } else {
                self.reinsert_timer(notification);
            }
        }
    }

    /// Is idle state inhibited, eg. by the idle-inhibit protocol
    pub fn is_inhibited(&mut self) -> bool {
        self.is_inhibited.load(atomic::Ordering::Acquire)
    }

    /// Should be called whenever activity occurs on a seat, eg. mouse/keyboard input.
    ///
    /// You may want to use [`Self::notify_activity`] instead which accepts a [`Seat`].
    pub fn notify_activity_for_wl_seat(&mut self, seat: &WlSeat) {
        let Some(notifications) = self.notifications.get(seat) else {
            return;
        };

        for notification in notifications {
            let data = notification.data::<IdleNotificationUserData>().unwrap();

            if data.is_idle() {
                notification.resumed();
                data.set_idle(false);
            }

            self.reinsert_timer(notification);
        }
    }

    /// Returns the [`ExtIdleNotifierV1`] global.
    pub fn global(&self) -> GlobalId {
        self.global.clone()
    }

    fn notifications(&self) -> impl Iterator<Item = &ExtIdleNotificationV1> {
        self.notifications.values().flatten()
    }

    fn reinsert_timer(&self, notification: &ExtIdleNotificationV1) {
        let data = notification.data::<IdleNotificationUserData>().unwrap();

        if let Some(token) = data.take_timer_token() {
            self.timers.disarm(token);
        }

        if !data.ignore_inhibitor && self.is_inhibited.load(atomic::Ordering::Acquire) {
            return;
        }

        let idle_notification = notification.clone();
        let inhibited = Arc::clone(&self.is_inhibited);
        let token = self.timers.arm(
            data.timeout,
            Box::new(move || {
                let data = idle_notification.data::<IdleNotificationUserData>().unwrap();

                if fires(data.ignore_inhibitor, inhibited.load(atomic::Ordering::Acquire), data.is_idle()) {
                    idle_notification.idled();
                    data.set_idle(true);
                }

                data.set_timer_token(None);
            }),
        );

        data.set_timer_token(token);
    }
}

/// Whether an expiring timer marks its notification idle (compd patch: the
/// upstream closure's test, out where a guard can reach it): not while an
/// inhibitor holds, unless the notification ignores inhibitors, and not twice.
fn fires(ignore_inhibitor: bool, inhibited: bool, is_idle_already: bool) -> bool {
    let is_inhibited = !ignore_inhibitor && inhibited;
    !is_inhibited && !is_idle_already
}

impl<D: IdleNotifierHandler + SeatHandler> IdleNotifierState<D> {
    /// Should be called whenever activity occurs on a seat, eg. mouse/keyboard input.
    pub fn notify_activity(&mut self, seat: &Seat<D>) {
        for seat in &seat.arc.inner.lock().unwrap().known_seats {
            if let Ok(seat) = seat.upgrade() {
                self.notify_activity_for_wl_seat(&seat);
            }
        }
    }
}

impl<D> GlobalDispatch2<ExtIdleNotifierV1, D> for GlobalData
where
    D: Dispatch<ExtIdleNotifierV1, GlobalData>,
    D: IdleNotifierHandler,
    D: 'static,
{
    fn bind(
        &self,
        _state: &mut D,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ExtIdleNotifierV1>,
        data_init: &mut wayland_server::DataInit<'_, D>,
    ) {
        data_init.init(resource, GlobalData);
    }
}

impl<D> Dispatch2<ExtIdleNotifierV1, D> for GlobalData
where
    D: Dispatch<ExtIdleNotificationV1, IdleNotificationUserData>,
    D: IdleNotifierHandler,
    D: 'static,
{
    fn request(
        &self,
        state: &mut D,
        _client: &Client,
        _resource: &ExtIdleNotifierV1,
        request: ext_idle_notifier_v1::Request,
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            ext_idle_notifier_v1::Request::GetIdleNotification { id, timeout, seat } => {
                let timeout = Duration::from_millis(timeout as u64);

                let idle_notifier_state = state.idle_notifier_state();

                let idle_notification = data_init.init(
                    id,
                    IdleNotificationUserData {
                        seat: seat.clone(),
                        is_idle: AtomicBool::new(false),
                        timeout,
                        timer_token: Mutex::new(None),
                        ignore_inhibitor: false,
                    },
                );

                idle_notifier_state.reinsert_timer(&idle_notification);

                state
                    .idle_notifier_state()
                    .notifications
                    .entry(seat)
                    .or_default()
                    .push(idle_notification);
            }
            ext_idle_notifier_v1::Request::GetInputIdleNotification { id, timeout, seat } => {
                let timeout = Duration::from_millis(timeout as u64);

                let idle_notifier_state = state.idle_notifier_state();

                let idle_notification = data_init.init(
                    id,
                    IdleNotificationUserData {
                        seat: seat.clone(),
                        is_idle: AtomicBool::new(false),
                        timeout,
                        timer_token: Mutex::new(None),
                        ignore_inhibitor: true,
                    },
                );

                idle_notifier_state.reinsert_timer(&idle_notification);

                state
                    .idle_notifier_state()
                    .notifications
                    .entry(seat)
                    .or_default()
                    .push(idle_notification);
            }
            ext_idle_notifier_v1::Request::Destroy => {}
            _ => unimplemented!(),
        }
    }
}

impl<D> Dispatch2<ExtIdleNotificationV1, D> for IdleNotificationUserData
where
    D: IdleNotifierHandler,
{
    fn request(
        &self,
        _state: &mut D,
        _client: &Client,
        _resource: &ExtIdleNotificationV1,
        request: ext_idle_notification_v1::Request,
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            ext_idle_notification_v1::Request::Destroy => {}
            _ => unimplemented!(),
        }
    }

    fn destroyed(&self, state: &mut D, _client: ClientId, notification: &ExtIdleNotificationV1) {
        let state = state.idle_notifier_state();
        if let Some(notifications) = state.notifications.get_mut(&self.seat) {
            notifications.retain(|x| x != notification);
        }

        state
            .notifications
            .retain(|seat, notifications| !notifications.is_empty() && seat.is_alive());
    }
}

#[cfg(test)]
mod compd_idle_timer_tests {
    use super::*;

    /// The timer path: armed through `IdleTimerLoop` on a loop whose data is
    /// `()` (so it cannot be reading any dispatch state), it runs `fire` once
    /// when the loop dispatches past the timeout.
    #[test]
    fn a_timer_armed_on_a_foreign_loop_fires_once() {
        let mut event_loop = calloop::EventLoop::<()>::try_new().unwrap();
        let timers: Box<dyn IdleTimerLoop> = Box::new(event_loop.handle());
        let fired = std::rc::Rc::new(std::cell::Cell::new(0));
        let seen = std::rc::Rc::clone(&fired);
        let token = timers.arm(Duration::from_millis(1), Box::new(move || seen.set(seen.get() + 1)));
        assert!(token.is_some());
        for _ in 0..3 {
            event_loop.dispatch(Some(Duration::from_millis(20)), &mut ()).unwrap();
        }
        assert_eq!(fired.get(), 1);
    }

    #[test]
    fn a_disarmed_timer_never_fires() {
        let mut event_loop = calloop::EventLoop::<()>::try_new().unwrap();
        let timers: Box<dyn IdleTimerLoop> = Box::new(event_loop.handle());
        let fired = std::rc::Rc::new(std::cell::Cell::new(false));
        let seen = std::rc::Rc::clone(&fired);
        let token = timers.arm(Duration::from_millis(1), Box::new(move || seen.set(true))).unwrap();
        timers.disarm(token);
        event_loop.dispatch(Some(Duration::from_millis(20)), &mut ()).unwrap();
        assert!(!fired.get());
    }

    #[test]
    fn a_timer_fires_unless_inhibited_or_already_idle() {
        assert!(fires(false, false, false));
        assert!(!fires(false, true, false), "an inhibitor holds");
        assert!(fires(true, true, false), "get_input_idle_notification ignores inhibitors");
        assert!(!fires(false, false, true), "idled once only");
    }
}

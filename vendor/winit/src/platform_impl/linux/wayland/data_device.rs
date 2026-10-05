//! Native drag source/offer ownership on winit's existing Wayland queue.
//! Transfers use calloop readiness and bounded nonblocking pipes.
use super::{WindowId, make_wid, state::WinitState};
use crate::{
    drag::{self, Action, Actions, Gesture, Offer},
    event::WindowEvent,
};
use ahash::AHashMap;
use sctk::data_device_manager::{
    DataDeviceManagerState, WritePipe,
    data_device::{DataDevice, DataDeviceData, DataDeviceHandler},
    data_offer::{DataOfferHandler, DragOffer},
    data_source::{DataSourceHandler, DragSource},
};
use sctk::reexports::calloop::{Interest, Mode, PostAction, RegistrationToken, generic::Generic};
use sctk::reexports::client::{
    Connection, Proxy, QueueHandle,
    backend::ObjectId,
    protocol::{
        wl_data_device::WlDataDevice, wl_data_device_manager::DndAction,
        wl_data_source::WlDataSource, wl_seat::WlSeat, wl_surface::WlSurface,
    },
};
use std::os::fd::OwnedFd;
use std::sync::{Arc, Mutex};

#[derive(Debug)]
pub enum Request {
    Start(Gesture, drag::Source),
    Accept(Offer, Option<String>, Actions, Action),
    Receive(Offer, String),
    Finish(Offer, bool),
    Cancel(Gesture),
}
struct Press {
    seat: WlSeat,
    surface: WlSurface,
    button: u32,
    serial: u32,
}
struct Source {
    _native: DragSource,
    gesture: Gesture,
    window: WindowId,
    seat: ObjectId,
    payload: drag::Source,
    action: Option<Action>,
    pipes: Vec<RegistrationToken>,
}
struct Target {
    native: DragOffer,
    window: WindowId,
    device: ObjectId,
    mime: Option<String>,
    action: Option<Action>,
    dropped: bool,
    receiving: bool,
    delivered: bool,
    pipe: Option<RegistrationToken>,
}
pub struct State {
    manager: Option<DataDeviceManagerState>,
    devices: AHashMap<ObjectId, DataDevice>,
    presses: AHashMap<Gesture, Press>,
    sources: AHashMap<ObjectId, Source>,
    targets: AHashMap<Offer, Target>,
    next: u64,
}
impl State {
    pub fn new(
        manager: Option<DataDeviceManagerState>,
        seats: impl Iterator<Item = WlSeat>,
        qh: &QueueHandle<WinitState>,
    ) -> Self {
        let mut state = Self {
            manager,
            devices: Default::default(),
            presses: Default::default(),
            sources: Default::default(),
            targets: Default::default(),
            next: 1,
        };
        for seat in seats {
            state.add_seat(&seat, qh);
        }
        state
    }
    fn id(&mut self) -> u64 {
        let id = self.next;
        self.next = self.next.checked_add(1).expect("native drag identity exhausted");
        id
    }
    pub fn add_seat(&mut self, seat: &WlSeat, qh: &QueueHandle<WinitState>) {
        if let Some(manager) = &self.manager {
            self.devices.insert(seat.id(), manager.get_data_device(qh, seat));
        }
    }
    pub fn press(
        &mut self,
        seat: &WlSeat,
        surface: &WlSurface,
        button: u32,
        serial: u32,
    ) -> Option<Gesture> {
        if button != 0x110 || self.manager.as_ref()?.data_device_manager().version() < 3 {
            return None;
        }
        self.release(seat, button);
        let id = Gesture(self.id());
        self.presses
            .insert(id, Press { seat: seat.clone(), surface: surface.clone(), button, serial });
        Some(id)
    }
    pub fn release(&mut self, seat: &WlSeat, button: u32) {
        self.presses.retain(|_, press| press.seat != *seat || press.button != button);
    }
}
fn native_actions(actions: Actions) -> DndAction {
    let mut result = DndAction::empty();
    if actions.copy {
        result |= DndAction::Copy;
    }
    if actions.move_ {
        result |= DndAction::Move;
    }
    result
}
fn native_action(action: Action) -> DndAction {
    match action {
        Action::Copy => DndAction::Copy,
        Action::Move => DndAction::Move,
    }
}
fn action(action: DndAction) -> Option<Action> {
    match action {
        DndAction::Copy => Some(Action::Copy),
        DndAction::Move => Some(Action::Move),
        _ => None,
    }
}
fn nonblocking(fd: &OwnedFd) -> std::io::Result<()> {
    let flags = rustix::fs::fcntl_getfl(fd)?;
    rustix::fs::fcntl_setfl(fd, flags | rustix::fs::OFlags::NONBLOCK)?;
    Ok(())
}
impl WinitState {
    fn drag_event(&mut self, window: WindowId, event: drag::Event) {
        if self.windows.get_mut().contains_key(&window) {
            self.events_sink.push_window_event(WindowEvent::DragDrop(event), window);
            self.dispatched_events = true;
        }
    }
    fn discard_target(&mut self, offer: Offer) {
        if let Some(target) = self.drag.targets.remove(&offer) {
            if let Some(token) = target.pipe {
                self.loop_handle.remove(token);
            }
            target.native.destroy();
        }
    }
    fn discard_source(&mut self, source: ObjectId, finished: bool) {
        if let Some(source) = self.drag.sources.remove(&source) {
            for token in &source.pipes {
                self.loop_handle.remove(*token);
            }
            let event = if finished {
                source
                    .action
                    .map(|action| drag::Event::Finished { gesture: source.gesture, action })
                    .unwrap_or(drag::Event::Cancelled(source.gesture))
            } else {
                drag::Event::Cancelled(source.gesture)
            };
            self.drag_event(source.window, event);
            // Drop is the unique owner of wl_data_source.destroy.
        }
    }
    fn retire_source_pipe(&mut self, source: &ObjectId, token: Option<RegistrationToken>) {
        if let Some(source) = self.drag.sources.get_mut(source) {
            source.pipes.retain(|registered| Some(*registered) != token);
        }
    }
    pub fn drag_remove_seat(&mut self, seat: &WlSeat) {
        self.drag_pointer_removed(seat);
        if let Some(device) = self.drag.devices.remove(&seat.id()) {
            let targets: Vec<_> = self
                .drag
                .targets
                .iter()
                .filter(|(_, t)| t.device == device.inner().id())
                .map(|(id, _)| *id)
                .collect();
            for offer in targets {
                self.discard_target(offer);
            }
        }
    }
    pub fn drag_pointer_removed(&mut self, seat: &WlSeat) {
        self.drag.presses.retain(|_, press| press.seat != *seat);
        // A removed seat cannot continue its pointer grab. The compositor
        // also cancels its source, but retire our ownership immediately.
        let sources: Vec<_> = self
            .drag
            .sources
            .iter()
            .filter(|(_, s)| s.seat == seat.id())
            .map(|(id, _)| id.clone())
            .collect();
        for id in sources {
            self.discard_source(id, false);
        }
    }
    pub fn drag_close_window(&mut self, window: WindowId) {
        self.drag.presses.retain(|_, p| make_wid(&p.surface) != window);
        let sources: Vec<_> = self
            .drag
            .sources
            .iter()
            .filter(|(_, s)| s.window == window)
            .map(|(id, _)| id.clone())
            .collect();
        for id in sources {
            self.discard_source(id, false);
        }
        let offers: Vec<_> = self
            .drag
            .targets
            .iter()
            .filter(|(_, t)| t.window == window)
            .map(|(id, _)| *id)
            .collect();
        for id in offers {
            self.discard_target(id);
        }
    }
    pub fn dispatch_drag_requests(&mut self, qh: &QueueHandle<Self>) {
        let requests: Vec<_> = self
            .window_requests
            .get_mut()
            .iter()
            .flat_map(|(id, requests)| {
                requests.drag.lock().unwrap().drain(..).map(|r| (*id, r)).collect::<Vec<_>>()
            })
            .collect();
        for (window, request) in requests {
            match request {
                Request::Start(gesture, payload) => {
                    let valid = self
                        .drag
                        .presses
                        .get(&gesture)
                        .is_some_and(|p| make_wid(&p.surface) == window);
                    if !valid || !payload.valid() {
                        self.drag_event(window, drag::Event::Rejected(gesture));
                        continue;
                    }
                    let press = self.drag.presses.remove(&gesture).unwrap();
                    if self.drag.sources.values().any(|source| source.seat == press.seat.id()) {
                        self.drag_event(window, drag::Event::Rejected(gesture));
                        continue;
                    }
                    let Some(manager) = self.drag.manager.as_ref() else {
                        self.drag_event(window, drag::Event::Rejected(gesture));
                        continue;
                    };
                    let Some(device) = self.drag.devices.get(&press.seat.id()) else {
                        self.drag_event(window, drag::Event::Rejected(gesture));
                        continue;
                    };
                    let native = manager.create_drag_and_drop_source(
                        qh,
                        [&payload.mime],
                        native_actions(payload.actions),
                    );
                    native.start_drag(device, &press.surface, None, press.serial);
                    let id = native.inner().id();
                    self.drag.sources.insert(
                        id,
                        Source {
                            _native: native,
                            gesture,
                            window,
                            seat: press.seat.id(),
                            payload,
                            action: None,
                            pipes: vec![],
                        },
                    );
                    self.drag_event(window, drag::Event::Started(gesture));
                },
                Request::Accept(offer, mime, actions, preferred) => {
                    let Some(target) = self
                        .drag
                        .targets
                        .get_mut(&offer)
                        .filter(|t| t.window == window && !t.dropped)
                    else {
                        self.drag_event(window, drag::Event::Failed(offer));
                        continue;
                    };
                    let valid = mime
                        .as_ref()
                        .is_none_or(|m| target.native.with_mime_types(|types| types.contains(m)));
                    if !valid {
                        self.drag_event(window, drag::Event::Failed(offer));
                        continue;
                    }
                    target.native.accept_mime_type(target.native.serial, mime.clone());
                    target.native.set_actions(native_actions(actions), native_action(preferred));
                    target.mime = mime;
                },
                Request::Receive(offer, mime) => self.receive_drag(window, offer, mime),
                Request::Finish(offer, applied) => {
                    let valid = self.drag.targets.get(&offer).is_some_and(|t| {
                        t.window == window
                            && t.dropped
                            && (!applied || (t.delivered && t.action.is_some()))
                    });
                    if !valid {
                        self.drag_event(window, drag::Event::Failed(offer));
                        continue;
                    }
                    if applied {
                        self.drag.targets[&offer].native.finish();
                    }
                    self.discard_target(offer);
                },
                Request::Cancel(gesture) => {
                    let source = self
                        .drag
                        .sources
                        .iter()
                        .find(|(_, s)| s.window == window && s.gesture == gesture)
                        .map(|(id, _)| id.clone());
                    if let Some(source) = source {
                        self.discard_source(source, false);
                    } else if self
                        .drag
                        .presses
                        .get(&gesture)
                        .is_some_and(|p| make_wid(&p.surface) == window)
                    {
                        self.drag.presses.remove(&gesture);
                        self.drag_event(window, drag::Event::Cancelled(gesture));
                    }
                },
            }
        }
    }
    fn receive_drag(&mut self, window: WindowId, offer: Offer, mime: String) {
        let valid = self.drag.targets.get(&offer).is_some_and(|t| {
            t.window == window
                && t.dropped
                && !t.receiving
                && t.mime.as_ref() == Some(&mime)
                && t.action.is_some()
        });
        if !valid {
            self.drag_event(window, drag::Event::Failed(offer));
            return;
        }
        let pipe = self.drag.targets[&offer].native.receive(mime.clone());
        let fd: OwnedFd = match pipe {
            Ok(pipe) => pipe.into(),
            Err(_) => {
                self.drag_event(window, drag::Event::Failed(offer));
                self.discard_target(offer);
                return;
            },
        };
        if nonblocking(&fd).is_err() {
            self.drag_event(window, drag::Event::Failed(offer));
            self.discard_target(offer);
            return;
        }
        let mut bytes = Vec::new();
        let token = self.loop_handle.insert_source(
            Generic::new(fd, Interest::READ, Mode::Level),
            move |_, fd, state| {
                if !state.drag.targets.contains_key(&offer) {
                    return Ok(PostAction::Remove);
                }
                let mut buffer = [0u8; 16384];
                loop {
                    match rustix::io::read(&*fd, &mut buffer) {
                        Ok(0) => {
                            let target = state.drag.targets.get_mut(&offer).unwrap();
                            target.delivered = true;
                            target.pipe = None;
                            if let Some(action) = target.action {
                                state.drag_event(
                                    window,
                                    drag::Event::Data {
                                        offer,
                                        mime: mime.clone(),
                                        bytes: Arc::from(std::mem::take(&mut bytes)),
                                        action,
                                    },
                                );
                            } else {
                                state.drag_event(window, drag::Event::Failed(offer));
                            }
                            return Ok(PostAction::Remove);
                        },
                        Ok(count) if bytes.len() + count <= drag::Source::MAX_BYTES => {
                            bytes.extend_from_slice(&buffer[..count])
                        },
                        Err(rustix::io::Errno::INTR) => continue,
                        Err(rustix::io::Errno::AGAIN) => return Ok(PostAction::Continue),
                        _ => {
                            state.drag_event(window, drag::Event::Failed(offer));
                            if let Some(target) = state.drag.targets.get_mut(&offer) {
                                target.pipe = None;
                            }
                            state.discard_target(offer);
                            return Ok(PostAction::Remove);
                        },
                    }
                }
            },
        );
        match token {
            Ok(token) => {
                let target = self.drag.targets.get_mut(&offer).unwrap();
                target.receiving = true;
                target.pipe = Some(token);
            },
            Err(_) => {
                self.drag_event(window, drag::Event::Failed(offer));
                self.discard_target(offer);
            },
        }
    }
    fn offer_for_device(&self, device: &WlDataDevice) -> Option<Offer> {
        self.drag
            .targets
            .iter()
            .find(|(_, t)| t.device == device.id() && !t.dropped)
            .map(|(id, _)| *id)
    }
}
impl DataDeviceHandler for WinitState {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        device: &WlDataDevice,
        x: f64,
        y: f64,
        surface: &WlSurface,
    ) {
        let Some(native) = device.data::<DataDeviceData>().and_then(DataDeviceData::drag_offer)
        else {
            return;
        };
        let window = make_wid(surface);
        if !self.windows.get_mut().contains_key(&window) {
            native.accept_mime_type(native.serial, None);
            return;
        }
        let mimes = native.with_mime_types(|types| {
            types.iter().filter(|m| m.len() <= 255).take(256).cloned().collect()
        });
        let offer = Offer(self.drag.id());
        let action = action(native.selected_action);
        self.drag.targets.insert(
            offer,
            Target {
                native,
                window,
                device: device.id(),
                mime: None,
                action,
                dropped: false,
                receiving: false,
                delivered: false,
                pipe: None,
            },
        );
        self.drag_event(
            window,
            drag::Event::Enter { offer, position: crate::dpi::LogicalPosition::new(x, y), mimes },
        );
        if action.is_some() {
            self.drag_event(window, drag::Event::Action { offer, action });
        }
    }
    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, device: &WlDataDevice) {
        if let Some(offer) = self.offer_for_device(device) {
            // SCTK has already destroyed an undropped offer on leave.
            let target = self.drag.targets.remove(&offer).unwrap();
            self.drag_event(target.window, drag::Event::Leave(offer));
        }
    }
    fn motion(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        device: &WlDataDevice,
        x: f64,
        y: f64,
    ) {
        if let Some(offer) = self.offer_for_device(device) {
            let window = self.drag.targets[&offer].window;
            self.drag_event(
                window,
                drag::Event::Motion { offer, position: crate::dpi::LogicalPosition::new(x, y) },
            );
        }
    }
    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
    fn drop_performed(&mut self, _: &Connection, _: &QueueHandle<Self>, device: &WlDataDevice) {
        if let Some(offer) = self.offer_for_device(device) {
            let target = self.drag.targets.get_mut(&offer).unwrap();
            target.dropped = true;
            if let Some(current) =
                device.data::<DataDeviceData>().and_then(DataDeviceData::take_dropped_offer)
            {
                target.action = action(current.selected_action);
            }
            let window = target.window;
            self.drag_event(window, drag::Event::Drop(offer));
        }
    }
}
impl DataOfferHandler for WinitState {
    fn source_actions(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        _: DndAction,
    ) {
    }
    fn selected_action(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        native: &mut DragOffer,
        selected: DndAction,
    ) {
        if let Some((offer, target)) =
            self.drag.targets.iter_mut().find(|(_, t)| t.native.inner() == native.inner())
        {
            target.action = action(selected);
            let (offer, window, action) = (*offer, target.window, target.action);
            self.drag_event(window, drag::Event::Action { offer, action });
        }
    }
}
impl DataSourceHandler for WinitState {
    fn accept_mime(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataSource,
        _: Option<String>,
    ) {
    }
    fn send_request(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        source: &WlDataSource,
        mime: String,
        pipe: WritePipe,
    ) {
        let Some(owned) = self.drag.sources.get(&source.id()).filter(|s| s.payload.mime == mime)
        else {
            return;
        };
        // A hostile receiver may request the same MIME repeatedly. Bound the
        // concurrent registrations for this source.
        if owned.pipes.len() >= 16 {
            self.discard_source(source.id(), false);
            return;
        }
        let bytes = owned.payload.bytes.clone();
        let id = source.id();
        let fd: OwnedFd = pipe.into();
        if nonblocking(&fd).is_err() {
            self.discard_source(id, false);
            return;
        }
        let mut offset = 0;
        let registered = Arc::new(Mutex::new(None));
        let completed = registered.clone();
        let token = self.loop_handle.insert_source(
            Generic::new(fd, Interest::WRITE, Mode::Level),
            move |_, fd, state| {
                if !state.drag.sources.contains_key(&id) {
                    return Ok(PostAction::Remove);
                }
                while offset < bytes.len() {
                    match rustix::io::write(&*fd, &bytes[offset..]) {
                        Ok(0) => {
                            state.retire_source_pipe(&id, *completed.lock().unwrap());
                            state.discard_source(id.clone(), false);
                            return Ok(PostAction::Remove);
                        },
                        Ok(count) => offset += count,
                        Err(rustix::io::Errno::INTR) => continue,
                        Err(rustix::io::Errno::AGAIN) => return Ok(PostAction::Continue),
                        Err(_) => {
                            state.retire_source_pipe(&id, *completed.lock().unwrap());
                            state.discard_source(id.clone(), false);
                            return Ok(PostAction::Remove);
                        },
                    }
                }
                state.retire_source_pipe(&id, *completed.lock().unwrap());
                Ok(PostAction::Remove)
            },
        );
        match token {
            Ok(token) => {
                *registered.lock().unwrap() = Some(token);
                self.drag.sources.get_mut(&source.id()).unwrap().pipes.push(token);
            },
            Err(_) => self.discard_source(source.id(), false),
        }
    }
    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &WlDataSource) {
        self.discard_source(source.id(), false);
    }
    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &WlDataSource) {
        self.discard_source(source.id(), true);
    }
    fn action(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        source: &WlDataSource,
        selected: DndAction,
    ) {
        if let Some(source) = self.drag.sources.get_mut(&source.id()) {
            source.action = action(selected);
        }
    }
}
sctk::delegate_data_device!(WinitState);

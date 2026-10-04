use std::{
    any::Any,
    cell::RefCell,
    os::fd::{AsFd, OwnedFd},
    sync::Mutex,
};
use tracing::{debug, error};

use wayland_server::{
    DisplayHandle, Resource,
    backend::ClientId,
    protocol::{
        wl_data_source::{self, WlDataSource},
        wl_surface::WlSurface,
    },
};

use crate::input::{
    Seat,
    // compd: `GrabType` for the drag cancel in `destroyed()`.
    dnd::{DndAction, GrabType, Source, SourceMetadata},
};
// compd: `Clock`/`Monotonic`/`SERIAL_COUNTER` for the drag cancel.
use crate::utils::{Clock, IsAlive, Monotonic, SERIAL_COUNTER, alive_tracker::AliveTracker};
use crate::wayland::Dispatch2;
use crate::wayland::selection::SelectionTarget;
use crate::wayland::selection::offer::OfferReplySource;
use crate::wayland::selection::seat_data::SeatData;
use crate::wayland::selection::source::{CompositorSelectionProvider, SelectionSourceProvider};

use super::DataDeviceHandler;

#[doc(hidden)]
#[derive(Debug)]
pub struct DataSourceUserData {
    pub(crate) inner: Mutex<SourceMetadata>,
    alive_tracker: AliveTracker,
    display_handle: DisplayHandle,
}

impl DataSourceUserData {
    pub(super) fn new(display_handle: DisplayHandle) -> Self {
        Self {
            inner: Default::default(),
            alive_tracker: Default::default(),
            display_handle,
        }
    }
}

impl<D> Dispatch2<WlDataSource, D> for DataSourceUserData
where
    D: DataDeviceHandler,
    D: 'static,
{
    fn request(
        &self,
        _state: &mut D,
        _client: &wayland_server::Client,
        _resource: &WlDataSource,
        request: wl_data_source::Request,
        _dhandle: &DisplayHandle,
        _data_init: &mut wayland_server::DataInit<'_, D>,
    ) {
        let mut data = self.inner.lock().unwrap();

        match request {
            wl_data_source::Request::Offer { mime_type } => {
                data.mime_types.push(mime_type);
            }
            wl_data_source::Request::SetActions { dnd_actions } => match dnd_actions {
                wayland_server::WEnum::Value(dnd_actions) => {
                    data.dnd_actions = DndAction::vec_from_wl(dnd_actions);
                }
                wayland_server::WEnum::Unknown(action) => {
                    error!("Unknown dnd_action: {:?}", action);
                }
            },
            wl_data_source::Request::Destroy => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(&self, state: &mut D, _client: ClientId, source: &WlDataSource) {
        self.alive_tracker.destroy_notify();
        // compd hook 3a: report every destroy, owning or not, before anything else.
        state.selection_source_gone(crate::wayland::selection::SelectionSource {
            provider: SelectionSourceProvider::DataDevice(source.clone()),
        });

        // Remove the source from the used ones.
        // compd: the drag cancel below must run even for a source
        // with no used-source record, so a missing record no longer ends this function.
        let seat = state
            .data_device_state()
            .used_sources
            .remove(source)
            .as_ref()
            .and_then(Seat::<D>::from_resource);

        if let Some(seat) = seat {
            // compd hook: the atomic clipboard replacement, via the shared path
            // (scoped ownership read, then the handler with no borrow held). A
            // `wl_data_source` can only own the clipboard, so only that target is
            // replaced.
            crate::wayland::selection::replace_owned_selections(
                state,
                &self.display_handle,
                &seat,
                |p| matches!(p, SelectionSourceProvider::DataDevice(s) if s == source),
            );
        }

        // compd: destroying the source (the Wayland way for a client
        // to cancel a drag — e.g. Escape) or its client disconnecting ends any live drag
        // using it, now.
        cancel_drags_using(state, source);
    }
}

// compd: end every live drag whose source is `source`, on every
// seat (a used-source record names one seat at most, and may be stale).
//
// Found through the live-drag registry (`input::dnd::live_drags_using`), not a
// downcast of the active grab: here the compositor builds the grab, so its concrete
// type is unknown. Only a drag whose `Source` type IS `WlDataSource` is found; a
// compositor wrapping the source in its own type is still covered by `DnDGrab`'s own
// backstop (it cancels on the next motion/release once the source is not alive).
//
// The pointer grab is unset WITH focus restore: unsetting
// without restore leaves the pointer focus-less, so the button the user is still
// holding releases into no client. `unset_grab` takes the handle's lock and re-enters
// the compositor (DnD `cancelled`, focus enter/leave), exactly as an ordinary DnD
// release does; this is safe as long as nothing it calls re-enters the same handle,
// and `destroyed()` never runs from inside a grab callback.
fn cancel_drags_using<D>(state: &mut D, source: &WlDataSource)
where
    D: DataDeviceHandler,
    D: 'static,
{
    let seats = state.seat_state().seats.clone();
    for seat in seats {
        let kinds = crate::input::dnd::live_drags_using::<D, WlDataSource>(&seat, |s| s == source);
        for kind in kinds {
            match kind {
                GrabType::Pointer => {
                    if let Some(pointer) = seat.get_pointer() {
                        let time = Clock::<Monotonic>::new().now().as_millis();
                        pointer.unset_grab(state, SERIAL_COUNTER.next_serial(), time);
                    }
                }
                GrabType::Touch => {
                    if let Some(touch) = seat.get_touch() {
                        touch.unset_grab(state);
                    }
                }
            }
        }
    }
}

impl IsAlive for WlDataSource {
    #[inline]
    fn alive(&self) -> bool {
        let data: &DataSourceUserData = self.data().unwrap();
        data.alive_tracker.alive()
    }
}

impl Source for WlDataSource {
    fn metadata(&self) -> Option<SourceMetadata> {
        self.data::<DataSourceUserData>()
            .map(|data| data.inner.lock().unwrap().clone())
    }

    fn choose_action(&self, action: DndAction) {
        self.action(action.into());
    }

    fn send(&self, mime_type: &str, fd: OwnedFd) {
        debug!(?mime_type, "DnD transfer request");
        self.send(mime_type.to_owned(), fd.as_fd());
    }

    fn drop_performed(&self) {
        if self.version() >= wl_data_source::EVT_DND_DROP_PERFORMED_SINCE {
            self.dnd_drop_performed();
        }
    }

    fn cancel(&self) {
        self.cancelled();
    }

    fn finished(&self) {
        if self.version() >= wl_data_source::EVT_DND_FINISHED_SINCE {
            self.dnd_finished();
        }
    }
}

impl Source for WlSurface {
    fn is_client_local(&self, target: &dyn Any) -> bool {
        target
            .downcast_ref::<WlSurface>()
            .is_some_and(|target| target.id().same_client_as(&self.id()))
    }

    fn metadata(&self) -> Option<SourceMetadata> {
        None
    }

    fn choose_action(&self, action: DndAction) {
        let _ = action;
    }

    fn send(&self, mime_type: &str, fd: OwnedFd) {
        let _ = (mime_type, fd);
        unreachable!("Local dnd drops can't send");
    }

    fn drop_performed(&self) {}
    fn cancel(&self) {}
    fn finished(&self) {}
}

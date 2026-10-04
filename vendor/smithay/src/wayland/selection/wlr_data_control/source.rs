use std::cell::RefCell;
use std::sync::Mutex;

use wayland_protocols_wlr::data_control::v1::server::zwlr_data_control_source_v1::{
    self, ZwlrDataControlSourceV1,
};
use wayland_server::DisplayHandle;
use wayland_server::backend::ClientId;

use crate::input::Seat;
use crate::wayland::Dispatch2;
use crate::wayland::selection::SelectionTarget;
use crate::wayland::selection::offer::OfferReplySource;
use crate::wayland::selection::seat_data::SeatData;
use crate::wayland::selection::source::SelectionSourceProvider;

use super::DataControlHandler;

#[doc(hidden)]
#[derive(Debug)]
pub struct DataControlSourceUserData {
    pub(crate) inner: Mutex<SourceMetadata>,
    display_handle: DisplayHandle,
}

impl DataControlSourceUserData {
    pub(crate) fn new(display_handle: DisplayHandle) -> Self {
        Self {
            inner: Default::default(),
            display_handle,
        }
    }
}

/// The metadata describing a data source
#[derive(Debug, Default, Clone)]
pub struct SourceMetadata {
    /// The MIME types supported by this source
    pub mime_types: Vec<String>,
}

impl<D> Dispatch2<ZwlrDataControlSourceV1, D> for DataControlSourceUserData
where
    D: DataControlHandler,
    D: 'static,
{
    fn request(
        &self,
        _state: &mut D,
        _client: &wayland_server::Client,
        _resource: &ZwlrDataControlSourceV1,
        request: <ZwlrDataControlSourceV1 as wayland_server::Resource>::Request,
        _dhandle: &DisplayHandle,
        _data_init: &mut wayland_server::DataInit<'_, D>,
    ) {
        match request {
            zwlr_data_control_source_v1::Request::Offer { mime_type } => {
                let mut data = self.inner.lock().unwrap();
                data.mime_types.push(mime_type);
            }
            zwlr_data_control_source_v1::Request::Destroy => (),
            _ => unreachable!(),
        }
    }

    fn destroyed(&self, state: &mut D, _client: ClientId, source: &ZwlrDataControlSourceV1) {
        // compd hook 3a: report every destroy, owning or not, before anything else.
        state.selection_source_gone(crate::wayland::selection::SelectionSource {
            provider: SelectionSourceProvider::WlrDataControl(source.clone()),
        });

        // Remove the source from the used ones.
        let seat = match state
            .data_control_state()
            .used_sources
            .remove(source)
            .as_ref()
            .and_then(Seat::<D>::from_resource)
        {
            Some(seat) => seat,
            None => return,
        };

        // compd hook: a data-control source may own the clipboard, the primary
        // selection or both; each owned target gets the atomic replacement offer
        // (upstream cleared both unconditionally).
        crate::wayland::selection::replace_owned_selections(
            state,
            &self.display_handle,
            &seat,
            |p| matches!(p, SelectionSourceProvider::WlrDataControl(s) if s == source),
        );
    }
}

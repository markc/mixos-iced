use std::{cell::RefCell, sync::Mutex};

use wayland_protocols::wp::primary_selection::zv1::server::zwp_primary_selection_source_v1::{
    self as primary_source, ZwpPrimarySelectionSourceV1 as PrimarySource,
};
use wayland_server::{DisplayHandle, backend::ClientId};

use crate::{
    input::Seat,
    wayland::{
        Dispatch2,
        selection::{offer::OfferReplySource, seat_data::SeatData, source::SelectionSourceProvider},
    },
};

use super::PrimarySelectionHandler;

/// The metadata describing a data source
#[derive(Debug, Default, Clone)]
pub struct SourceMetadata {
    /// The MIME types supported by this source
    pub mime_types: Vec<String>,
}

#[doc(hidden)]
#[derive(Debug)]
pub struct PrimarySourceUserData {
    pub(crate) inner: Mutex<SourceMetadata>,
    display_handle: DisplayHandle,
}

impl PrimarySourceUserData {
    pub(super) fn new(display_handle: DisplayHandle) -> Self {
        Self {
            inner: Default::default(),
            display_handle,
        }
    }
}

impl<D> Dispatch2<PrimarySource, D> for PrimarySourceUserData
where
    D: PrimarySelectionHandler,
    D: 'static,
{
    fn request(
        &self,
        state: &mut D,
        _client: &wayland_server::Client,
        _resource: &PrimarySource,
        request: primary_source::Request,
        _dhandle: &DisplayHandle,
        _data_init: &mut wayland_server::DataInit<'_, D>,
    ) {
        let _primary_selection_state = state.primary_selection_state();
        let mut data = self.inner.lock().unwrap();

        match request {
            primary_source::Request::Offer { mime_type } => {
                data.mime_types.push(mime_type);
            }
            primary_source::Request::Destroy => {}
            _ => unreachable!(),
        }
    }

    fn destroyed(&self, state: &mut D, _client: ClientId, source: &PrimarySource) {
        // compd hook 3a: report every destroy, owning or not, before anything else.
        state.selection_source_gone(crate::wayland::selection::SelectionSource {
            provider: SelectionSourceProvider::Primary(source.clone()),
        });

        // Remove the source from the used ones.
        let seat = match state
            .primary_selection_state()
            .used_sources
            .remove(source)
            .as_ref()
            .and_then(Seat::<D>::from_resource)
        {
            Some(seat) => seat,
            None => return,
        };

        // compd hook: the primary selection now gets the same atomic replacement
        // offer as the clipboard (upstream cleared it unconditionally).
        crate::wayland::selection::replace_owned_selections(
            state,
            &self.display_handle,
            &seat,
            |p| matches!(p, SelectionSourceProvider::Primary(s) if s == source),
        );
    }
}

pub mod wire {
    use smithay::backend::allocator::dmabuf::Dmabuf;
    use smithay::reexports::wayland_protocols::wp::linux_dmabuf::zv1::server::zwp_linux_buffer_params_v1;
    use smithay::reexports::wayland_server::protocol::wl_buffer;
    use smithay::wayland::buffer::BufferHandler;
    use smithay::wayland::dmabuf::{
        DmabufGlobal, DmabufHandler, DmabufParamsData, DmabufState, ImportNotifier,
    };
    use crate::state::state::{Dispatch, DispatchWire};

    pub fn dmabuf_state(
        dispatch: &mut Dispatch,
    ) -> &mut DmabufState {
        &mut dispatch.dmabuf.state
    }

    pub fn dmabuf_imported<WireObject: DispatchWire>(
        dispatch: &mut Dispatch,
        _global: &DmabufGlobal,
        _dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) where
        WireObject: smithay::reexports::wayland_server::Dispatch<
                zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1,
                DmabufParamsData,
            > + smithay::reexports::wayland_server::Dispatch<wl_buffer::WlBuffer, Dmabuf>
            + BufferHandler
            + DmabufHandler
            + 'static,
    {
        // No GLES validator for this buffer (no GPU binding, or none for the
        // composite node): smithay default import, counted as accepted. A
        // client gone before the answer still counts: the ledger records
        // what compd decided, as on the validated path.
        let _ = notifier.successful::<WireObject>();
        dispatch.dmabuf_ledger.record_accepted();
    }
}

pub mod dispatch {
    use smithay::{
        backend::drm::DrmDeviceFd,
        reexports::{
            wayland_protocols::wp::linux_drm_syncobj::v1::server::wp_linux_drm_syncobj_manager_v1::WpLinuxDrmSyncobjManagerV1,
            wayland_server::GlobalDispatch,
        },
        wayland::{
            dmabuf::DmabufState,
            drm_syncobj::{DrmSyncobjGlobalData, DrmSyncobjState, supports_syncobj_eventfd},
        },
    };
    use crate::state::state::{Dispatch, DispatchWire};

    pub fn hook_syncobj<WireObject: DispatchWire>(dispatch: &mut Dispatch, fd: DrmDeviceFd)
    where
        WireObject: GlobalDispatch<WpLinuxDrmSyncobjManagerV1, DrmSyncobjGlobalData>,
        WireObject: 'static,
    {
        if supports_syncobj_eventfd(&fd) {
            info!("DRM syncobj is supported, initializing state (linux-drm-syncobj-v1)");

            dispatch.dmabuf.syncobj_state = Some(DrmSyncobjState::new::<WireObject>(
                &dispatch.output.display_handle,
                fd,
            ));
        } else {
            warn!("DRM syncobj eventfd not supported; clients will use implicit sync");
        }
    }
}

use smithay::reexports::wayland_protocols::xdg::foreign::zv2::server::zxdg_exporter_v2::ZxdgExporterV2;
use smithay::reexports::wayland_protocols::xdg::foreign::zv2::server::zxdg_importer_v2::ZxdgImporterV2;
use smithay::reexports::wayland_server::{Dispatch, DisplayHandle, GlobalDispatch};
use smithay::wayland::GlobalData;
use smithay::wayland::xdg_foreign::{XdgForeignHandler, XdgForeignState};
use crate::state::state::DispatchWire;
use protocols::xdg::foreign::state::Foreign;
pub fn new<I: DispatchWire>(display_handle: &DisplayHandle) -> Foreign
where
    I: XdgForeignHandler,
    I: GlobalDispatch<ZxdgExporterV2, GlobalData>,
    I: GlobalDispatch<ZxdgImporterV2, GlobalData>,
{
    let xdg_foreign_state = XdgForeignState::new::<I>(&display_handle);

    Foreign {
        xdg_foreign_state,
    }
}

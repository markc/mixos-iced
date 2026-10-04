use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_decoration_manager_v1;
use smithay::reexports::wayland_server::{Dispatch, DisplayHandle, GlobalDispatch};
use smithay::wayland::GlobalData;
use smithay::wayland::shell::xdg::decoration::{
    XdgDecorationHandler, XdgDecorationManagerGlobalData, XdgDecorationState,
};
use crate::state::state::DispatchWire;
use protocols::xdg::decoration::state::Decoration;

/// Initializes the XDG decoration manager.
///
/// **Compositor Hookup:**
/// Calling `XdgDecorationState::new` registers the `zxdg_decoration_manager_v1` global
/// object with the Wayland display. When the event loop (`calloop`) processes new client
/// connections, clients will see this global in the registry and can bind to it.
///
/// Note: For this to work, your `Loop` struct must use Smithay's `delegate_xdg_decoration!(Loop);`
/// macro elsewhere in your codebase to route the Wayland socket events into the
/// `XdgDecorationHandler` trait implementation below.
pub fn new<I: DispatchWire>(display_handle: &DisplayHandle) -> Decoration
where
    I: GlobalDispatch<
            zxdg_decoration_manager_v1::ZxdgDecorationManagerV1,
            XdgDecorationManagerGlobalData,
        > + Dispatch<zxdg_decoration_manager_v1::ZxdgDecorationManagerV1, GlobalData>
        + 'static,
{
    let xdg_decoration_state = XdgDecorationState::new::<I>(&display_handle);

    Decoration {
        xdg_state: xdg_decoration_state,
    }
}

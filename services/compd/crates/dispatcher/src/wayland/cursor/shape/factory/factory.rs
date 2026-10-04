use smithay::input::SeatHandler;
use smithay::reexports::wayland_server::{Dispatch, DisplayHandle, GlobalDispatch};
use smithay::wayland::GlobalData;
use smithay::wayland::cursor_shape::CursorShapeManagerState;
use crate::state::state::DispatchWire;
use protocols::cursor::shape::state::CursorShape;
use smithay::reexports::wayland_protocols::wp::cursor_shape::v1::server::wp_cursor_shape_manager_v1::WpCursorShapeManagerV1 as CursorShapeManager;

pub fn new<I: DispatchWire>(display_handle: &DisplayHandle) -> CursorShape
where
    I: GlobalDispatch<CursorShapeManager, GlobalData>,
    I: Dispatch<CursorShapeManager, GlobalData>,
    I: SeatHandler,
    I: 'static,
{
    // Initialize the XDG Shell protocol.
    // Side-effect: Triggers window mapping/unmapping. When a client requests a new Toplevel,
    // it dispatches an event in your XDG delegate to assign the window a position in the `Space`.
    let cursor_shape_state = CursorShapeManagerState::new::<I>(&display_handle);
    CursorShape {
        state: cursor_shape_state,
    }
}

use smithay::backend::allocator::format::FormatSet;
use smithay::output::{Mode, Output};
use smithay::reexports::wayland_server::DisplayHandle;

// Backend represented by mode, output and renderer.
// These are shared between renderer and backend which makes it problematic.
// However, renderer is probably available to re-reference by the renderer() call.
// pub struct Backend {
//     pub mode: Mode,
//     pub output: Output,
// }


pub trait Backend {
    /// Backend responsible to delegate events into the event handler, so it needs a reference to event handler.
    /// This is problematic because backend cannot hold mutable reference to the event handler.
    fn load(&mut self) -> (&Output, &Mode);
    // fn load(&mut self) -> (&mut Backend);
    fn bind_display(&mut self, display_handle: &DisplayHandle) -> FormatSet;

    // fn renderer(&mut self) -> ;
}
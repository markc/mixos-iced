use smithay::desktop::PopupManager;
use smithay::reexports::wayland_server::protocol::wl_compositor::WlCompositor;
use smithay::reexports::wayland_server::protocol::wl_subcompositor::WlSubcompositor;
use smithay::reexports::wayland_server::{Dispatch, DisplayHandle, GlobalDispatch, Resource};
use smithay::utils::{Clock, Monotonic};
use smithay::wayland::compositor::CompositorState;
use smithay::wayland::{Dispatch2, GlobalData, GlobalDispatch2};
use crate::state::state::DispatchWire;
use protocols::compositor::state::Compositor;

pub fn new<I: DispatchWire>(display_handle: &DisplayHandle) -> Compositor
where
    I: GlobalDispatch<WlCompositor, GlobalData>
        + GlobalDispatch<WlSubcompositor, GlobalData>
        + 'static,
{
    // NOTE ON `State::new()` CALLS:
    // Every `*State::new::<Loop>(&loader.display_handle)` call below registers a specific
    // Wayland Global with the Display.
    //
    // **Calloop Interaction:** When `calloop` wakes up and accepts a new client connection,
    // the client binds to these globals. As the client sends requests to these globals over
    // the UNIX socket, `calloop` wakes up and delegates the messages to your `Loop` struct
    // via Smithay's `delegate_*!` macros (which you must define elsewhere).

    // Initialize the core compositor protocol (`wl_compositor`).
    // Side-effect: Clients will send `commit` requests here when they have new frames.
    // This queues an update that you process during your render loop.
    // was for Loop. Now its WireObject.
    let compositor_state = CompositorState::new_v6::<I>(&display_handle);

    let clock = Clock::<Monotonic>::new();

    Compositor {
        state: compositor_state,
        clock,
    }
}

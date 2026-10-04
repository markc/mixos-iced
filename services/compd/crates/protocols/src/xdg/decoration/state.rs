use smithay::reexports::wayland_server::DisplayHandle;
use smithay::wayland::shell::xdg::decoration::{XdgDecorationHandler, XdgDecorationState};
use smithay::wayland::shell::xdg::ToplevelSurface;

///A quick summary of how this integrates with the Compositor lifecycle:
/// Setup: Decoration::new puts the capability onto the Wayland Display.
///
/// Event Loop (calloop): When the calloop wakes up due to readable data on the Wayland UNIX socket, Smithay parses the incoming bytes.
///
/// Routing: If a client sends a message to the zxdg_decoration global, Smithay uses your delegate_xdg_decoration! macro to route that specific message into the trait methods (new_decoration, request_mode, etc.).
///
/// Action: Inside those methods, send_pending_configure() pushes a response back into the Wayland output buffer.
///
/// Resolution: Before calloop goes back to sleep, it flushes the Wayland output buffer back to the client, causing the client to redraw itself without its own window borders.

/// This module broadcasts the availability of server-side decorations
/// so that Wayland clients will not attempt to draw their own decorations (borders, topbars).
///
/// In Wayland, "Client-Side Decorations" (CSD) means the app draws its own borders.
/// "Server-Side Decorations" (SSD) means the compositor (us) draws the borders.
pub struct Decoration {
    pub xdg_state: XdgDecorationState,
}

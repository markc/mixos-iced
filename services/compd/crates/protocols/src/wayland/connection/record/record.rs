use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use smithay::wayland::compositor::CompositorClientState;

/// Data associated with a specific Wayland client that connects to your compositor.
///
/// **What is a "Client"?**
/// Wayland is an Inter-Process Communication (IPC) protocol running over a UNIX socket.
/// Every time a new application (like Firefox or a terminal) connects to that socket,
/// the `wayland-server` backend creates a new `Client` object.
///
/// This struct is your compositor's custom data payload that gets attached to that `Client`
/// object for the duration of its connection.
pub struct WaylandClientSession {
    // Smithay requires a place to store internal state (like which surfaces and regions
    // exist) on a per-client basis. You saw this being accessed in `CompositorHandler::client_compositor_state`
    // in a previous file.
    pub compositor_state: CompositorClientState,
    pub proprietary: bool
}

/// The `ClientData` trait allows you to hook into the fundamental lifecycle of the Wayland socket.
///
/// **Calloop Interaction:**
/// The underlying `wayland-server` display is registered with your `calloop` event loop.
/// When the UNIX socket receives an `accept()` call (new connection) or a `hangup`/`EOF` (disconnection),
/// the event loop wakes up and triggers these trait methods asynchronously.
impl ClientData for WaylandClientSession {
    /// Triggered immediately after a new client successfully connects to the Wayland socket
    /// and its internal data structures are initialized.
    ///
    /// **Common use-cases:**
    /// - Logging the connection.
    /// - Keeping a global count of connected apps.
    /// - Spawning specific processes or enforcing security policies (e.g., checking the PID
    ///   or UID of the connecting process).
    fn initialized(&self, _client_id: ClientId) {}

    /// Triggered when a client disconnects from the compositor.
    ///
    /// This can happen gracefully (the app was closed) or forcefully (the app crashed,
    /// causing the OS to close its end of the socket).
    ///
    /// **Side-Effect / Cleanup:**
    /// You generally don't need to manually destroy `WlSurface` or `Window` objects here.
    /// Smithay's internal destructors will automatically fire when the client disconnects,
    /// which will eventually trigger cleanup in your `Space`. However, if you are caching
    /// custom data (like specific GPU textures mapped to this specific client ID), you would
    /// free that memory here.
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

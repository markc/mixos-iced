//! testkit: compd's headless test harness.
//!
//! Runs compd's real protocol engine with no GPU, no backend and no socket:
//!
//! - **Real:** `dispatcher::Dispatch` (every smithay handler compd ships),
//!   `Wire::drain_protocol` (the outbox drain the event loop runs once per
//!   iteration), and the comp registry, `world::comp::CompState`, fed by
//!   the same `WireTrait::surface_event` calls the `Orchestrator` makes.
//! - **Stand-in:** [`TestHost`] takes the `Orchestrator`'s seat behind
//!   `WireTrait`: two window `Space`s sharing one fake 1920x1080 output, with
//!   a simulated world switch, no camera and no renderer. A window
//!   the drain hands to `place_window` waits until [`Harness::tick_frame`] maps it
//!   and reports `SurfaceEvent::Placed`, which is what frames's window hook
//!   does with the `InitialMap` it drains inside the render loop. Deferred
//!   lifecycle events use production buffer-deferral and liveness checks;
//!   activation and teardown are observable without rendering.
//!
//! Clients are real `wayland-client` connections over a `socketpair`, in the
//! same thread: [`Harness::roundtrip`] pumps both ends until the client's
//! `wl_display.sync` comes back, so a test never blocks on a socket.
//!
//! The config dir is pointed at an empty per-process directory before anything
//! reads it, so `preferences.json` defaults apply and the machine's own files
//! never leak into a test.

pub mod client;
pub mod harness;
pub mod host;

pub use client::TestClient;
pub use harness::Harness;
pub use host::TestHost;

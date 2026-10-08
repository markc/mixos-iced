//! policy-host: compd's comp policy host.
//!
//! The engine side of the `comp` Bus service, kept free of the transport: no
//! comp-service and no tokio here. compd's `CompEngine` adapter calls in
//! and hands over what only the transport knows (the port's identity and
//! counters) as plain comp-model values.
//!
//! - [`project`]: the props-tree projection (`Loop` → `CompSnapshot`);
//! - [`edges`]: the observation edge pass (`surface.*`, `focus.changed`,
//!   `props.changed`);
//! - [`truth`]: `compd.truth`, compd's own view for the Bus-truth comparator.
//!
//! - [`control`]: the control verbs compd answers (workspaces, windows) and
//!   the executor for the policy effects they return.
//! - [`input`]: `comp.input.*`, Bus-injected input on the human and agent
//!   seats.
//! - [`region`] implements `comp.region.select` and the owner-scoped
//!   `comp.region.cancel`.
//! - [`panel`]: Quoin's panel holders (`comp.panel.*`, `panel.command`, the
//!   conceal enforcement).
//! - [`xwayland`]: the persisted `xwayland.enabled` startup switch.
//! - [`x11`]: X11's side of the workspace and minimise policy (EWMH
//!   desktops, HIDDEN).

pub mod capture;
pub mod control;
pub mod edges;
pub mod geometry;
pub mod input;
pub mod panel;
pub mod project;
pub mod region;
pub mod truth;
pub mod x11;
pub mod xwayland;
pub mod worlds;

pub use edges::Edges;
pub use project::{Identity, project};
pub use truth::truth;

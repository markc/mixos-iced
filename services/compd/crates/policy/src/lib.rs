//! policy: the compositor's window policy as pure logic.
//!
//! Every decision here reads the model (surfaces's records, the
//! workspace state, small fact structs the engine fills in) and returns an
//! [`Effect`] list the engine executes in order. Nothing here touches
//! Wayland, renders, or waits: deadlines are values, never polls.
//!
//! Modules:
//! - [`workspaces`]: membership, switch/move/follow, count, the
//!   on-workspace suppression term, and the workspace half of window
//!   control;
//! - [`window`]: the `comp.window.*` decisions (`{id, generation}` fencing
//!   through surfaces, refusal codes, focus/raise/close/state/minimise/place,
//!   force-close and wait outcomes);
//! - [`corner`]: the hot-corner detector;
//! - [`panel`]: `PanelHolders` and its request semantics;
//! - [`agent`]: the engine-neutral half of the agent seat and input
//!   injection (preflight refusals, injected holds, the
//!   `comp.input.sequence` run model, `release_all`);
//! - [`pointer`]: the `comp.pointer.watch` lease;
//! - [`bindings`]: the key-binding filter and its tables;
//! - [`x11`]: the X11 decoration rule and Motif interpretation.

pub mod agent;
pub mod bindings;
pub mod corner;
pub mod effect;
pub mod panel;
pub mod pointer;
pub mod window;
pub mod workspaces;
pub mod x11;

pub use effect::Effect;

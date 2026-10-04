//! The data carried between a launch call site, the (optional) worker thread,
//! and the general `Executed` dispatch. `LaunchRequest` is built on the calloop
//! thread (the activation token is a Wayland resource) and is `Send`.

pub mod types;

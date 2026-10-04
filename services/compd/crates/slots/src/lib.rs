//! Process-wide slots: the `define_channel!`, `define_buffer!`,
//! `define_storage!` and `define_document!` macros, persisted worlds and
//! documents, input routing, child-process launch and systemd readiness.

#[macro_use]
extern crate model;

#[macro_use]
pub mod buffer;
#[macro_use]
pub mod channel;
#[macro_use]
pub mod library;
#[macro_use]
pub mod persist;
// sd_notify READY/STATUS, no libsystemd.
pub mod sdnotify;
#[macro_use]
pub mod storage;
pub mod input;
pub mod launch;
pub mod trait_;
pub mod world;

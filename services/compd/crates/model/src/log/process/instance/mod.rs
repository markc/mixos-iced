//! compositor.developer structured logging — **backend runtime**.
//!
//! One drain thread consumes the global fan-in buffer and prints each record
//! dmesg-style (`process.instance.drain`). Started once by
//! `crate::log::process::main::spawn`.
//!
//! There is no gRPC log stream: layer 0 has no tokio/tonic/prost and no
//! build-time protobuf step. The live log surface is journald (stderr) today and
//! the Bus later.

pub mod instance;
pub use instance::*;

pub mod drain;

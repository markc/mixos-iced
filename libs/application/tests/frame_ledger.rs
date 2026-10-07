// SPDX-License-Identifier: MIT OR Apache-2.0
//! Execute the native runtime's actual private correlation guards with the
//! embedding workspace's pinned dependencies. No mirrored ledger is used.
use application::iced as core;

#[path = "../../../vendor/iced/winit/src/presentation.rs"]
mod presentation;

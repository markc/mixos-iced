//! Display-output contracts: the renderer contract hosts select through, the
//! linux-dmabuf advertisement, and per-frame tearing/pacing resolution.

#[macro_use]
extern crate model;

pub mod advertise;
pub mod render_contract;
pub mod tearing;

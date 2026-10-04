//! DRM/KMS backend: udev enumeration, GPU and connector selection, EDID and
//! mode handling, scanout (planes, framebuffers, flips, fences, swapchain),
//! syncobj, libinput, and the seat/session (logind, VT) that opens the devices.

#[macro_use]
extern crate model;

pub mod connector;
pub mod device;
pub mod edid;
pub mod gbm;
pub mod gpu;
pub mod input;
pub mod loop_;
pub mod mode;
pub mod output;
pub mod scanout;
pub mod seat;
pub mod syncobj;
pub mod udev;

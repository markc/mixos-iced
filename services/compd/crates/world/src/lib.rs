//! world: the compositor engine's state and behaviour on top of smithay — the
//! windows, surfaces, seats, camera and canvas, viewports and launch flow, the
//! comp policy state ([`comp`]), and the per-frame pump that drives them.

#[macro_use]
extern crate model;

pub mod bus;
pub mod camera;
pub mod canvas;
pub mod capture;
pub mod comp;
pub mod driver;
pub mod environment;
pub mod event;
pub mod host;
pub mod kind;
pub mod launch;
pub mod notify;
pub mod order;
pub mod overlay;
pub mod pump;
pub mod scene;
pub mod seat;
pub mod smithay_glue;
pub mod state;
pub mod storage;
pub mod surface;
pub mod viewport;
pub mod window;
pub mod world;

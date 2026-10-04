//! frames: the per-frame path shared by the backends. `draw` plans a frame as
//! an ordered list of passes with tap points and presents it (frame callbacks,
//! presentation feedback, the software cursor); `scene` assembles the render
//! elements for an output (windows, layers, background, effects); `hook`
//! holds the window and surface hooks the engine calls on map, commit and move.

#[macro_use]
extern crate model;

pub mod draw;
pub mod hook;
pub mod scene;

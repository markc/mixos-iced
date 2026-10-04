//! scenegraph: the renderer-agnostic scene model. `node` is the draw currency
//! (`DrawNode`, lowered to a render element in one place), `scene` the element
//! types the backends composite, `display` the output and backend records,
//! `state` the engine lifecycle (initialisation and the input/present loop).

#[macro_use]
extern crate model;

pub mod display;
pub mod node;
pub mod scene;
pub mod state;

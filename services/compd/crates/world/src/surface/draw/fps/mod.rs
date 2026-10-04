//! FPS overlay: a small, solid-black, top-right SCREEN-space iced surface
//! showing the current composited-frame rate.
//!
//! It is click-through (passthrough) so it never steals pointer input, and it
//! is pushed a new value only when the shown number changes, so an idle overlay
//! costs nothing per frame.

// Developer logging: bring error!/warn!/info!/trace!/abort! into scope.

pub mod fps;

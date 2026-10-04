//! decor: server-side window chrome (SSD) for compd.
//!
//! compd draws its chrome as scene elements. The decoration engine is
//! [`layout`]: the three styles (mac / win11 / mixos), the layout of a
//! window's frame around its content, and hit-testing. The rest of the crate
//! builds around it:
//!
//! - [`theme`]: the style preset for the shape, the design tokens for
//!   every colour (the desktop-wide theming mandate);
//! - [`state`]: per-window chrome state whose commit moves only when what the
//!   chrome shows changes, so chrome never repaints on its own;
//! - [`input`]: presses and releases on chrome as the intents a client's own
//!   requests produce (move, resize, close, maximise, minimise);
//! - [`window`]: which windows get chrome (they negotiated server-side) and the
//!   room it takes (a maximised window's content leaves it on the output).
//!
//! - [`raster`] + [`text`]: the titlebar band and the shadow tile, rasterised
//!   on the CPU (signed-distance coverage; the title through cosmic-text);
//! - [`render`]: those images as scene elements per window, re-rasterised
//!   only when the window's chrome state, the theme or the scale changes.

#[macro_use]
extern crate model;

pub mod clip;
pub mod input;
pub mod layout;
pub mod raster;
pub mod render;
pub mod seat;
pub mod state;
pub mod text;
pub mod theme;
pub mod window;

pub use layout::{
    CaptionButton, ChromeLayout, ChromePart, ChromeStyle, DecoExtents, ResizeEdge, Srgba,
};
pub use input::{Clicks, Intent};
pub use state::{Chrome, ChromeState};
pub use theme::{ChromeTheme, Pair, Palette, TokenSource};

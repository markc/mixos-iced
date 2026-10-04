//! Protocol-level state and logic shared by the dispatch layer: per-surface and
//! per-window records, grabs, redraw scheduling, seat and session state, and the
//! X11 window model, written against smithay without depending on the world.

#[macro_use]
extern crate model;

pub mod clipboard;
pub mod compositor;
pub mod cursor;
pub mod dispatch;
pub mod dmabuf;
pub mod dnd;
pub mod ephemeral;
pub mod foreign;
pub mod fractional;
pub mod grab;
pub mod layershell;
pub mod output;
pub mod popup;
pub mod presentation;
pub mod redraw;
pub mod seat;
pub mod session;
pub mod shm;
pub mod singlepixel;
pub mod space;
pub mod tearing;
pub mod text;
pub mod viewporter;
pub mod wayland;
pub mod window;
pub mod xdg;
pub mod xwm;

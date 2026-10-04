//! Input handling for the compositor seat: keyboard, pointer, touch and tablet
//! delivery, modifier handling, lid policy, and Bus-injected input.

#[macro_use]
extern crate model;

pub mod delegate;
pub mod inject;
pub mod input;
pub mod keyboard;
pub mod lid;
pub mod modifier;
pub mod pointer;

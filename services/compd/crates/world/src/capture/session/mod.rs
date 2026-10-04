//! Window-capture session state.
//!
//! Pure data: the capture phase state machine ([`session`]) and the message
//! type ([`message`]) shared between the iced overlay UIs, the surface message
//! channel, and the capture interface. No rendering, no Wayland, no `Loop`
//! dependency — those live in `recorder::interface`.

// Developer logging: bring error!/warn!/info!/trace!/abort! into scope for every module in
// this crate. (Drop this line if the crate genuinely never logs.)

pub mod message;
pub mod session;

//! The output-presence lifecycle event. Orchestration is its sole sender; world
//! systems self-subscribe via `builder.receive(&OUTPUT_CHANGED, …)` and react
//! however they like (e.g. capture stops when the display goes away). Mirrors
//! `launch.broadcast` — channel + a `broadcast(router, change)` so this crate
//! stays below `orchestration.core` (it takes a router, not the whole `Loop`).

pub mod output;

//! ledger: compd's frame, presentation and redraw-reason ledgers.
//!
//! Engine-neutral cores; they later merge into `frames`:
//!
//! - [`frame_trace`]: the opt-in, bounded span/event timeline on
//!   CLOCK_MONOTONIC;
//! - [`presentation_stats`]: per-window and per-output intervals, latency
//!   rings and the missed-vblank rule;
//! - [`presentation`]: the `wp_presentation` feedback ledger, generic over
//!   the feedback object, in which every feedback is answered exactly once,
//!   presented or discarded;
//! - [`redraw`]: the `RedrawReason` enum and the per-reason frame ledger.

pub mod frame_trace;
pub mod presentation;
pub mod presentation_stats;
pub mod redraw;

pub use surfaces::{SeatKind, SurfaceId};

//! The unified executor: self-spawn for a reliable PID, then apply the
//! configured backend. Backend- and dispatch-agnostic — runs either inline on
//! the calloop thread or on the launch worker.

// Developer logging: error!/warn!/info!/trace!/abort! in scope for this crate.

pub mod execute;

//! Adopt an already-running PID into a transient systemd `.scope` over the user
//! session bus. Delegates cgroup placement / resource control to systemd
//! WITHOUT taking the PID away from us: a scope (unlike a service) keeps the
//! compositor as the process parent, so reaping is still our job.

// Developer logging: bring error!/warn!/info!/trace!/abort! into scope for every module in
// this crate.

pub mod scope;
pub mod containment;

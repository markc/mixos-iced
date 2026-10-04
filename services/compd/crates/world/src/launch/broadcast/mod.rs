//! The general per-world `Executed` launch event. Orchestration is its sole
//! sender (point: the broadcast originates from orchestration); the executor
//! driver stays bus-unaware. World systems (placeholder, …) self-subscribe via
//! `builder.receive(&EXECUTED, …)` and route by correlation.

pub mod broadcast;

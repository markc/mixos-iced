// One buffer-boundary persist call for a system: its single-value slots (engine
// `sync`, PartialEq-gated, off-thread write) and its table collections (document
// `sync_documents`, per-record put/delete/re-link). Keeps the world flow's hook a
// single call.
pub mod base;

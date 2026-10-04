// The transaction/commit registry: rim or system code wraps a persisted mutation
// with `transact` (or calls `mark_world`), flagging a world for an end-of-frame
// commit. `immediate` commits at the next frame-end; otherwise the commit is
// debounced — batched up to a maximum of 1 second so frequently-changing state
// (placeholder transforms, sampler updates) doesn't spam the disk. The frame loop
// drains the due worlds via `due_worlds`; there is NO periodic poll of storage.
pub mod base;

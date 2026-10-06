# Microsoft Edit patches

Upstream: `microsoft/edit`, revision
`826b4c097b6f14ba0a846dc56f2f0223a3aaf73a`, MIT, Microsoft Corporation.
Imported through frozen cosmix revision
`e0297242305f3a3c3de09f1ca01e8faa771768da`.

The inherited buffer changes make memory commit failures all-or-nothing,
replace mutable SIMD dispatch statics with OnceLock, carry ambiguous Unicode
width per measurement and expose grapheme boundaries. The owned gap-buffer
reservation is Send, with a compile-time assertion at the public buffer API.
See `buffer/README.md` for the complete retained patch log and upstream paths.

The syntax crates retain thread-local scratch arenas, OnceLock memset
dispatch, deterministic register allocation and scoped architecture support.
See `README.md` for their upstream paths and patch log.

On entry, module files move from `foo/mod.rs` to `foo.rs`, manifests use plain
distinct package names and pinned dependencies, and the adapter references
this single global vendor directory. Those layout changes add no alternate
buffer or highlighter implementation.

External path modules declare their child file paths explicitly, including
the extracted stdext module, so the same buffer compiles from its global
vendor location without `mod.rs` files or duplicate implementations.

Guards: the retained allocation-failure and Unicode buffer tests, property
and OT convergence tests, deterministic generator and committed-definition
freshness tests. A refresh must preserve all of them.

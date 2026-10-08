# Compd testkit backing lifecycle acceptance

`examples/backing_lifecycle.rs` exercises the real `IcedSurface` and
`Ring<Backing>` using a hardware render-node GBM/EGL display and the production shared GLES/wgpu
context. Build `cargo build -p testkit --example backing_lifecycle` on the
designated build worker. Run the built fixture on an owned hardware render node
through `tests/desktop/settings_backing_lifecycle_gate.mix`, with four arguments:
the exact source checkout, built fixture binary, discovered render-node path and
a fresh artifact directory. Its parent directory must already exist.

The requested hardware device is discovered through EGL and its render path is
canonicalised. Display construction then uses the native compositor's GBM/EGL
platform and renderer factory, retaining the owned render fd for the whole
context lifetime. Full fixture process output and its result are saved beside
the requested artifact directory before a native failure is raised.

The gate bounds the owned process to 45 seconds. GPU completion waits in the
fixture are bounded to five seconds; production ring calls also run inside the
process deadline. No VT, seat, window, Bus endpoint or production service is
created. Missing hardware EGL support fails the gate; software devices are
refused.

Each phase writes every actual published GLES pixel to a fresh `.rgba` file and
its matching slot, texture, generation and immutable frame stamp to JSON. The
fixture compares every pixel against a distinct wgpu render-pass clear, and
compares the exact `FrameBinding` observer owner beside those pixels. Completion
comes from the real wgpu queue callback and device poll. It never calls the
binding observer or fabricates a native presented callback.

Coverage includes real backing rotation at depths two and three, growth,
shrink through three/two/one while preserving the completed pixel/binding pair,
and fresh writes after resize/reset and release/ensure. A new target must start
without a retired binding. GL texture names may legitimately be reused after
release; correctness depends on fresh allocation and binding retirement, not a
globally unique numeric name. Raw artifact hashes, adapter identification,
source/tree and fixture binary hashes are recorded by the gate.

This deliberately calls the existing `sync_depth` API. Ordinary GLES UI policy
continues to use depth one; changing a preference is insufficient to exercise
the backing ring. This gate proves shared-device pixel/metadata ownership, not
Wayland/KMS presentation, VT readiness, or performance latency.

The normal testkit library remains headless. GPU dependencies belong only to
the explicit example's dev-dependency graph. Strict Clippy checks the primary
testkit library and example with `--no-deps`; existing graphics-library warning
debt is recorded separately and is not waived by this fixture's acceptance.

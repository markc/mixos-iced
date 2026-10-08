# Graphics backing lifecycle acceptance

`examples/backing_lifecycle.rs` exercises the real `IcedSurface` and
`Ring<Backing>` using a hardware EGL device and the production shared GLES/wgpu
context. Build `cargo build -p graphics --example backing_lifecycle` on the
designated build worker. Run the built fixture on an owned hardware render node
through `tests/desktop/settings_backing_lifecycle_gate.mix`, with four arguments:
the exact source checkout, built fixture binary, discovered render-node path and
a fresh artifact directory. Its parent directory must already exist.

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

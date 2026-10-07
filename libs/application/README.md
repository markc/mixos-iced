# Application

Shared native application hosting for Ced, DOpus and Term. `start` takes the
initial state and boot task exactly once, selects a single asynchronous task
worker and applies a `Window` configuration. The returned builder accepts the
application's title, subscription, theme and style before entering `.run()`.

CPU, image, GPU and raster diagnostic support are selected with the
`tiny-skia`, `image`, `wgpu` and `raster-probe` features. The `iced`, `cpu` and
`runtime` namespaces expose the selected host interfaces to app adapters.
Applications do not depend directly on the iced crates.

The host does not own Bus connections, editor buffers, filesystem operations,
PTYs or their shutdown. Applications retain those lifetimes. Portable compound
widgets live in toolkit and receive neutral models and typed actions.

The opt-in `settings` feature adds `presentation::Host`: one existing settings
consumer plus an immutable toolkit appearance and optional deliberate content
styling. Capture `host.request()` on the UI loop, run its preparation on the
existing host worker, then call `host.complete(result)` on the UI loop. Completion
checks the original consumer/stage fence immediately before swapping the whole
presentation and acknowledging it. Preparation/resource/content errors keep the
last applied presentation; superseded or foreign completions cannot swap it.
Repeated capture of the same pending stage returns no new request. Replace or
cancel the host's one resource job when a fresh stage supersedes its capture.

Feed native events and RPC completions to `host.consumer_mut()` using the
application's existing Bus lifetime. The bridge creates no connection, receiver,
runtime, timer or redraw loop. Its returned shared change plan lets the host
request the required invalidation/redraw; unchanged revision evidence creates
no new preparation. Applied evidence is separate from frame presentation.

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

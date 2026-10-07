# BusViewer

Native ABP service browser and verb caller using the shared application host and toolkit. See [the manual](../../docs/busviewer.md) and [port techniques](../../docs/dev/bevy-to-iced.md).

`cargo build --locked --profile release-fast -p busviewer`

`cargo test --locked -p busviewer`

The app owns its native Bus lifecycle and plain discovery model. Shared widgets remain domain-independent. Cosmix source provenance is recorded in the manual and model header.

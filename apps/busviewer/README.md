# BusViewer

Native ABP service browser and verb caller using the shared application host and toolkit. See [the manual](../../docs/busviewer.md) and [port techniques](../../docs/dev/bevy-to-iced.md).

`cargo build --locked --profile release-fast -p busviewer`

`cargo test --locked -p busviewer`

The ignored `native` integration test requires a dedicated broker and
`BUSVIEWER_TEST_URL`, `BUSVIEWER_TEST_MIX` (the tested Mix executable) and
`BUSVIEWER_TEST_RESTART` (a Mix script that restarts that broker at the same
URL). Run it with `cargo test --locked -p busviewer --test native -- --ignored`.
The restart fixture must supervise only its isolated test broker. The test
checks discovery, partial failures, raw replies, registry events, reconnect
without mutation replay, and flushing a quit acknowledgement before close.

The app owns its native Bus lifecycle and plain discovery model. Shared widgets remain domain-independent. Cosmix source provenance is recorded in the manual and model header.

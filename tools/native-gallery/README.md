# Native toolkit gallery

Developer-only two-window gallery and native DnD acceptance client. This
host adapts the toolkit's neutral session API to the local iced/winit
Wayland backend. The toolkit itself remains usable with pristine iced.

Build `cargo build -p native-gallery --locked`, then run `native-gallery`.
Both windows contain the ordinary widget gallery. Drag the text strip at
the top of the source onto the target. Copy/Move completion comes from the
native data-device protocol; a Move removes the source only after success.

The desktop gate uses an isolated compositor and its primary-seat Bus
input API. `--trace PATH` writes bounded JSON evidence for the gate.
`--role source|target` uses separate processes; `--action copy|move` chooses
the target's preferred action. Other switches exercise rejection,
cancellation, closure, stale presses and payloads exceeding pipe capacity.

The desktop gate also checks overlapping dropped offers, cancellation followed
by a fresh drag, and a receiver disconnecting before acknowledging a Move.
`--ack-count 2` deliberately holds the first acknowledgement for those cases.
The source retains its payload until the protocol reports a successful Move.

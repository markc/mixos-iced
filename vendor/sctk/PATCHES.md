# SCTK dropped-offer ownership

Upstream: smithay-client-toolkit 0.19.2 from crates.io, VCS base
`c583de8dd5651f8168c6513cd282137c42aae049`. MIT licence retained.
The manifest comes from the release's Cargo.toml.orig, with an isolated
workspace boundary; the release's sources and examples are retained.

`DataDeviceData::take_dropped_offer` transfers a dropped native offer out of
the active slot without destroying it. The existing dispatcher otherwise
destroys that slot on every new Enter, even when an earlier dropped offer
is still transferring or waiting for its receiver's acknowledgement.
Undropped offers remain owned by SCTK and keep their existing Leave cleanup.
The winit adapter takes the dropped offer in its Drop callback and owns its
explicit finish/destroy lifecycle thereafter.

Guard: the native toolkit desktop gate's overlapping-transfer case starts
a second drag while the target holds the first acknowledgement, then verifies
both payloads and successful source completions.

# winit Wayland drag and drop

Upstream: `https://github.com/iced-rs/winit.git`, exact base
`05b8ff17a06562f0a10bb46e6eaacbe2a95cb5ed` (0.30.8).
The upstream source tree and Apache-2.0 licence are retained. The four
CC-BY-SA documentation key figures are replaced by original permissively
licensed vectors; their file attribution is in docs/res/ATTRIBUTION.md.

Local changes add a native source/offer lifecycle to the existing SCTK
data-device backend. No second connection, dispatch thread or transfer
transport is introduced. `drag` defines bounded MIME payloads, action
negotiation, gesture/offer identities and completion events.

`WindowExtWayland` queues requests onto the existing event-loop wakeup.
Pointer presses retain their own seat, origin surface and serial; a start
consumes one still-held press belonging to the requesting window. Releases
invalidate unused presses. The backend never uses the global latest-button
serial as a drag authorisation.

Data-device callbacks maintain one offer record per enter, and retain a
dropped offer until delivery and explicit completion. Nonblocking native
pipes are registered with calloop for read/write readiness. Reads deliver
once at EOF with a 16 MiB cap; writes handle partial progress. Only
`dnd_finished` publishes successful source completion. Window/seat removal,
rejection and cancellation retire resources.

The SCTK source factory sets actions once; its 0.19.2 `DragSource::set_actions`
helper is deliberately unused because it sends a duplicate protocol request.
`rustix` gains its `fs` feature solely for nonblocking FD flags.

Guards: backend unit tests for payload validation and the desktop native
drag test exercise MIME/action negotiation, bytes, exactly-once completion,
invalid presses, cancellation, closures and transfer sizes above pipe capacity.

Wayland text-input commits are counted per object. Done batches are accepted
only inside the latest enable context, so synthetic Disabled cannot expose
old composition to a replacement field. Older cursor/content commits within
the same context still apply as the protocol requires. Pending batches are
cleared at enable/disable/leave. Guards: `epoch_tests` covers stale Done after
replacement, in-context updates and serial wrap; all commit sites are tracked.
Queued native IME events retain an epoch ticket and revalidate it individually
at delivery, including after sink append. A threaded disable therefore also
retires batches already queued by Done. `epoch_delivery_tests` covers that
ordering and a context change between two events of one queued batch.

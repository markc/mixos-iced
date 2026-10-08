# Native participant observation

Status: additive source checkpoint; native acceptance pending.

The canonical `application.describe.v1` envelope remains unchanged. Native GUI
owners add `settings_observation`, `native_frames` and `installed_frame_stamp`.
Quoin adds `native_scene_frames`, one bounded entry per actual scene surface,
with separate current installed state. These are optional capability evidence,
not required identity fields. Headless owners do not claim native frames.

`settings_observation` records the first native delivery and actual resource ACK
for each snapshot identity and connection generation. Each point carries an
optional boot identity, CLOCK_MONOTONIC identifier and nanoseconds. Reading
description performs no I/O, activation, ACK, rendering or timestamp refresh.
Delivery is not durable acceptance. Application ACK is not presentation.

`native_frames` exposes the real frame owner's bounded historical receipts.
Their immutable stamp, native request, actual window incarnation and native
clock remain attached to the submitted buffer. `installed_frame_stamp` is
current preparation state and must never be substituted into a historical
receipt. Quoin captures the binding of the actual published GPU ring slot,
filters actual renderer visibility, and delivers a receipt only after a real
backend presentation. Nested swap evidence is distinct from physical KMS/VT
evidence. Hidden buffers and offscreen captures do not certify presentation.

The shared `application::participants` registry is a bounded pure observation
adapter. Existing native owners supply service/process/session/surface identity,
registration and retirement, compositor visibility, accepted operation identity,
actual ACK and existing frame handle. It opens no transport, timer or authority.
Its phases are pending, accepted, applied and presented. Its states distinguish
hidden, minimised, inactive session, closed, unsupported and nonresponsive within
an explicit observation deadline. A deadline is not a diagnosis that a process
is hung. Inactive physical VT classification requires real seat evidence.

A participant generation retires held callbacks but a stable retained view
captures fresh callback provenance for each new native request. Neither a
reconnection nor an unchanged confirmation invents a new presentation stamp.
Latency is available only for exact operation identity, installed view stamp,
actual native receipt and matching boot/clock. Foreign or absent clocks and
history lost on restart are unavailable. Quantiles require measured samples;
sequential observation deadlines are only upper bounds.

This checkpoint supplies the production seams and pure boundary tests. Wiring
the participant registry to native registration/retirement topics and the
simultaneous seven-owner native matrix remain acceptance work. No polling or
parallel authority is authorised by this observation contract.

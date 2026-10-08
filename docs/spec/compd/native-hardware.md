# Native hardware observations

Status: accepted additive contract, compd 0.1.18. ABP wire unchanged.

`comp.hardware.snapshot {}` returns the actual compositor `instance`, `native`
(true only on KMS), monotonic `sequence`, current `session_active` from the
existing session owner, and `events` containing keyboard, pointer, paused and
active rows or null. Each row contains its sequence and optional kernel input
device sysname. Only the actual native libinput callback records keyboard and
pointer events after routing through the normal input pipeline. Nested input
and Bus injection cannot advance these observations. No key values are stored.

Paused/active rows record the actual libseat callback and status transition;
they do not certify that every resume step or subsequent scanout succeeded.
Consumers must separately require genuine output/window presentation after
return. Input observations certify callback arrival and pipeline routing, not
that an app consumed the event. Physical acceptance must also match the device
to captured hardware identity and inspect the resulting actual product state.
Virtual/uinput devices must not be labelled physical from these rows alone.

`comp.hardware.wait {instance, after, until, timeout_ms?}` accepts a nonempty
instance (at most 128 bytes), unsigned sequence, and until keyboard, pointer,
paused or active. Timeout defaults to 20000 ms and accepts 1 through 60000 ms.
It returns the latest matching actual event strictly newer than after, plus
instance, until and waited_ms. Events can occur between snapshot and admission:
the retained matching row closes that race without replaying a previous edge.
An after sequence ahead of the current owner refuses invalid_sequence;
another incarnation refuses stale_instance; nested waits refuse
unsupported_backend. An unmet deadline refuses timeout. Only an edge observed
by the owner's monotonic clock at or before the admitted deadline can satisfy
the wait; late event-loop settlement does not make a later edge succeed.

Waits retain the original native command reply in the existing bounded long
operation pool. The existing event-loop deadline timer only wakes settlement;
no snapshot poller, extra service, key log or transport is introduced. Storage
is four latest rows, with device sysnames preserved exactly when they are
`event` followed by ASCII digits and at most 128 bytes. Malformed or overlong
sysnames produce a null device identity while preserving the observed edge
and sequence; prefixes are never substituted for identities. Sequence
exhaustion freezes observations instead of wrapping into replay.

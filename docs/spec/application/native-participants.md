# Native participant observations

The existing scene-host registry subscriber owns `application.participants.v1`.
Its `app.describe` extension `participants` and retained
`<service>.participants.changed` topic contain the same copied owner record.
Observing a record does not request a redraw, install settings or set authority.

External application records arrive on owner-protected
`<application-service>.presentation.changed`, contract
`application.presentation.v1`. The local broker supplies the registration
incarnation and native Unix principal. The receiver requires explicit local
origin, the current registration incarnation and payload PID equal to the
actual Unix peer PID. It joins exactly one actual native window with that PID;
ambiguous multi-window associations are unavailable. The native window ID and
generation are separate from the producer's frame window and connection
generation. Same-name replacement retires old observation ownership even if
the name list never changed. Registry history and pending notice maps are
bounded; retired callbacks cannot regain admission after tombstone eviction.

Each actual FrameHandle mints an immutable `owner` identity before its first
callback. The authenticated registration/PID and this identity admit one native
window incarnation. Retirement keeps its last request baseline and rejects the
old owner even when a later callback has a greater request number. A same-PID
replacement needs a fresh actual Handle association; copied old pixels cannot
be rebound by a new sole-PID match. A pending first callback remains unavailable
for presentation. Associations are bounded to 128 and remain tombstones until
registration retirement; exhaustion refuses new association instead of evicting
history and reviving old callbacks. Participant rows expose `frame_owner`.

Presentation topic reconciliation is serial and coalesces desired registry
state. Sent subscriptions, including ones whose reply was lost, remain owned
until an unsubscribe acknowledgement. A new registry event cannot cancel a
pending removal. The possible subscription set is bounded to 128 and reset on
actual broker generation retirement; an old acknowledgement cannot remove a
new generation's subscription.

Quoin scene records use the compositor's actual per-scene FrameHandles. Queued
pixel stamps live on graphics Backings and survive ring moves. A replacement or
released Backing clears its metadata. A generation change preserves historical
receipt provenance, but old receipts cannot certify a current generation.

Each row contains service, PID, registration incarnation, connection generation,
native-window identity, frame-window identity and surface incarnation; current
and applied snapshot identities; phase, state, resource evidence and optional
actual presentation receipt. Visible, hidden, minimised, inactive session and
closed states remain distinct. A hidden or inactive buffer is never presented
merely because settings were applied. A physical VT claim additionally needs a
real native seat transition; nested session state is not physical proof.

`app.participants.wait` accepts exactly:

```json
{"operation_id":"changed-operation","services":["term","shell"],"timeout_ms":1000}
```

Operation ID is nonempty and at most 128 bytes. Service names are unique,
nonempty and at most 256 bytes; there are 1–128 services. Timeout is 1–60000 ms.
The bounded request uses owner watch changes and one absolute deadline from
admission, with lifecycle/generation cancellation. It returns
`application.participants.wait.v1`, operation ID, expected services,
`deadline_elapsed` and `observations`. Completion requires every requested
service to have actual matching changed-operation records in presented or
explicit nonvisible states, or a real closed record. On expiry, incomplete
eligible live rows become nonresponsive while their actual phase remains
unchanged. Missing rows remain missing, not manufactured participants.

On-time completion requires the selected phase to have actually been observed
by the owner no later than the admission deadline. A predeadline receipt may
settle later and still succeed; a receipt first observed after the deadline
cannot succeed on time. The owner retains up to 128 meaningful phase changes
with private monotonic observation times, independent of authority acceptance
times. Exhausted history refuses with `PARTICIPANT_HISTORY_GAP`; it never
invents a missing earlier receipt. Repeated unchanged frames do not replace the
first observation of the same phase and stamp.
History retains that first physical receipt and its presentation latency for
wait replies and operation samples. The inspector and retained changed topic
continue to show the latest actual physical receipt; their
`accepted_to_presented_ns` is the interval to that latest frame, an upper bound
on first-presentation latency when ordinary frames continue. Changes to
visibility, operation, stamp or incarnation still create new history records.

Optional `until` is `settled` (default), `presented` or `closed`. `closed`
requires actual retirement of every selected owner. `presented` requires
actual matching operation presentation for every selected row; hidden,
minimised, inactive and closed rows cannot complete it. Optional `keys` is a
unique list of 1–128 exact participant keys, at most 512 bytes each. Every key
must exist and each requested service must have a selected row. This selects
one real Quoin scene without claiming that its hidden sibling scenes presented.
The reply echoes the selected keys and criterion.

### Preparation failure (wait contract 0.2.0)

The additive `until:"preparation_failed"` criterion observes a failed current
authority preparation. It does not classify a transport error, hidden buffer or
presentation deadline as a preparation failure. Existing criteria are unchanged.
The request requires exact `keys` and an `owners` object with precisely those
keys. Each value contains PID, native connection generation, registration and
surface incarnation, nullable native-window identity and nullable frame owner:
The two nullable fields may be omitted, with the same meaning as `null`; all
other owner fields are required. An absent handle cannot match a live handle.

```json
{"operation_id":"failed-operation","services":["shell"],"keys":["shell/control"],"owners":{"shell/control":{"pid":42,"connection_generation":7,"registration_incarnation":"native-registration","surface_incarnation":3,"native_window":null,"frame_owner":null}},"until":"preparation_failed","timeout_ms":1000}
```

Every selected record must retain that exact live owner, the requested accepted
operation and an actual typed `preparation_failure` whose `identity` equals its
current accepted snapshot. Closed, replaced, stale-operation and unrelated
local-context faults cannot complete the wait. Failure evidence contains
`identity`, settings consumer `generation` and `fault:{code,path,message}`.
The consumer generation is distinct from the native owner connection generation.
Cold owners may have no applied data; callers testing recovery additionally
verify their exact prior applied identity, resources and retained product state.

The shared consumer captures failure only for its current authority update and
clears it on supersession, disconnection or success. Existing native application
publications carry it in `settings.preparation_failure`; Quoin supplies the same
consumer evidence through its existing owner observation. Participant changes
retain the field and wake the existing finite native wait. No polling, extra
connection, authority or redraw is introduced. The reply retains the v1 envelope
and echoes `until:"preparation_failed"`; unsupported implementations refuse the
new criterion rather than silently treating it as settled.

Authority observation uses the optional bounded `settings_observation` header
on the existing protected settings snapshot topic. The snapshot body, digest
and schema are unchanged. New durable changed operations may carry original
acceptance timing; no-op, old replay, lookup and restart do not invent it.
Latency requires matching operation identity, installed stamp and actual frame
receipt, plus authenticated local source and equal boot/clock identities.
Original settings delivery must itself carry local broker origin and a valid
local Unix/session-bound principal. Remote operation identity remains valid
control evidence, but its comparable timing fields are cleared before a local
application can republish them. Matching JSON boot/clock fields alone cannot
make a remote authority acceptance time local.
Unavailable timing is null. Hidden, inactive and closed rows never supply new
presentation samples. Quantiles must use real monotonic samples and report
sample count, environment and estimator; sequential observation bounds are
separate evidence.

This contract is additive. Source implementation alone is not native or KMS
acceptance; the exact published candidate must pass broker, lifecycle and
seven-owner runtime gates.

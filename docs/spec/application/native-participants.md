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

Optional `until` is `settled` (default), `presented` or `closed`. `closed`
requires actual retirement of every selected owner. `presented` requires
actual matching operation presentation for every selected row; hidden,
minimised, inactive and closed rows cannot complete it. Optional `keys` is a
unique list of 1–128 exact participant keys, at most 512 bytes each. Every key
must exist and each requested service must have a selected row. This selects
one real Quoin scene without claiming that its hidden sibling scenes presented.
The reply echoes the selected keys and criterion.

Authority observation uses the optional bounded `settings_observation` header
on the existing protected settings snapshot topic. The snapshot body, digest
and schema are unchanged. New durable changed operations may carry original
acceptance timing; no-op, old replay, lookup and restart do not invent it.
Latency requires matching operation identity, installed stamp and actual frame
receipt, plus authenticated local source and equal boot/clock identities.
Unavailable timing is null. Hidden, inactive and closed rows never supply new
presentation samples. Quantiles must use real monotonic samples and report
sample count, environment and estimator; sequential observation bounds are
separate evidence.

This contract is additive. Source implementation alone is not native or KMS
acceptance; the exact published candidate must pass broker, lifecycle and
seven-owner runtime gates.

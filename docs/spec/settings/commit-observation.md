# Runtime commit observation

Status: additive source checkpoint; authority snapshot schema remains 1.

`settings.get`, `settings.apply` and `settings.reset` may return an optional
`observation` outside the durable snapshot and receipt. It records operation ID,
exact snapshot identity, `changed`, and optional validation-start, commit-start
and accepted CLOCK_MONOTONIC points with boot and clock identity. It is retained
for only the latest actual successful durable operation in this process.

Replaying that operation returns its original observation. An older replay or
post-restart lookup has no runtime observation; current time is never assigned
as its original acceptance time. A newly durable no-op has `changed:false` and
does not imply a new application activation or native presentation. Failed
validation or storage does not replace the previous successful observation.

The snapshot bytes, digest, receipt schema and ABP framing are unchanged.
Measurements compare only exact identities and matching clocks. Missing clocks
remain unavailable. Native frame presentation and cold resource/bootstrap work
are independent observations, not inferred from commit completion.

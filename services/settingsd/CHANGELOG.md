# settingsd contract changes

## 0.1.3

Include bounded runtime authority observation in the existing protected snapshot
topic header. Replayed, restarted and unchanged operations do not acquire a new
durable acceptance or presentation time.

Declare the native settings supervision dependency explicitly so the daemon
also builds on its own, without application feature unification.

## 0.1.2

Add optional runtime commit observation outside the snapshot and durable
receipt. Preserve original timing for the latest replay; older receipts and
restart have no reconstructed acceptance time. Newly durable no-ops explicitly
report no change. Snapshot schema, digest and ABP framing stay unchanged.

## 0.1.1

Accept the optional versioned `appearance.resources` reference with structural
authority validation only: no local file, font or asset I/O, and renderer-local
availability stays a consumer preparation fault (LastGood), never authority
rollback. Whole-object change/reset only; unknown nested paths fail.
Describe/contract advertises versioned resource support; authority verbs are
unchanged and the top-level snapshot schema remains 1. Omission keeps old
accepted profile bytes and effective digests exact.

## 0.1.0

Serve describe/get/validate/apply/reset/status through native ABP; durable
changed/no-op receipts, target/revision fencing, exclusive writer, recoverable
replacement and complete scoped retained publication. Package the service unit.
Before initial release: add explicit idempotent first-run session seed, preserving
established missing-primary failure, and session wants/upholds wiring without
making GUI startup wait for authority readiness. Authority verbs/schema unchanged.
First creation requires installer-only --allow-create; the automatic unit uses
plain seed and cannot recreate a wholly lost profile directory or mount.

Before initial release: pin package source and verify stored content digest;
bind storage to the held directory/lock; fence validation; add bounded pending
publication retries and preserve missing-store failure visibility.
Fence effective compiler interpretation; distinguish intact unsupported/I/O
failures from corruption; keep pending publication backoff capped at 30 seconds
until success. Inline capacity refusal has a distinct snapshot_too_large status.

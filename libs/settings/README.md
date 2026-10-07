# settings

Headless desktop settings types, validation/resolution and shared consumer.
Shared by the authority and app/compositor adapters. Library API: 0.3.0;
authority wire/schema contract remains 0.1.0/schema 1.

`consumer::Consumer` owns ordering, bounded bootstrap buffering, one active
subscribe/read job, pending-only recovery and fenced staged/application state.
`domains::ChangePlan` compares one effective context for paint, text, layout,
resource and motion work; shell hosts opt into shell geometry. Provenance and
source/revision metadata alone do not cause rendering work.
Construct apps with `Consumer::for_app(binding, "ced")` and shell hosts with
`Consumer::for_shell(binding)`. `context()` returns the canonical projection key
(`app:ced` or `desktop`); adapters use it without repeating key construction.

The optional `native` feature executes actions over an **existing**
`bus::native_client::SupervisedClient`. It does not connect, spawn a task or take
the incoming receiver. The host multiplexes work futures with its current
incoming lane and connection state, feeding the sampled connection generation
even when a state watch coalesces a disconnect/reconnect. Feed queue loss through
`lost()` before using further queued data. There is one pending retry deadline,
from 250 ms capped at 30 seconds; remove it after success/disconnect and never
arm an idle timer. Unsupported/wrong-target/protocol refusals await a new event
or explicit refresh rather than automatically retrying unchanged bad data.
`retry_deadline()` is absolute and remains fixed during malformed-event storms;
`retry_delay()` returns its remaining duration, not a new per-event delay.
Inspect `is_confirmed()` to distinguish fresh authority evidence from preserved
usable data during loss/recovery. Captured read baselines expose same-incarnation
rollback while allowing an older read that legitimately raced a newer event.
Cancel an old executor future when `current_work()` changes or disappears;
superseded work must not become a second active host job.
Reconcile the pending timer from `retry_deadline()` after every engine input and
completion, including successful RPC replies: a delivery anomaly may have
scheduled recovery while that otherwise successful read was in flight. Timer
scheduling follows engine state, not just the last RPC result.

Each native call has a one-second bound. Pass the same initial deadline to
`native::execute_until` for subscribe and read to bound their combined bootstrap
to one second before presenting a labelled fallback;
complete cancelled work as a timeout so the engine can recover.
After that deadline the host may capture `fallback_request()` and run
`Request::prepare` on its worker, passing a loaded cache candidate and a resource
readiness callback. Retained bootstrap data is tried first, then cache, then
embedded defaults. Every candidate must pass the callback for all references
used by its context/host. If none is usable, preparation returns diagnostics.
`complete_fallback` fences the completion before producing the usual UI stage;
the usual synchronous pre-swap check and acknowledgement still apply.
Resource loading and real renderer integration remain host work.

Readback advances `current()`, not `applied()`. Prepare every changed resource
before entering the renderer loop. Check `is_current(update)` immediately before
the synchronous swap, then activate and `acknowledge(update)` with no intervening
await; a stale completion must never mutate the renderer. Failed activation
preserves last-good applied data. Identical render inputs advance evidence
without a swap. Neither readback nor application proves a presented frame.
Profiles/contexts are immutable for one consumer lifetime. A host rebind cancels
its old work, retires the old subscription via `unsubscribe_topic`, and creates
a new consumer; unique consumer tickets reject completions from retired hosts.

`presentation_kind()` labels installed data as Current, Retained, Cached,
LastGood or Embedded. Fallback never enters `current()` or supplies mutation
fences. Embedded/cached fallback cannot be saved as authority-derived data.
An unchanged fresh authority projection promotes evidence without a swap.
`fault()` reports authority/current-resource faults; `fallback_fault()` reports
fallback preparation/activation failure. A usable fallback clears its own fault
while preserving an ongoing authority outage or unavailable live resources.

The optional `cache` feature provides bounded presentation-cache I/O over
`config::atomic`. The caller supplies an existing absolute directory inside its
isolated session; no paths are created and no host preferences are touched.
Reads reject symlinks in every path component, non-regular files and oversized
data. A versioned, digest-bound envelope also binds instance/profile, context,
shell capability and interpretation; the whole projection must match a current
recompilation. An unavailable old pinned package source is refused visibly.
JSON parsing preserves floating-point round trips for exact projection checks.
Digests detect corruption; they do not authenticate local files. Local cache
never supplies authority evidence, even when its data is valid.

`cache_target()` captures the immutable producer/binding/context/capability
descriptor for `load_for` and `Writer::open_for`; the private producer fence
does not enter the persistent filename. `Save::same_capture` allows bounded
worker deduplication. `Request::prepare_with_cache` loads lazily after retained
resource validation and preserves load failures in its prepared diagnostics.

Capture `cache_save()` only after activation (or unchanged evidence advancement),
then submit it to the host's one serial `cache::Writer` off the UI loop. Captures
carry immutable applied data and an activation serial. A stable advisory lock
excludes duplicate writers; superseded saves cannot replace newer attempts.
The latest failed save can retry, while post-rename sync failure reports
`cache_write_ambiguous`. Do not delete corrupt/ambiguous data automatically.
Retire the writer when retiring its producer consumer. The held directory inode
is the I/O boundary; renaming it cannot redirect a write elsewhere, but may make
the cache unavailable under the new path. That is a fallback/cache miss, never
an authority rollback. Cache failure does not change applied renderer state.

See [the settings contract](../../docs/spec/settings/README.md).

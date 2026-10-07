# settings

Headless desktop settings types, validation/resolution and shared consumer.
Shared by the authority and app/compositor adapters. Library API: 0.2.0;
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
complete cancelled work as a timeout so the engine can recover. Persisted local
cache and resource/artifact preparation remain future integration work.

Readback advances `current()`, not `applied()`. Prepare every changed resource
before entering the renderer loop. Check `is_current(update)` immediately before
the synchronous swap, then activate and `acknowledge(update)` with no intervening
await; a stale completion must never mutate the renderer. Failed activation
preserves last-good applied data. Identical render inputs advance evidence
without a swap. Neither readback nor application proves a presented frame.
Profiles/contexts are immutable for one consumer lifetime. A host rebind cancels
its old work, retires the old subscription via `unsubscribe_topic`, and creates
a new consumer; unique consumer tickets reject completions from retired hosts.

See [the settings contract](../../docs/spec/settings/README.md).

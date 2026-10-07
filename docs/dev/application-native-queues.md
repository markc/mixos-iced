---
title: Native delivery queues and admission
description: Bounded retained GUI delivery and accepted-work admission for native applications, shared through application::native_queue.
---

# Native delivery queues and admission

`application::native_queue` holds two small synchronous primitives shared by
native desktop applications: `Outbox<T, SLOTS>` and `Admission` with its
`Permit`. They are renderer- and transport-neutral, require only `std`, and
carry no Bus client, runtime, actor trait or resource worker. Actors keep
their own select loops, command validation, operation execution, shutdown
sequencing and receipts.

## Why these two

Three app Bus adapters retained GUI deliveries and accepted replies with
similar code, and the similarity hid the same three mistakes: an accepted
reply FIFO could shed results it had already accepted, retained work could
strand behind a full channel with no readiness branch, and shutdown could
leave retained replies behind active jobs. One helper pair fixes the
ownership rules once:

- accepted work is bounded from acceptance to reaping, not just while a
  command is pending;
- an item is either delivered, or restored at its original queue position
  while the actor waits for readiness — it is never parked inside a
  cancellable send future;
- a closed receiver stops flushing and hands the remaining work back to the
  actor for explicit retirement.

The helper is not a delivery guarantee over a lost transport, not an actor
framework and not a replacement for per-app operation guards.

## Outbox: replaceable slots and a reliable FIFO

Classify deliveries locally before they enter the outbox, through a tiny
local enum mapped to fixed slot indices:

- **Replaceable** slot state: a unit Settings wake, a full-refresh Changed,
  the latest connection snapshot, the latest explicitly informational
  notice. Settings state is already retained in the bridge Mailbox;
  coalescing a wake does not coalesce authoritative events again.
- **Reliable** FIFO entries: accepted commands, typed initial registration
  refusals, original-argv handoff completions, operation completions the GUI
  is awaiting, any terminal receipt whose delivery is part of the host
  contract.

Never coalesce an accepted result. A ThemeApplied result may be replaceable
only when the host documents one outstanding request or a latest-selection
operation; otherwise retain each accepted result FIFO. Do not replace an
accepted handoff result with a later duplicate error, and do not share
Connected/Disconnected with a first typed NameTaken refusal in one slot.

Ordering rules:

- The first insertion gives a slot its position in the delivery order.
  Replacement updates the value without moving the marker, so continuously
  refreshed lifecycle/notice state cannot push a pending Settings wake
  indefinitely backwards.
- Reliable entries keep relative FIFO order; total retention is at most
  `reliable_capacity + SLOTS`.
- `push` returns the input unchanged when the reliable FIFO is full;
  `replace` returns the superseded value explicitly, and invalid indices
  return the input. A returned value is not load-shedding.
- `flush_with` hands items to the sender in order. `Ok(())` means the sender
  took ownership. On the first `Full` or `Closed` error the item is restored
  at its original front position and flushing stops. The helper never
  discards an item because the receiver is full or gone; `drain` is the
  actor's explicit retirement path.

Flush only after the transport reports readiness, and never hold a taken
item across an await point. Drive the sender's `poll_ready` through
`std::future::poll_fn` in the actor's select, and recover `try_send` failures
with `is_full()`/`into_inner()`:

```ignore
ready = std::future::poll_fn(|cx| gui.poll_ready(cx)),
    if !outbox.is_empty() => {
    match ready {
        Ok(()) => match outbox.flush_with(|item| match gui.try_send(item) {
            Ok(()) => Ok(()),
            Err(err) if err.is_full() => Err(SendError::Full(err.into_inner())),
            Err(err) => Err(SendError::Closed(err.into_inner())),
        }) {
            Flush::Empty | Flush::Full => {}
            Flush::Closed => { /* owned shutdown; retire via drain */ }
        },
        Err(_) => { /* receiver gone; retire via drain */ }
    }
}
```

This future borrows only the retained sender; outbox mutation happens after
readiness completes. Use one GUI sender owned by the actor; handoff tasks
return their result through the owned JoinSet instead of cloning the sender.

## Admission: bounded accepted work

`Admission::new(limit)` is synchronous, nonblocking admission, not an async
semaphore. `try_acquire` returns a `Permit` while capacity remains; the
small internal mutex is never held across host code or an await, and a
poisoned lock is recovered like `application::message::Once`.

For incoming accepted commands, use one pool of 32 initially. Store the
permit with the authoritative `IncomingCommand` in the app-local accepted
record before offering the command to the GUI. If delivery admission fails,
the command was not accepted: release that reservation deliberately and
attempt a bounded refusal. The same permit moves through:

```text
accepted record -> GUI operation pending -> retained response
                -> response task -> completed JoinSet result
                -> actor reaps and finishes
```

Never release the permit when removing the pending map entry or when merely
spawning the response. Return `(permit, response_result)` from the task so
completed-but-unreaped results stay bounded too. A panic or abort drops an
unfinished permit, counts as abandoned and is reported through the actor's
task join or receipt; an aborted permit never counts as a successful
response.

The invariant is `awaiting_gui + retained_responses +
response_tasks_including_unreaped <= limit`, however quickly the GUI answers.
A retained reply FIFO with capacity `limit` therefore cannot overflow if the
permit protocol is obeyed; an impossible overflow must preserve the item and
surface an invariant failure, never be treated as ordinary load shedding.

`Permit::finish` records an explicitly handled terminal outcome — a failed
response is finished after its failure is recorded. It does not claim a
successful reply, log private command data or perform transport work; the
actor retains the actual error or retirement reason in its receipt.

Keep a separate bounded execution limit for concurrently running work, and a
separate small refusal budget so BUSY noise cannot consume all
accepted-response execution slots. Existing Tokio owned semaphore permits are
adequate for concurrent execution; do not duplicate them with another shared
scheduler. For outgoing GUI calls, acquire the admission permit before
enqueueing the effect and retain it through task reaping, so effects waiting
for the worker are bounded as well as active tasks. If a job must later
produce a reliable GUI completion, retain an output credit until the
completion transfers to the bounded GUI channel; an app-local envelope can
carry that permit beside the delivery while in the outbox.

## Capacity notes

`Outbox` bounds retained deliveries; `Admission` bounds accepted work. Both
are claims the actor enforces, and neither is a global channel proof:

- futures mpsc has per-sender capacity semantics, so `channel(64)` must not
  be described as a literal global 64-item bound independent of sender
  count. Test the actual bound; the shared acceptance and outbox limits
  supply the relevant workload bound.
- A synchronous UI reply method must not silently `try_send` into an
  arbitrarily full general effect channel. Keep reply/control traffic
  separate from outbound calls, and enforce a bound on producers: at most
  one consumable reply per accepted command, one handoff and one Quit. A
  take-once reply capability (such as `application::message::Once` carrying
  a non-Clone capability through Clone GUI messages) or a bounded dedicated
  control channel with explicit retained reply ownership is required; an
  unbounded channel plus a comment about normal callers is not a capacity
  proof.
- An existing unbounded fault `Vec<String>` also needs a fixed sample cap
  plus counters if prolonged transport failure is part of the boundedness
  claim; preserve individual diagnostics up to the cap and never present a
  capped sample as the total fault count.

## Origin, deadlines and cancellation

The helper is deliberately ignorant of protocols. Host accepted records
retain the actual `IncomingCommand`, its original generation and command
identity; check the frame's origin against the live generation before
accepting, before queued GUI mutation and before invoking a deferred Bus
mutation. Never relabel a GUI command with the current generation, and use
monotonic nonreused command ids for the worker lifetime.

For an outgoing call, capture `deadline = Instant::now() + limit` at the
public Handle call entrance. Include queue admission, channel wait,
work-slot wait, transport call and response in that absolute budget. If it
expires before transport invocation, return NotSent and do not send later;
once the transport may have sent, a timeout is outcome-unknown unless the
owning Bus error explicitly proves otherwise, and must not be replayed.

## Shutdown

`TaskSet::abort_and_report` freezes admission, requests cancellation and returns
only immediately available outcomes. Its `unconfirmed` count means future
destruction has not been observed. Dropping the set requests abort; it cannot
stop non-yielding code or a running blocking operation. Finish credits only for
recorded outcomes; remaining credits remain active until actually abandoned.
Check the common deadline before submitting another reply or starting a queued
cache save. A Bus stop signal may still be attempted at expiry, with incomplete
close reported. Term has one two-second lane drain and a separate runtime
teardown allowance capped at 100 ms.

Native Bus actors can use `application::native_actor` with the existing
`settings-native` or `acceptance` feature. `TaskSet<T>` bounds running and
completed but unreaped tasks, returning each successful task's `Permit` in
`Completed<T>` for explicit host retirement. `try_spawn_with` checks capacity
before invoking its factory and returns the original item on Full. Abort and
panic count unfinished permits as abandoned; the task slot remains until reaped.

`Accepted` retains the receiving supervisor, command and admission instant.
`is_current` requires both supervisor identity and connected generation. `Reply`
sends through that captured supervisor using the host-supplied absolute deadline,
including retained queue delay. `submit_replies` restores work at its original
outbox position when the task set is full. The concrete migration owners are
Term and BusViewer; these shared mechanics introduce no client, receiver, runtime
or worker loop. Product classification and shutdown ordering remain host-owned.

1. Enter once and capture the host's one absolute deadline. Stop new
   ordinary admission. Keep the accepted app.quit response ahead of close.
   Seal the reply/control producer or take its final bounded snapshot so
   Reply followed by Quit has defined ordering.
2. Preserve required host cleanup; generic queue code must not abort a
   capture-cancel/restore operation or replay collision intent.
3. Continue admitting retained replies whenever a job is reaped, within the
   same execution limit and deadline. Drain existing tasks and retained jobs
   as one loop; do not spawn the whole retained FIFO simultaneously, and do
   not stop merely because the JoinSet is temporarily empty while retained
   work exists.
4. Flush GUI results using sender readiness plus `timeout_at(deadline)`, not
   `yield_now` loops, sleeps or a new timer. If the GUI is closed, explicitly
   retire unsendable deliveries and retain their failure in the worker
   receipt; a final GUI Stopped delivery cannot be the sole completion
   acknowledgement once the receiver is gone.
5. Flush the latest settings jobs/cache through the existing
   `Lane.flush_cache(deadline)`, close the supervised client, abort or join
   only residual owned tasks within the same deadline, and publish the actual
   worker receipt. Record active and abandoned admission counts; zero queued
   replies alone is not a clean receipt.

## Limitations

- These primitives establish bounded ownership and honest outcomes. They do
  not guarantee delivery over a lost transport, replace per-app operation
  guards, or prove presented settings merely because a wake was queued.
- The outbox is single-owner by construction (`&mut self`); it is not a
  concurrent queue and not a replacement for an mpsc channel.
- A sender that panics takes the item it held with it; wrap transport calls
  in infallible adapters and treat a panicking sender as a host bug.
- `Permit::finish` is the actor's claim of a handled outcome; admission
  counts neither validate nor log responses.
- Deadlines, origin checks, reply capabilities and receipt content are
  host-owned and local to each app adapter.

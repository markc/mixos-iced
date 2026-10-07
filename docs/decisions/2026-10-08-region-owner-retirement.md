# Region selections are owned, fenced and never resurrected

Status: accepted design, implemented for `comp.region.select` / `comp.region.cancel` 0.1.12.

An identified `comp.region.select` names its caller with three fields: the
compositor process instance it was sent to, a random lowercase UUID v4 owner
capability, and a positive capture generation. The owner is validated
strictly — every hex position, the v4 version byte and the v4 variant byte,
lowercase only — and identity equality is exact string equality, so a lenient
reader can never split one identity into two. `comp.region.cancel` matches the
exact active identity and cancels it through the ordinary finish path (focus
restore, overlay removal, the original select reply); any other identity is
retired and can never start later, so a reordered mesh delivery or a
cancel-before-select can never resurrect a retired generation.

The engine fences the compositor instance on both verbs. A select or cancel
naming a different compositor process — a delivery still in flight after a
restart included — is refused `stale_instance` before any reservation, run or
retirement, so a stale identity can never act here, never blocks the live
identity, and cannot resurrect on the new process.

The per-owner retirement state is bounded at 4096 owners per compositor
process and persists for the process lifetime. At the limit, an unknown
owner's select or cancel is refused `region_owner_capacity` before anything
changes; known owners stay fully serviceable, and legacy identity-less
selections are unaffected. Owners are never evicted: the owner capability is
a targeting capability, not an authentication claim, so one caller that
presents 4096 distinct fresh UUIDs can fill the map until the compositor
restarts. That is accepted: the refusal is honest and bounded, and eviction
would trade memory for a window in which an evicted owner's delayed select
could resurrect — a weakening of retirement that is not on offer.

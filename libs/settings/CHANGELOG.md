# settings contract changes

## 0.3.0

Add shared retained/cache/embedded fallback preparation with host resource
validation, immutable stage fencing and explicit presentation labels. Persisted
data never seeds authority ordering or mutation fences. Optional cache I/O
uses bounded no-follow directory-relative reads and atomic replacement, with
schema/target/capability/digest checks plus complete projection recompilation.
Applied-generation captures and one locked serial writer reject stale saves;
ambiguous post-rename failure remains visible. Exact JSON float round trips
preserve compiler projection comparison. The authority wire/schema stay
0.1.0/1. Real resource loading, GUI/first-map timing and artifact integration
remain pending.

## 0.2.0

Add transport-neutral consumer and render-domain change plan, with an optional
native executor over an app-owned supervised connection. Bound work/buffering,
coalesce queue-loss recovery, fence connection/consumer/renderer completions,
and advance unchanged render evidence without a redraw. Retired confirmation
candidates are deduplicated within a bounded history. No owned runtime or
renderer dependency. Authority wire contract/schema remain 0.1.0/1.
Use canonical app/shell constructors, captured read baselines/tickets and one
absolute recovery deadline. Malformed-event storms cannot bypass the deadline;
same-incarnation rollback is distinct from a read racing a newer publication.
DTO additions require explicit render/evidence classification at compile time.
Persisted fallback cache, artifact/resource preparation and GUI activation are
still pending; this API does not advertise their completion.

## 0.1.0

Initial binding/revision/receipt/snapshot types, independent design read
projection, validated batch vocabulary and ordering/work-ticket reducer.
Renderer transport/bootstrap adapters remain pending.
Before initial release: report confirmed same-revision contradictions and
validate authored app enum values before accessibility precedence.

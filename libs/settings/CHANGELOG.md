# settings contract changes

## 0.2.0

Add transport-neutral consumer and render-domain change plan, with an optional
native executor over an app-owned supervised connection. Bound work/buffering,
coalesce queue-loss recovery, fence connection/consumer/renderer completions,
and advance unchanged render evidence without a redraw. Retired confirmation
candidates are deduplicated within a bounded history. No owned runtime or
renderer dependency. Authority wire contract/schema remain 0.1.0/1.
Persisted fallback cache, artifact/resource preparation and GUI activation are
still pending; this API does not advertise their completion.

## 0.1.0

Initial binding/revision/receipt/snapshot types, independent design read
projection, validated batch vocabulary and ordering/work-ticket reducer.
Renderer transport/bootstrap adapters remain pending.
Before initial release: report confirmed same-revision contradictions and
validate authored app enum values before accessibility precedence.

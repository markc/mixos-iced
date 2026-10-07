# Desktop settings contract

Status: accepted initial authority contract, version 0.1.0. Full desktop consumer
integration remains in development. No frozen ABP wire bytes change.

`settingsd` serves one explicitly initialised profile in the initial slice.
`settings` provides shared headless types, resolver and pure snapshot reducer;
`design::DesignReadProjection` is render data that cannot construct an accepted
design or fork compiler apply lineage. The current apps are Ced, Dopus, Term,
Cap, BusViewer and Scene Editor; compd owns the iced Quoin shell.

## Served verbs

All trusted mesh operators can invoke these verbs. Requests carry JSON bodies;
responses preserve ordinary ABP rc conventions: 0 for handled results and 10 for
refusal, invalid input or unknown verbs. Domain status is structured in the body.

| Verb | Input and result |
| --- | --- |
| `settings.describe` | No mutation; version, fields, ranges, defaults, limits and implemented/deferred capabilities |
| `settings.get` | `{binding:{instance,profile}}`; complete accepted snapshot, publication/recovery status |
| `settings.validate` | Fenced apply-shaped body; resolve/compile candidate without a write or receipt; valid with current base identity or diagnostics/conflict |
| `settings.apply` | Fenced batch described below; durable changed/unchanged receipt |
| `settings.reset` | Apply-shaped body with empty changes and explicit reset paths; same transaction semantics |
| `settings.status` | Binding plus optional operation_id; accepted/published revision and retained receipt or unknown_operation |

The profile identifier and instance use 1–64 ASCII letters, digits, dash or
underscore. Operation IDs use the same character set, up to 128 bytes. Revisions
are canonical decimal u64 **strings**, including on strict-data boundaries;
numeric, overflowing, signed and leading-zero representations are refused.

```json
{
  "binding": {"instance": "example", "profile": "default"},
  "expected_incarnation": "authority-readback-uuid",
  "expected_revision": "1",
  "operation_id": "example-batch-1",
  "changes": {
    "appearance.mode": "dark",
    "ui.text_scale": 1.1,
    "shell.panels.bottom.thickness": 48
  },
  "reset": []
}
```

An optional request_digest is checked against BLAKE3 of the canonical serde JSON
representation of the typed request, with request_digest set to null. Object
maps are sorted; reset-list order remains part of the request. Use the shared
request digest API rather than hashing arbitrary JSON text. Reusing an operation
ID with different input is refused. Receipt lookup/deduplication precedes expected
revision conflict, after target/incarnation checks. An old retained operation can
retrieve its original receipt after subsequent edits.
Retry the identical request to recover its receipt. Updating a fence changes the
digest and requires a new operation ID. Validation checks the present fence;
another writer may still win before a later apply.

Retain at most 128 receipts, inside the accepted document. A no-op writes its
receipt durably without increasing desktop revision or publishing a new snapshot.
Missing/evicted IDs return unknown_operation: opaque IDs have no expiry order,
and the initial contract cannot prove expired_operation. Resolve ambiguity by
readback and a newly fenced operation; exactly-once is not promised beyond
retention. Restores establish a new incarnation and clear foreign-history
receipts. Status includes publication_pending and recovering separately from
durable acceptance; a changed result can remain unpublished after broker failure.

## Initial data and resolution

The schema covers appearance scheme/mode/contrast plus embedded or complete
strict-data design source; UI density/text scale/reduced motion; named panels
with edge/mode/thickness; ordered shell pages; and whole app override records
with scheme/mode/contrast/text scale. Describe supplies exact ranges. Unknown
fields/scopes are refused. Field reset restores a package default; removing an
app override restores profile inheritance. Per-field authored provenance beyond
these initial concrete profile values is later work.
The package source is pinned in accepted storage at profile creation. Null
appearance.source selects that pinned source, including after binary upgrades;
it does not silently adopt the upgraded binary's embedded defaults. Adopting a
new package source requires an explicit mutation with a new revision.

Validate the whole candidate and all advertised app contexts before acceptance.
Compilation checks the source's claimed contexts. App overlays affect their
context alone; profile high contrast takes precedence. A custom source is at
most 256 KiB. Encoded requests are at most 384 KiB. Inline snapshots are at most
960 KiB, reserving 64 KiB for ABP/broker envelope overhead under the current 1 MiB
retained limit. Larger settings are refused until native immutable artifact
delivery is implemented. Required fonts/assets and runtime live capability are
not yet advertised.

Snapshot schema 1 includes binding, incarnation, desktop revision, resolved-design
generation, source digest, complete desktop and effective per-app/desktop design
projections. The resolved-design generation is owned by the authority and
survives restart; it is not a broker sequence or a reconstituted compiler apply
handle. Clients must confirm an incarnation change through fresh bound readback
before installing it. Work tickets fence superseded asynchronous completion.
Conflicting payloads at the same incarnation/revision trigger a fresh read;
if the bound authority confirms the contradiction, report it and preserve the
installed values rather than enter an endless readback loop.
Fresh decoded topic deliveries use Reducer.observe. Captured tickets are for
asynchronous completions; a new delivery can advance the state while an older
bound read is pending, and that read cannot roll the revision back.

## Topics and storage

Publish retained `settingsd.desktop.changed.<profile>`, owned by the registered
`settingsd` service. The unscoped base name is reserved too. All clients may
subscribe; ordinary publishers/clearers cannot replace its canonical state.
Noded strips inner routing headers and stamps broker_service independently of
caller input. The publisher repopulates retained state at startup and reconnect,
even without a settings mutation. Generic props-prefix ownership is deliberately
not used: embedding a profile in that prefix changes its matched service owner.
Failed publication starts at most three one-shot retries, with 250/500/1000 ms
delays. There is no idle timer. Reconnection or a new accepted revision starts a
new retry job; exhaustion leaves publication_pending visible.

Default root: resolved MixOS Etc directory / `settings/<profile>/`. Explicit
test roots are supported. One retained writer.lock inode provides a process
flock; it is never removed on normal exit. The root must be an owned directory
not writable by other users. `desktop.conf.mix` holds schema, binding,
incarnation, revisions, authored desktop, pinned package source, receipts and
a verified BLAKE3 content digest. This digest detects corruption; it is not an
authentication signature. A synced
`desktop.previous.conf.mix` is written before replacing the accepted generation.
Files are private, never evaluated as Mix source.
Reads, lock acquisition, replacements and syncs are relative to the same held
directory descriptor. Replacing/detaching the profile directory or writer-lock
inode fences the existing writer. Newly created directory entries are synced in
their parents before initial acceptance.

Replacement uses exclusive temporary create, write, file sync, directory-relative
rename and directory sync. Pre-rename errors leave accepted state unchanged.
Post-rename sync errors report outcome_unknown and fence further mutations until
recovery; they cannot be reported as a known uncommitted failure. Corrupt input
is preserved before a valid backup is restored with a new incarnation. Newer
schemas or mismatched target bindings are not overwritten by a fallback.
Corruption evidence is hard-linked and synced before the atomic replacement;
recovery never removes the primary before its replacement is ready.
A missing established primary fails visibly even with a backup; automatic
recovery applies only to a present corrupt primary.

`settingsd init --instance example` explicitly creates a profile and refuses
existing accepted/backup data. `settingsd serve --instance example` never
materialises defaults for a missing established store. Installer seeding and
automatic session adoption are separate later work; the unit is packaged but
the session target does not yet start it automatically.

## Validation and remaining scope

Machine fixtures: [authority.spec.mix](authority.spec.mix). Rust tests cover
precision, patches, projection round-trip, writer locking, changed/no-op receipts,
lost replies, conflicts, digest reuse, eviction, backup restore and future schemas.
`tests/settings/authority_test.mix` runs real ABP publication, unrelated operator
mutation, forged publish/clear refusal and authority restart against an isolated
noded. OS process lifecycle here belongs to the test fixture; application calls
and observations remain native ABP.

This slice does not claim GUI propagation, renderer acknowledgements, presentation,
offline async client bootstrap, candidate import/watch, immutable artifacts,
full field provenance, named-profile management, compatibility, scheduled policy,
preview, routed observation or replication. Every deferred feature must extend
the same authority and shared contracts, with appropriate contract versions and
migration. Native VT/image presentation and latency gates remain outstanding.

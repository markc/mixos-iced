# Desktop settings contract

Status: accepted initial authority contract, version 0.1.0. Full desktop consumer
integration remains in development. No frozen ABP wire bytes change.

`settingsd` serves one explicitly initialised profile in the initial slice.
`settings` provides shared headless types, resolver, consumer and snapshot reducer;
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
Panel thickness accepts a whole numeric value such as 48 or 48.0, within its
declared range; Mix data numbers use floating representation. Fractional values,
numeric strings and overflow are refused before acceptance.

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
The accepted effective digest also fences compiler interpretation drift. A
changed interpretation requires explicit migration rather than changing the
same accepted identity on reload.

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

The settings library API is now 0.2.0; authority verbs and snapshot schema remain
0.1.0 and 1. The shared consumer performs subscribe-before-get over the host's
existing supervised Bus connection through its optional native executor. It
owns no transport/task/incoming receiver. Hosts feed connection generations,
deliveries and explicit queue loss, execute at most one current action, and
cancel superseded work. Bootstrap buffers one latest candidate; loss coalesces
one later full read. A bounded history suppresses repeated rejected confirmation
candidates. Transport recovery has one pending backoff from 250 ms to 30 seconds;
success/disconnect cancels it. Protocol/target/schema refusal is visible and
quiescent until a new event or explicit refresh. Every native call is bounded
to one second; the host must also cap the combined initial bootstrap to one
second and show a labelled fallback while recovery continues.

One context-specific change plan separates paint, text, layout, resources, motion
and optional shell geometry. Source/provenance/revision-only changes advance
evidence without rendering work. Received accepted data is distinct from staged
and applied data. Prepare all resources, then on the renderer event loop check
the stage ticket immediately before synchronous activation and acknowledgement,
without an intervening await. Old connection, consumer and stage completions
cannot claim application. Hosts must fence the actual swap as well as its report.
Identical render inputs need no swap; application never proves presentation.
Local persistent fallback cache and artifact/resource preparation remain pending.

## Topics and storage

Publish retained `settingsd.desktop.changed.<profile>`, owned by the registered
`settingsd` service. The unscoped base name is reserved too. All clients may
subscribe; ordinary publishers/clearers cannot replace its canonical state.
Noded strips inner routing headers and stamps broker_service independently of
caller input. The publisher repopulates retained state at startup and reconnect,
even without a settings mutation. Generic props-prefix ownership is deliberately
not used: embedding a profile in that prefix changes its matched service owner.
Failed publication starts a single pending retry job, with exponential backoff
from 250 ms capped at 30 seconds. Success removes the job and every timer.
Reconnection or a new revision restarts the backoff. There is no idle heartbeat;
publication_pending remains visible throughout a failed pending job.

Default root: resolved MixOS Etc directory / `settings/<profile>/`. Explicit
test roots are supported. One retained writer.lock inode provides a process
flock; it is never removed on normal exit. The root must be an owned directory
not writable by other users. `desktop.conf.mix` holds schema, binding,
incarnation, revisions, authored desktop, pinned package source, receipts and
a verified BLAKE3 content digest and effective interpretation digest. These
digests detect corruption and interpretation drift; they are not an
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
I/O errors, strict shape changes and intact documents requiring a changed
compiler interpretation fail without an automatic backup rollback.

`settingsd init --instance example` explicitly creates a profile and refuses
existing accepted/backup data. `settingsd serve --instance example` never
materialises defaults for a missing established store. `settingsd seed --instance
example` provisions a first-run profile or validates an existing primary without
rewriting valid state. A retained writer.lock is establishment evidence: seed
refuses a missing established primary even without a backup. Existing-store
corruption uses the same validated, evidence-preserving recovery as serve.
The unit seeds before serving, binds the instance to the machine hostname, and
the session target wants/upholds it. Settings readiness never blocks application
or compositor startup. The image provisions the authority account's writable
MixOS settings parent; isolated native image adoption still needs runtime evidence.

## Validation and remaining scope

Machine fixtures: [authority.spec.mix](authority.spec.mix). Rust tests cover
precision, patches, projection round-trip, writer locking, changed/no-op receipts,
lost replies, conflicts, digest reuse, eviction, backup restore and future schemas.
`tests/settings/authority_test.mix` runs real ABP publication, unrelated operator
mutation, forged publish/clear refusal, authority restart and cold broker restart
against isolated noded instances. Broker loss keeps the authority alive and its
writer exclusive; the shared consumer reconnects through the same client and
the authority repopulates the empty retained broker without a semantic edit.
OS process lifecycle here belongs to the test fixture; application calls
and observations remain native ABP.

This slice does not claim GUI propagation, renderer acknowledgements, presentation,
persistent offline cache/fallback presentation, candidate import/watch, immutable artifacts,
full field provenance, named-profile management, compatibility, scheduled policy,
preview, routed observation or replication. Every deferred feature must extend
the same authority and shared contracts, with appropriate contract versions and
migration. Native VT/image presentation and latency gates remain outstanding.

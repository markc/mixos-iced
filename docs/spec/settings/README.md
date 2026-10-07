# Desktop settings contract

Status: accepted initial authority contract, version 0.1.1. Full desktop consumer
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
delivery is implemented.

## Appearance resources reference

`appearance.resources` is an optional versioned subdocument naming one immutable
asset set by explicit identity, accepted through the ordinary fenced batch as a
whole object; reset restores omission and nested partial paths such as
`appearance.resources.set_id` are unknown. The authority validates structure
only and never opens files, reads a host font database or performs asset I/O.

```json
{"schema": 1, "set_id": "core-icons",
 "manifest_blake3": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
 "icons": {"family": "Symbols", "style": "rounded", "weight": 400}}
```

`schema` must be exactly 1; anything else is unsupported, not ignored.
`set_id` uses the assets set-ID contract (1–96 ASCII letters, digits, dash or
underscore). `manifest_blake3` is exactly 64 lowercase hex characters, so a set
with the same ID and a changed manifest is never the requested set. The optional
`icons` record selects one declared catalogue: family at most 256 bytes, style
at most 96 bytes, weight an exact 1–1000 value. `schema` and `weight` accept
Mix's floating whole numbers (`1.0`, `400.0`) exactly like panel thickness;
fractional, negative, out-of-range and non-finite values are refused before
acceptance. Unknown fields anywhere in the
subdocument fail. Apps never override resources; every effective context
carries the profile reference verbatim.

Omission means the profile-pinned packaged default and is skipped in BOTH
authored and effective serialisation. Serialising a null resources field would
change old omitted-resource bytes and every digest over them (accepted profile,
snapshot, effective interpretation), so old accepted profiles, old snapshots
and old cache envelopes remain byte- and digest-identical. An existing old
strict client rejects a snapshot carrying the new field rather than silently
applying a changed interpretation; this compatibility boundary is advertised
in `settings.describe` and contract version 0.1.1 while the top-level snapshot
schema stays 1. An unavailable set is a consumer preparation fault reported as
LastGood, never authority rollback or a settingsd font read.

A resource reference change conservatively invalidates resources, text, layout
and paint in the shared change plan (icon glyphs may change even with identical
public family names); omission-to-omission colour changes do not re-register
resources. A reference change participates in full appearance equality, so it
advances design revision as well as revision.

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

The settings library API is now 0.3.5; the contract is 0.1.1 and the snapshot
schema remains 1. The shared consumer performs subscribe-before-get over the
host's
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
Use the same initial deadline with `native::execute_until` for subscribe and
read. Recovery exposes an absolute deadline, preserved during malformed-event
storms; arrivals cannot restart or bypass it. Canonical app/shell constructors
choose `app:<id>` or `desktop` contexts. Captured read tickets and baselines
distinguish real same-incarnation rollback from a valid read racing a newer
publication. Cancel old executor futures when their current work ticket changes.

One context-specific change plan separates paint, text, layout, resources, motion
and optional shell geometry. Source/provenance/revision-only changes advance
evidence without rendering work. Received accepted data is distinct from staged
and applied data. Prepare all resources, then on the renderer event loop check
the stage ticket immediately before synchronous activation and acknowledgement,
without an intervening await. Old connection, consumer and stage completions
cannot claim application. Hosts must fence the actual swap as well as its report.
Identical render inputs need no swap; application never proves presentation.
`Consumer::evidence()` reports binding, context, connection generation,
confirmation, presentation kind, accepted (`current`) and UI-acknowledged
(`applied`) identities, plus normal and fallback faults. Each identity carries
incarnation, canonical string revision, design revision and source digest.
Absent identities are null. Losing confirmation demotes installed current data
to LastGood; an installed fallback is never an authority fence. The shell
exposes this same evidence at `shell.props.get` path `settings`, including root
and dotted reads such as `settings.applied.revision`, behind its existing caller
and connection checks. Evidence does not assert a broker participant record or
frame presentation. `settings_cache` supplies the shared host's persistence
identity, cache fault and fallback diagnostics. The persistence identity is
historical and must match current applied evidence to describe that capture.
Quoin uses the shared worker's service-owned persistent cache while its existing
Bus supervisor starts; terminal refusal preserves offline resources, and only
explicit initial rejection permits a configured scene service override.
`compd.truth.slots.<id>.decided` independently reads the
geometry owner's decided size, or null when absent, without falling back to
committed client geometry. The native desktop geometry gate compares these
readbacks with real client configures and captured pixels.
DTO field additions require explicit classification in the shared comparison;
source/provenance exclusions are deliberate rather than wildcard omissions.
Shared fallback preparation tries validated retained bootstrap data, then local
cache, then current embedded defaults. The host supplies readiness checks for
every reference used by its context and shell capability on its worker. A fenced
completion produces the same immutable UI stage; it never seeds the authority
reducer or mutation fences. Installed data has explicit Current, Retained,
Cached, LastGood and Embedded labels. Identical fresh authority data can promote
cache/embedded evidence without a redundant swap.

Optional `settings/cache` stores applied authority/retained generations in an
existing isolated absolute cache directory. Its versioned JSON envelope binds
instance/profile, context, shell capability, interpretation and content digest.
Bounded directory-relative regular-file reads refuse symlinks in every path
component and cannot block on FIFOs. Before reuse, complete projections must
match current compilation of their authored inputs; unavailable old pinned
package sources and compiler drift are refused visibly. Exact JSON float
round trips preserve these checks. Digests detect corruption, not file origin;
valid cache data still supplies no authority evidence. A stable locked serial
writer fences superseded applied-generation saves, permits retry of the latest
failed attempt and reports ambiguous post-rename failure. All I/O belongs off
the UI loop. Cache failure preserves usable applied data. Real font/asset loading,
artifact preparation and GUI fallback/first-map timing remain pending.

Cache envelope schema 2 adds a renderer-neutral `ResourceBinding`: schema,
set ID, exact manifest digest, versioned selection interpretation and optional
icon reference. Its domain-separated digest covers the unchanged canonical
snapshot AND the binding, so a legacy snapshot digest can never replay into a
resource-aware envelope. Resource-aware hosts return the binding from their
readiness check; the staged update carries it, so the ordinary
`Consumer::acknowledge` preserves it, and `acknowledge_resources` remains for
current stages where the host computes the binding — it must agree with a
fallback stage's carried binding rather than replace it. The captured save
writes schema 2. The readiness check is
also given the expected binding of the candidate it is checking (retained
activation binding, cached envelope binding, or none for embedded) and must
return exactly that binding when one is expected; the fallback owner rejects
disagreement with `binding_mismatch` and continues the ladder, so a different
current-default resolution is never relabelled as the cached candidate. On
load, an explicit authored reference must equal the recorded binding exactly
(`cache_binding_mismatch` otherwise), including an omitted icon selector;
`ResourceBinding.icons` records the authored optional selector verbatim, and
the resolved default family/style/weight belongs to appearance evidence,
never the cache binding. For omission the binding records the host's pinned
default identity (icons None) without canonical mutation. Bindings never
carry face IDs, aliases, pointers, filesystem roots or font bytes; cold loads
re-register from verified set bytes. The resource interpretation is
feature-independent `settings::resource_interpretation()`; the cache module
re-exports the same value.

Legacy schema-1 envelopes are read deliberately under the one named
predecessor interpretation, with their original strict digest/recompile
checks, and only without an explicit resource reference. Their absent binding
means this host's packaged-default policy with honest legacy/unpinned-cache
evidence; they never claim to retain original font bytes. New captures with a
binding write schema 2; captures without one keep writing the recognised
legacy predecessor bytes, and no eager rewrite of old files occurs on read.
The writer applies the same binding/reference equality as the loader before
serialising: a mismatched binding or an invented selector on omission refuses
the write with `cache_binding_mismatch`, and a resource-bearing snapshot
without a binding refuses with `cache_binding_required`, so no envelope the
loader rejects is ever persisted. The named predecessor interpretation derives
from the actual embedded default source, so legacy loadability is conditional
on that source remaining byte-identical to the 0.3.4 release's; a changed
default relabels genuine legacy envelopes `unsupported_cache` rather than
loading them under a changed interpretation. The resource interpretation also
embeds the settings library version, so every settings release refuses
resource-bound schema-2 caches (`unsupported_cache`) until new captures are
written.

## Shell panel preferences

Quoin prepares appearance and the global panel policy together, then installs
both on the compositor loop before acknowledging the settings revision. Each
record's explicit edge selects one of four panels on every current and future
output; the record ID is provenance, independent of a connector or page name.
`dock`, `overlay` and `hidden` map to runtime docked, pinned and hidden modes.
The default profile owns the bottom panel in dock mode, requesting 40 logical
pixels. An edge without pages draws nothing and reserves no work area.

Thickness is requested logical pixels. Runtime edge ranges, the active page's
minimum and the output/opposite-edge budget fit the request. A bottom page with
a 52-pixel minimum therefore presents 52 pixels for the default 40-pixel
request. Panel rows include a nullable `settings` object with the record ID,
`requested_px`, `fitted_px`, `presented_px`, `reserved_px` and constraint names.
Output shrinkage fits the panel; regrowth recovers the request. Page selection
and registration use the existing carousel and panel model.

Settings owns the mode and thickness of each declared edge. Conflicting legacy
panel writes refuse `SETTINGS_MANAGED` before changing interaction state; exact
current mode or settled-size requests are no-ops. Transient show/hide and page
selection remain available. Persistent toggles cannot undock managed panels.
For managed edges Toggle changes transient visibility only while hidden; docked
or pinned modes treat it as a successful no-op. Corner-menu mode entries are
disabled while managed; activating a disabled selection is an inert action.
Removing a profile record
restores that edge's captured local mode and settled thickness. Saving a page
selection keeps those local values, so profile preferences do not leak into the
local state file. Thickness or appearance updates retain interaction holders;
an actual transition to hidden retires invalid keyboard/menu holders.

Duplicate records targeting one edge and non-empty `shell.page_order` lack a
runtime identity mapping and fail whole shell preparation. The authority's
schema can accept them, but Quoin reports the preparation fault and retains
the last good appearance and panel policy. Correct them through a new fenced
settings mutation. This is consumer capability validation, not a rollback of
the accepted authority revision. Empty page ordering preserves local order.

Requested maximised windows keep their owning output and follow only changes
to their desired content rectangle; removal chooses an available output while
retaining the original unmaximise geometry. Scene reservations are reconciled
after settings activation and before frame work, including without the comp
Bus port. An unchanged target does not emit another maximise configure while
the client is slow to acknowledge or replace its buffer. The compositor's
decided slot remains authoritative during these transitions; older legally
acknowledged buffers do not undo a newer target. Existing aspect-preserving
fit and inverse input mapping remain in use.

Fullscreen continues to use the whole output. Its compositor record, pending
protocol intent and committed state fence work-area reconfiguration through
delayed entry/exit commits. After fullscreen releases ownership, the next normal
dispatch restores the latest maximised content rectangle, even if the work-area
map has not changed again. Window geometry changes schedule redraw and retarget
the human Wayland pointer, respecting grabs. This does not by itself prove
stationary iced hover or scene bounds before/after rendering.

The protocol fixtures exercise actual xdg configure/ack/buffer commits using
the production geometry executor. Their headless output has no renderer; native
Bus delivery fixtures and protocol fixtures do not establish their composed
rendered settings-to-frame behaviour. Persistent tiling, new maximise intent
with no outputs, native window/input/frame evidence and VT latency acceptance
remain outstanding. Current text scale changes the title font, while decoration
extents continue to come from the selected style's fixed height and border.

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
materialises defaults for a missing established store. Plain `settingsd seed
--instance example` validates an existing profile without rewriting valid state;
it refuses a wholly missing directory. First installation explicitly uses
`settingsd seed --allow-create --instance example` as the service account.
A retained writer.lock is establishment evidence: even --allow-create refuses
a missing established primary without a backup. Existing-store
corruption uses the same validated, evidence-preserving recovery as serve.
The unit validates with plain seed before serving, binds the instance to the machine hostname, and
the session target wants/upholds it. Settings readiness never blocks application
or compositor startup. The image provisions the authority account's writable
MixOS settings parent and seeds once explicitly. Automatic unit startup never
uses --allow-create; missing mounts/directories fail visibly rather than resetting
preferences. Isolated native image adoption still needs runtime evidence.

## Validation and remaining scope

Machine fixtures: [authority.spec.mix](authority.spec.mix). Rust tests cover
precision, patches, projection round-trip, writer locking, changed/no-op receipts,
lost replies, conflicts, digest reuse, eviction, backup restore and future schemas.
`tests/settings/authority_test.mix` runs real ABP publication, unrelated operator
mutation, forged publish/clear refusal, authority restart and cold broker restart
against isolated noded instances. Broker loss keeps the authority alive and its
writer exclusive; the shared consumer reconnects through the same client and
the authority repopulates the empty retained broker without a semantic edit.
The broker-restart fixture also starts a headless cold consumer from persisted
cache while offline, then promotes it to current with no redundant stage after
fresh ABP readback. Resource readiness is a fixture inventory, not GUI proof.
Another native fixture keeps an authority registered but deliberately withholds
read replies, exercising the combined bootstrap deadline and expired work.
OS process lifecycle here belongs to the test fixture; application calls
and observations remain native ABP.

This slice does not claim GUI propagation, renderer acknowledgements, presentation,
GUI offline-fallback/first-map timing, candidate import/watch, live resource
hosting and font registration, immutable artifact verification, full field
provenance, named-profile management, compatibility, scheduled policy,
preview, routed observation or replication. Every deferred feature must extend
the same authority and shared contracts, with appropriate contract versions and
migration. Native VT/image presentation and latency gates remain outstanding.

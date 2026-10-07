# app.describe — the shared application discovery contract

Status: **accepted**, contract version 0.1.0, marker `application.describe.v1`.
Implemented in `libs/application/src/describe.rs` behind the opt-in
`describe` feature (serde/serde_json only). Registered once in
`docs/spec/bus/verbs.conf.mix` with owner `application`. Machine fixtures:
[describe-v1.spec.mix](describe-v1.spec.mix). Producer migrations are tracked
below; until a producer migrates, its legacy object stays served and
`read_legacy` reads it.

This standardises one existing discovery verb and its live metadata. It does
not widen application mutation authority, add a second settings API, or create
a dispatcher, application trait, lifecycle scaffold or second registry.

## Request

The canonical request is `{}`. Empty and whitespace-only bodies are also
accepted as `{}` for existing callers. Before dispatch the host runs the
shared `validate_request` on the command body:

- bodies over 4 KiB are refused before parsing;
- malformed JSON, null, arrays and scalars are refused;
- nonempty objects are refused as arguments.

This makes previously ignored invalid arguments fail explicitly; the valid
no-argument request is unchanged, and no negotiation fields are introduced.
Inspect the command body, not unrelated routing or authentication headers.

## v1 response

A JSON object. Unknown root fields are allowed and preserved. The marker
versions only the common envelope; `version` remains the responding
application's software version, and existing product markers such as
`contract:"ctk-app-control.v0"` or `schema:"cap.v1"` are neither replaced nor
required to adopt one product schema.

| Field | Requirement |
| --- | --- |
| `describe_contract` | The exact marker `application.describe.v1` |
| `version` | Nonempty bounded software version string; preserve the existing value |
| `pid` | Positive u32 process id of the actual responding owner |
| `service` | The actually registered service name, including an allocated/fallback name |
| `app_id` | Nonempty Wayland application id for GUI clients; explicit null when there is no applicable Wayland client identity |
| `verbs` | Array of nonempty names **or** descriptor objects, including `app.describe`; mixed arrays are valid |
| `settings` | Optional/null for a host without a settings consumer; otherwise the unmodified live shared Consumer Evidence object |
| `settings_cache` | Optional/null without a cache session; otherwise the unmodified shared Session CacheEvidence object |

A descriptor is an object with a nonempty string `name`. Optional `read_only`
is boolean or null; absent/null means **unknown**, never false or true.
Optional `description` is a string. Arbitrary bounded `args` values and
extension fields are preserved. Duplicate names are refused for v1 producers.
Safety is never inferred from a prefix, the spelling "get", registry
membership or the verb's own read-only status; a descriptor for
`app.describe`, when supplied, must mark `read_only` true.

Limits, enforced on the encoded bytes where a bound is on bytes:

| Bound | Value |
| --- | --- |
| request body | 4 KiB before parsing |
| encoded response | 256 KiB, refused rather than truncated |
| verbs | 512 |
| verb name | 128 bytes |
| identity/version fields | 256 bytes |
| one descriptor | 16 KiB |

Callers validating native responses must bound the raw body to 256 KiB
**before** `serde_json::from_str`; the `Value` validator alone cannot enforce
a raw input byte limit. Use the existing Bus command/response deadlines.

### Evidence fields

`settings` and `settings_cache` are checked only as object-or-null. Their
shape and semantics stay owned and tested where they live:
`settings::consumer::Evidence` (`libs/settings/src/consumer.rs`) and
`application::presentation::native::CacheEvidence`. Producers serialize those
exact values; conformance tests compare the inserted JSON with the owning
session's serialization. This validator does not reimplement the settings
state machine, and does not require current==applied, kind==current or
cache.persisted==applied: bootstrap, pending activation, offline LastGood,
diagnostics and pending persistence are valid describe states. There are no
`accepted`/`presented` fields — `current` and `applied` already have precise
shared meanings.

## Shared API

```rust
pub const CONTRACT: &str = "application.describe.v1";
pub const VERB: &str = "app.describe";

pub fn validate_request(body: &str) -> Result<(), Violation>;

pub struct Identity<'a> {
    pub app_id: Option<&'a str>,
    pub version: &'a str,
    pub pid: u32,
    pub service: &'a str,
}
pub fn complete(value: &mut serde_json::Value, identity: Identity<'_>)
    -> Result<(), Violation>;

pub fn validate(value: &serde_json::Value) -> Result<Description<'_>, Violation>;
pub fn read_legacy(value: &serde_json::Value)
    -> Result<LegacyDescription<'_>, Violation>;
```

`complete` first verifies an object and its verb inventory, then adds the
common fields from the actual frontend identity. If a reserved field already
exists it must equal the supplied value; contradictory
app_id/version/PID/service is an error, never silently overwritten. All
unrelated fields, nested values and verb representations/order are preserved,
and the updates are applied atomically only after the completed value
validates as a whole. `complete` never turns a `Vec<String>` response into
mixed descriptors. Product verb tables must add `app.describe` in their
existing representation before completion.

`validate` requires the v1 marker and the required common fields; an absent or
unknown marker cannot accidentally pass as v1. `read_legacy` stays explicit
and permissive enough to read the current product objects (a verbs array plus
partial identity). It returns missing fields as missing, never inferred or
fabricated, keeps legacy duplicate compatibility, and does not convert a bare
HELP array or shell.info's bare suffixes into a v1 describe. BusViewer keeps
its own permissive HELP parser for those sources.

`Description` and `LegacyDescription` borrow the object and expose the common
fields and a verb iterator; they do not clone a second source of truth.
`Violation` carries a bounded field path, a stable code and a message; the
caller maps it to its own rc and refusal body, so no application's established
error codes are renamed. Refusals keep the ordinary ABP rc conventions (0
handled, 10 refusal).

## Fixtures

JSON response bodies live under `docs/spec/application/fixtures/`. The strict-data
manifest names each fixture, its producer, mode (legacy/v1) and expected
validity/refusal. Producer fixtures are copied from the actual builder they
name; only declared nondeterministic values (pid, installed build version,
active tab, evidence contents) are example-normalised in the owner's exact
field shape. Validator-only cases are labelled `validator-only`. Term and Cap
serve no describe verb in this tree and have no fixture — no fabricated
replies for unmigrated producers, and no seven fabricated "ideal" envelopes.
Legacy outputs preserve which fields are absent. `shell.info` is included as
a documented non-app.describe object, not as a fabricated seventh describe
reply.

## Producer conformance

| Producer | Served today in this tree | Migration |
| --- | --- | --- |
| Ced | Legacy ctk-app-control.v0 reply; GUI seam adds settings/settings_cache | Complete at the existing GUI response seam |
| Scene Editor | `schema:"scene-editor.v1"` describe + evidence; verbs omit app.describe | Add app.describe to the inventory, complete at the `command` seam |
| Term | No app.describe served | Add it to the frontend's queued describe handler |
| BusViewer | Descriptor inventory; served app.describe not advertised | Add the app.describe descriptor, version/pid/service at the GUI seam |
| Cap | No app.describe served | Reuse the existing empty-object parsing and modal-safe branch |
| Dopus | `dopus.describe` legacy reply only; no app.describe dispatch | Keep dopus.describe; complete app.describe in the desktop adapter |
| scene-host (compd) | shell.info + shell.props.get only | Add SceneVerb::AppDescribe on the existing owner loop |

Per-owner integration follows the design review of 2026-10-08; the exact
seams are:

- **Ced**: preserve the controller's legacy DescribeReply and UI-owned
  augmentation. Validate the request before dispatch; at the existing GUI
  response seam call `complete` with `APP_ID`, the actual Bus service and
  `std::process::id()`, after serializing the reconciled session evidence
  once. Keep the headless path separate and honest (app_id null, no
  fabricated settings evidence).
- **Scene Editor**: keep `model::describe` as product metadata, add
  `app.describe` to its string inventory, and `complete` it at the existing
  `command` response seam after `settings_ui.reconcile`, adding actual
  PID/service and retaining views and schema.
- **Term**: complete the frontend's queued describe response after its
  existing settings processing, using `service_name()`, and add PID. Do not
  move this to term-core or reconstruct frontend appearance in the Bus
  worker. Audit the advertised inventory against the actual global handler:
  native-session route verbs must not be advertised as global verbs.
- **BusViewer**: retain descriptor objects, add the app.describe descriptor
  (read_only true) to the same model table used by HELP, and add
  version/PID/service at the GUI seam. Do not erase args/read_only metadata
  or change fallback discovery of older peers.
- **Cap**: reuse the existing empty-object parsing and modal-safe describe
  branch, delegating the common request check. Complete with PID/service and
  current reconciled evidence; preserve `cap.v1` and the string verb table.
  Headless has no fictitious GUI consumer.
- **Dopus**: keep app.describe and dopus.describe as aliases in the core
  dispatch, and complete/enrich app.describe in the desktop adapter. Do not
  change the old dopus.describe body or the core DescribeReply literal; add
  APP_ID, PID and the live service at that adapter.
- **Quoin/scene-host**: add a distinct SceneVerb::AppDescribe, recognised
  before the existing shell/service-prefix parser. Serve it on SceneHost's
  existing owner loop using the same fresh evidence as shell.props.get,
  returning changed=false with no scene mutation or settings broker query.
  Advertise full canonical shell.* names for this new response, plus
  app.describe; keep shell.info's bare suffix list and semantics unchanged.
  Pass authoritative compd component version/build identity into the host (a
  private crate's workspace CARGO_PKG_VERSION is not compd's public version).
  app_id is null; conformance uses compd ownership, not a pretend separate
  window.

The shared helper owns neither freshness nor response ordering. The frontend
must sample/reconcile the live generation before reading its one session,
then construct the whole response without an intervening await, and respond
through its existing generation-fenced Bus reply machinery. No settings.get
round trip, extra subscription or polling. Queries stay available during
dialogues and pending product operations whenever the service is serving;
they close no dialogue, clear no ticket, start no discovery and change no
tab. This domain read-only contract is not the stronger no-redraw
layout-inspection contract; describe traffic does not prove frame
invalidation.

### Conformance required before claiming universal support

1. Unit validator cases: `{}` and empty requests accepted; invalid
   shape/unknown args/oversize refused; legacy marker absence distinguishable
   from current; both verb representations and mixed arrays accepted; unknown
   safety remains unknown; malformed descriptors, duplicate v1 names,
   contradictory reserved metadata and byte/count limits rejected; unknown
   extension fields survive.
2. Actual producer tests run the shared validator on each emitted response
   through the producer's real handler (controller test plus GUI
   augmentation, real command handler, queued frontend handler, response
   sink, open-dialog test, Served::Reply augmentation, native Port
   request/response) — not only `complete()` with hand-built maps.
3. Literal evidence equality: every native GUI response's settings/cache
   equals its current owning session's serialization after reconciliation,
   covering Current, initial fallback, pending activation and lost-generation
   LastGood without insisting those states have identical current/applied
   values.
4. Inventory tests: app.describe appears once and is actually served;
   existing product verbs are retained; HELP descriptors preserve their
   metadata; shell.info stays byte-shape compatible while the new
   app.describe reports full names and the actual fallback service; Term's
   targetless versus target-bound routing distinction is checked explicitly.
5. Native acceptance reuses the owned app gates: call app.describe with `{}`
   on each exact candidate, validate the complete response, match PID and app
   id to the owned process/window, and match settings
   context/binding/identity to the actual authority phase. Reissue during one
   genuine pending operation or open dialogue to prove no product-state
   mutation. Headless and response-sink tests are not native GUI evidence.

Until all seven pass, report precisely which producer is
legacy/unmigrated. Accepting six old envelopes through read_legacy does not
establish universal v1 support. The registry row (`app.describe`, owner
`application`, version 0.1.0) tracks this verb contract; application software
versions keep their own normal release changes.

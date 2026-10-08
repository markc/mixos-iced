---
title: Application macro contract
description: Standard discovery, invocation context and native execution requirements for Mix application macros.
---

# Application macro contract

Status: accepted target contract, version 1.0.0, 2026-10-08. Shared implementation
and all-app rollout are pending. Existing Ced support is the initial reference,
not proof of compliance with every requirement below. No ABP wire bytes change.

Authority: [application macro decision](../../decisions/2026-10-08-application-macros.md).

## 1. Scope and ownership

Every native GUI app supports this contract. A scene-backed application surface
also has a macro entry; compd/Quoin hosts the shared adapter and supplies its
scene identity. Background services and CLI tools expose operations rather than
menus. No app-specific interpreter or independently copied runner is allowed.

`application` owns discovery, context types, execution coordination and bounded
result delivery. App adapters own menu placement, context capture, domain
operations, revision checking and undo. The existing host worker integrates
macro work without adding a second Bus connection or UI event loop. Toolkit
receives only generic menu/output models and actions.

## 2. Runtime and source homes

| Kind | Runtime home | Source ownership |
| --- | --- | --- |
| App-local macro | `<AppDirs component>/config/macros/*.mix` | Shipped examples in `apps/<component>/scripts/macros/`; user additions stay in the resolved app root |
| Scene-local macro | `<AppDirs scene component>/config/macros/*.mix` | Shipped examples owned by the scene/compd component; distinct from scene layout and behaviour |
| Global script or macro | `/opt/mixos/mix/*.mix` | Shared runtime scripts in `share/mix/`, collected for installation |

Use the authoritative AppDirs resolver, including existing profile/image-root
overrides. Do not duplicate or replace precedence in macro code. Different
profiles or instances with distinct app roots must not share local catalogues
accidentally. A scene adapter must assign a stable component/root explicitly;
it must not put all scenes into a single undifferentiated Quoin directory.

Discovery is non-recursive over these two runtime locations. Reusable helper
modules may live below `lib/` and be loaded explicitly with `require`; they are
not menu entries. Behaviour, strict config and scene layout files are not macros
merely because their names end in `.mix`.

Provision shipped app examples only when their destination is absent; never
overwrite a user's copy on upgrade. Global installation follows the existing
packaging/provisioning path; applications run as the user and never elevate to
write `/opt/mixos/mix`. **Create Macro…** defaults to the writable local home;
global editing remains available through the normal authorised operator path.
Document which global files are package-owned so upgrades preserve operator
additions. These two homes do not relocate service scripts, build tools or hub
automation: those retain their owning component or private hub.

## 3. Script metadata and discovery

An eligible macro is a regular readable `.mix` file with this leading header:

```mix
-- macro: Report selection
-- macro-key: Ctrl+Alt+R
-- macro-apps: ced,dopus
```

`macro` is the required non-empty menu label. `macro-key` is an optional shortcut
using the shared chord parser. `macro-apps` is an optional comma-separated list
of exact component IDs or the single value `*`; omission means all apps for a
global macro and its owning app for a local macro. Invalid metadata is diagnosed
and never interpreted as broader eligibility. This filter controls presentation,
not Bus authority.

Read at most the first 20 lines, stopping at the first non-comment executable
line; allow blank lines in that block. Filenames follow `snake_case.mix` for new
files. Preserve Ced's previously accepted stems on migration. Ignore unlabelled
scripts, directories and helper modules. Discovery never executes a script.
Malformed entries produce diagnostics while other valid entries remain usable.

Identity is `(scope, stem)`, for example `local:report_selection` or
`global:report_selection`; same-stem entries remain distinct. Menus group **This
App** and **Global**, sort by label then identity, and show scope when labels
collide. Built-in shortcuts win; then local macros, then global macros, each in
stable filename order. A shortcut conflict drops only the shortcut, not the menu
entry, and is reported. Ced keeps its existing local `macro.<stem>` action IDs.

Ced accepts `-- ced-macro:` and `-- ced-key:` as compatibility aliases. A file
using old and new headers with contradictory values is refused with a diagnostic;
matching duplicates are allowed. Migration never rewrites user scripts silently.

Load at startup and on **Reload Macros**. Native filesystem events update the
catalogue; rename, removal and overflow trigger a fresh scan. There is no polling.
If watching is unavailable, show that fact and retain explicit reload. Menu
discovery and header I/O run off the UI thread.

## 4. Common invocation context

Run the selected file with `/opt/mixos/bin/mix` using an argv list, without a
shell wrapper. A missing interpreter produces a visible failure with no fallback.
Macros are ordinary one-shot scripts; a macro that needs a resident worker uses
the existing native service/lifecycle facilities explicitly.

Supply `MIX_MACRO_CONTEXT` as a JSON-encoded versioned envelope:

```json
{
  "schema": "mixos.macro-context.v1",
  "run_id": "opaque-run-id",
  "macro": {"scope": "local", "stem": "report_selection"},
  "app": {"component": "ced", "app_id": "dev.mixos.ced", "service": "ced", "instance": "example"},
  "app_root": "/home/user/.local/state/mixos/apps/ced",
  "cwd": "/home/user/Documents",
  "target": {"kind": "buffer", "id": "opaque-buffer-id", "epoch": "opaque-epoch", "revision": "7"},
  "selection": {"unit": "utf8-byte", "start": "0", "end": "12"},
  "origin": "agent:macro.report_selection",
  "extensions": {}
}
```

`app` identifies the actual invoking instance and advertised Bus service, never
an assumed singleton. Adapters inherit the session's real native Bus connection
configuration. `app_root` and `cwd` are resolved absolute paths. `target` and
`selection` are nullable; domain-specific extra references go in the component's
object under `extensions`. Revisions and offsets are canonical decimal strings
to preserve full integer range across Mix's floating-point numbers. Selection
units are explicit, with offsets into the captured target revision.

The UTF-8 JSON envelope is bounded to 64 KiB. Pass large selections, image data
and file collections by native handles/references rather than putting their
content in environment variables. Unrepresentable context is a visible refusal,
never truncation. Values are data, never injected into generated source. Helpers
use ordinary `env`, `json_parse` and validation with structured failure results.

Capture context when execution begins, after draining relevant pending app work.
Edits use the owning service's epoch/revision fences and native transaction
semantics. A stale target is reported; never redirect to a newly active document.
Switching tabs or focus after launch does not change the captured target.

Ced additionally supplies all existing `CED_*` variables with their existing
meaning, including `CED_ORIGIN=agent:macro.<stem>` for legacy local macros.
New global invocation origins include scope to distinguish them; the adapter
must validate origin length and the owning service's accepted syntax.

## 5. Menu and execution lifecycle

Every app presents **Macros** by default. It contains eligible local/global
entries and **Create Macro…**, **Open App Macros Folder**, **Open Global Macros
Folder**, **Reload Macros**, and **Macro Output**. Folder opening and editing
use the existing native file-manager/editor actions. Built-in labels use Fluent;
user script labels are displayed as supplied. Keyboard navigation works whether
or not the catalogue is empty. No launch control may be a dead placeholder.

Create produces a labelled local script with explanatory context comments and
a harmless example; open it in Ced. Never overwrite an existing filename.
Document the resolved folders in the app's diagnostics/help so users can find
scripts without guessing an environment override.

Allow one active macro per app instance initially. A second activation receives
an explicit busy result, without silent queueing or duplicate effects. Execution
does not block UI painting/input. Stream stdout/stderr to a bounded output view
and expose running, succeeded, failed or cancelled status with exit code and
diagnostics. Bound pending delivery and retained output by bytes as well as lines;
report truncation and keep draining the process pipes to prevent deadlock.

Provide **Stop Macro** while running. Reap the launched process on stop and app
shutdown under the shared host's shutdown policy, using native Mix/process
lifecycle facilities where needed. Stopping execution cannot undo Bus effects
already accepted or independently managed services. No automatic retry of a
failed mutation or promise of atomicity across multiple services is permitted.

Run under the normal user/session authority with the normal Mix capability
surface. Do not introduce a restricted mini-language or recurring approval step
for ordinary local macros. Context does not grant extra authority. Cross-node
application operations travel through the ABP Bus between noded instances;
SSH/HTTP/stdio relays are not replacements for missing native verbs.

## 6. Application and agent control

The visible menu and agent activation must use the same discovery, context
capture and runner. Extend each app's existing action dispatcher where it exists;
otherwise add owner-prefixed catalogue/run/status/stop verbs on that app's
existing Bus service. Do not create a second macro daemon or service identity.

Catalogue returns scope/stem identities, labels, shortcut and eligibility plus
contract version and diagnostics. Run returns a run ID or a structured refusal;
status reports observed execution state; stop targets that run ID. Never mark
completion merely because launch was accepted. Native completion events remove
the need to poll. Verb names and exact payloads must be registered with owners,
version and implementation status before landing their implementations; this
document does not declare any proposed verb already served.

Applications document meaningful domain verbs and provide at least one useful
shipped macro. Mutation examples exercise domain state and undo where supported;
read-only apps can demonstrate a report/query. One desktop-wide example must
compose two real services and check failure/results at each step. Long-lived
event handling uses Mix serve/event facilities rather than busy loops.

## 7. Compatibility and acceptance

The rollout must preserve existing Ced macro discovery, shortcuts, context,
output, busy handling and origin/undo semantics. Track outstanding legacy work
explicitly; no completed claim based solely on menu screenshots or mocks.

Required evidence:

- Discovery fixtures cover both homes, unlabelled files, malformed headers,
  eligibility, duplicate identities/labels, shortcut collisions and Ced aliases.
- Native watch tests cover edit, atomic rename, deletion, overflow and explicit
  reload; no idle polling or unnecessary redraw.
- Invocation tests cover exact argv/environment/context, stale revisions,
  two app instances, large-context refusal and no interpreter fallback.
- Real execution covers success, non-zero exit, launch failure, output flood,
  cancellation, app shutdown and mutation failure without automatic retry.
- Every app launches its example from the actual GUI entry and from its native
  action interface, with matching observable results; the UI remains responsive.
- Ced edits remain undoable in the correct lane. DOpus/Term/Cap examples prove
  their own domain behaviour rather than shell substitutions for missing verbs.
- A real multi-service macro and an authorised two-node macro prove ABP routing,
  instance identity, replies and final state through the native stack.
- The supported self-contained VT-native image supplies interpreter, scripts,
  folders and native service wiring without host desktop/session dependencies.
- New principal app components fail the application gate unless macro support,
  documented context/operations and a real acceptance example are declared and
  verified. Deferred existing apps remain explicit failing rollout work.

Add machine-checkable `*.spec.mix` fixtures and a Mix integration gate during
implementation. A declaration alone is not execution evidence. Version changes
to headers/context require compatibility and migration, not a silent rewrite.

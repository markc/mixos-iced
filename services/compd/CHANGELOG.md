# Changelog

## 0.1.20

- Add finite native `comp.output.scale`, fenced by the actual output instance
  and current topology generation. Preserve mode/transform/location ownership;
  refresh owning world placements, work areas, fractional clients and damage.
- Publish additive read-only output `instance` and `generation` inspector facts.
- Add retained real-app fractional output transition acceptance source. Native
  execution and physical mode/hotplug acceptance remain separate requirements.

## 0.1.18

- Add bounded `comp.hardware.snapshot` and incarnation-fenced
  `comp.hardware.wait` observations from the actual native libseat/libinput
  callbacks. Bus injection and nested input cannot satisfy native waits.
  Retain last-event sequences and device sysnames without storing key values.
  These observations do not claim physical device provenance, client delivery,
  resumed scanout or audible audio.

## 0.1.17

- Add strict native `comp.world.list`, `comp.world.create` and
  `comp.world.activate`, with eight retained spatial worlds per compositor.
  Initial and runtime worlds share the same systems, output and renderer owners.
- Keep dormant client geometry, tile facts and pending observations in their
  owning Space. Dormant untile preserves world membership and normal restore;
  active-world geometry verbs refuse dormant clients rather than migrating them.

## 0.1.16

- Add strict generation-fenced `comp.window.tile` and `comp.window.untile` for
  explicitly admitted active xdg clients. Persistent bounded column groups use
  actual work areas, client hints and prepared SSD metrics, and retain original
  normal restores through minimise, workspace and fullscreen/maximise overlays.
- Separate tile membership, native requested flags and client-committed tiled
  state. Complete-group infeasibility reports a pending reason; overlay exit
  restores normal geometry until a feasible current plan returns. Native tiled
  waits require a matching real client commit and current geometry.
- Share the renderer-free geometry executor across production and native
  fixtures. Work-area/output/SSD and client lifecycle events reflow the same
  owner; free placement and interactive movement refuse tiled ownership.
- Add an owned native Bus/render/input tiling acceptance gate and bounded
  testkit client hint/key controls and real button-serial move/resize requests.

## 0.1.15

- Report client-committed fullscreen in window projections and state replies.
  `configure_pending` includes fullscreen entry and exit until the acknowledged
  state commits. Native state waits retain the live pending-state fence.
- Share the production fullscreen output placement, restore and configure path
  with native protocol fixtures; preserve the decided restore size while client
  buffers lag behind compositor geometry.

## 0.1.14

- Include the compositor's actual process instance in the `comp.info` reply,
  matching its existing snapshot and region selection contract. Cap can fence
  native region capture and cleanup to the compositor that owns the request.

## 0.1.13

- Serve canonical embedded Quoin descriptions on the existing shell service,
  including without an active output. Retain original commands on a bounded
  native lane and report actual compd identity and installed presentation.

## 0.1.12

- Add owner-scoped native region cancellation. `comp.region.select` takes an
  optional strict `{instance, owner, generation}` selection identity echoed on
  terminal replies; the new `comp.region.cancel {selection}` short verb is
  admitted independently of the long pool and cancels only the exact active
  run through the existing finish path (focus restore, overlay removal, the
  original select reply). Bounded per-owner retirement watermarks refuse
  resurrected generations, and a stale compositor instance is refused without
  mutating state. Legacy selections without an identity are unchanged and
  cannot be cancelled through the verb.

## 0.1.11

- Use application's paired settings bridge for shell activation and worker
  delivery, sharing cancellation-safe progress and final cache flushing with
  native apps. Retain the existing Bus identity and independent registry lane.

## 0.1.10

- Prepare validated persistent shell settings through the shared resource/cache
  worker, including offline startup and terminal Bus refusal. Expose matching
  cache evidence in shell properties.
- Use the existing nonblocking Bus supervisor for cold startup. Advance to a
  configured scene service override only after explicit initial rejection and
  retirement; preserve an established identity through later failures.
- Drain the latest activated settings capture and close the worker under one
  two-second shutdown budget with bounded runtime completion.

- Reconcile stationary pointer routing when an applied client shape changes
  after a scene resize. Same-sized buffer repaints remain quiet; the composed
  gate forces a delayed size commit and distinguishes ACK from new content.

## 0.1.9

- Expose shell settings activation evidence through existing `shell.props.get`
  paths, preserving caller provenance and connection fences.
- Expose owner-decided slot sizes independently of client ACKs in `compd.truth`.
- Add a native rendered desktop gate covering settings-driven panel geometry,
  real xdg client sizes, captured pixels and stationary pointer routing.
- Reconcile stationary pointer focus and iced hover through the shared seat
  routing owner after changed scene placement has applied its pending sizes.
  Keep notifications through grabs, session pause and output loss without polling.

## 0.1.8

- Share the renderer-free maximise/work-area executor with protocol fixtures;
  preserve owning outputs and authoritative slots through delayed client ACKs
  and buffer commits, without redundant configures on unchanged targets.
- Defer work-area resize while fullscreen owns geometry and reconcile the
  latest maximised rectangle after its exit commit.
- Reconcile scene reservations after settings activation and before frame work,
  including sessions without the comp Bus port; redraw and retarget the window
  pointer after actual geometry changes.

## 0.1.7

- Activate global panel modes and requested thickness with prepared shell
  appearance. Settings-owned edges report requested, fitted, presented and
  reserved dimensions; conflicting legacy writes refuse `SETTINGS_MANAGED`.
  Local saved modes and settled sizes survive profile removal and page saves.
- Requested maximised windows retain their output and only follow changes to
  that output's work area. Removing the output selects an available output
  while preserving the original unmaximise geometry.

## 0.1.6

- Scene loader adds versioned native editor snapshots and guarded actions,
  reusing its existing planner and reducer. Normal editor activation routes
  the independent Scene Editor app; `SCENES_EDITOR_APP=0` retains the legacy
  scene frontend. No scene files or editor user copies are removed.
- Passive loader reload preparation retains complete scene entries, so the
  committed mount receives an integer generation fence before behaviour starts.

## 0.1.5

- Restore hover and pressed feedback on compositor-owned iced controls by
  delivering the frame event to rebuilt views before painting. Pointer
  damage stays scoped to its receiving surface and stops when input stops;
  frame-generated messages and scheduled animation wakes are retained.

## 0.1.4

- Reuse toolkit's `CenteredButton` and centred-content helper for compact
  scene targets. Workspace labels retain their corrected placement and hit areas.

## 0.1.3

- Centre panel workspace labels inside their full hit targets. The iced
  scene renderer positions intrinsic content in a bounded wrapper instead
  of compressing centring spacers to zero inside minimum-sized rows.

## 0.1.2

- Extend `comp.capture.frame` with cursor inclusion, output-local logical
  regions and output-generation fences. Existing defaults remain compatible.
- Cursorless file captures share the existing screencopy render paths on
  native KMS, nested and inactive-VT renderers.

## 0.1.1

- Native frame and fenced-window capture with bounded completion replies.

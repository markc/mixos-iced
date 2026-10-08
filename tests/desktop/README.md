# Desktop-tier gates

`ced_settings_gate.mix REPO BUILD_BIN_DIR` checks real Ced settings frames,
matching applied/cache receipts, document/caret preservation, cached and embedded
cold startup, a 65-second offline broker interval and recovery on the same window.
It uses separate owned compositor and app brokers, named candidate binaries,
Weston/pixman and software GLES. `settings_geometry_gate.mix` checks composed
shell reservations, native client configure/commit, captured palette pixels and
stationary pointer routing, including a forced delayed-buffer ACK.

`application_native_gate.mix --bin-dir PATH` exercises Ced, DOpus and Term
using named candidate binaries, a private ABP broker and nested compositor.
It checks editor layout/input, file copy/cancellation and real terminal PTY
output and native enrolment of both Mix PTYs, with captures at scales 1 and
2.5. It requires `bwrap`, `setpriv` and non-interactive `sudo` for an isolated
candidate shell binding that preserves broker ownership verification. Term
runs as the invoking user after privileges are dropped. The gate never
restarts an existing desktop session.

Mix scripts that run compd for real and judge it by evidence: Bus replies,
observation topics, frame traces and screenshots. Nothing here touches a live
desktop. Each run starts its own headless weston host (`nested_host_lib.mix`,
or the equivalent setup inside the standalone scripts) and nests compd inside
it, so a build worker with no DRM node and no desktop can run every gate.

Two kinds of script:

- **Harness gates** (on `nested_host_lib.mix`) take `--bin PATH` (default
  `/opt/compd/bin/compd`) and `--config-file PATH` (compd's settings file):
  `desktop_nested_gate` (with its waybar, fuzzel and xmessage arms),
  `comp_control_smoke`, `comp_workspaces_gate`, `comp_agent_seat_gate`,
  `comp_presentation_smoke`, `comp_region_select_gate`,
  `client_checklist_gate`.
- **Standalone gates** take the compd binary as their first argument and
  accept `--scale S` to run the nested output at a fractional scale:
  `nested_smoke`, `chrome_gate`, `scene_gate`, `screencopy_gate`;
  `idle_probe` takes no scale.

Gates whose subject is a MixOS application rather than compd (the editor, the
file manager, the panel host and its scenes) return with each application.

`toolkit_gallery_gate.mix` runs the generic widget toolkit's gallery
(`libs/toolkit`, `cargo build -p toolkit --example gallery --features
gallery-wgpu`) inside a nested compd, waits for its window, places it and
captures it (`--bin`, `--config-file`, `--noded`, `--no-build`). It is the desktop-side
evidence for toolkit; toolkit's own tests never start a compositor.

Helpers: `nested_host_lib.mix` (the shared harness), `bus_watch.mix` and
`panel_holder.mix` (`mix --serve` helpers the smokes start), the HTML fixtures
and `smoke-settings.json` (the settings file compd is started with).

Binaries and paths the gates rely on:

- `mix` and `noded` in `/opt/mixos/bin`; compd and the testkit probes
  (`testkit-input-probe`, `testkit-presentation-probe`, `testkit-screenshot`,
  …) beside the compd binary under test, else in `/opt/compd/bin`.
- The broker URL from `MIXOS_NODED_URL`, else `~/.config/mixos/node.conf.mix`
  or `/etc/mixos/node.conf.mix`, else loopback. A gate with fixed Bus names
  starts a private `noded` (`--noded PATH` overrides the lookup).
- compd's per-run state through `MIXOS_ETC`, `MIXOS_VAR` and `MIXOS_RUN`, and
  its own switches `COMPD_FRAME_TRACE`, `COMPD_FRAME_TRACE_FILE`,
  `COMPD_FRAME_TRACE_LIMIT`, `COMPD_CHROME_CLIP_DEBUG`, `COMPD_XWAYLAND`.
- Seats are `seat0` (human) and `agent`; decoration styles are `mac`, `win11`
  and `mixos`; probe app IDs are `dev.mixos.*`; the presentation probe's
  stdout summary line starts `COMPD_PRESENTATION_PROBE `; the Xwayland
  descriptor is `$XDG_RUNTIME_DIR/compd/<socket>.xwayland.env`.

Run one gate from the repo root, so the fixtures and helpers resolve under
`tests/desktop/` (when installed beside compd they resolve from
`/opt/compd/bin` instead). Each script's header lists its arguments; a run
with none, or a bad one, prints the usage line:

```
mix tests/desktop/desktop_nested_gate.mix --bin target/release-fast/compd --config-file tests/desktop/smoke-settings.json
mix tests/desktop/nested_smoke.mix target/release-fast/compd --bus --scale 2.5
```

Or run the whole suite, every gate (the scaled ones at each `--scales` entry),
with one PASS/FAIL table at the end:

```
mix tests/desktop/suite.mix [--only a,b] [--skip a,b] [--scales 1,2.5] [--bin PATH] [--config-file PATH] [--json]
```

Each script's header documents its checks and exit codes.

The standalone `scene_editor_settings_gate.mix` owns separate compositor and
application brokers. It checks actual light/dark frames, selection and dialogue
retention, checked cache and embedded fallback, a 65-second cold outage with
same-window recovery, initial collision forwarding and bounded shutdown. Run
it with the repository root and a combined build's binary directory:

```
mix tests/desktop/scene_editor_settings_gate.mix /path/to/mixos /path/to/mixos/target/release-fast
```

It uses `settings_host_lib.mix` with the Ced settings gate. These settings gates
run independently of `suite.mix`; they require `settingsd`, `noded`, the app,
compd and the testkit binaries. Scene authority mutation and complete shell
geometry are separate checks.

`term_settings_gate.mix REPO BUILD_BIN_DIR` requires acceptance-enabled Term
and the named candidate Mix, noded, settingsd and compd binaries. It shares the
owned PTY mount-namespace launcher with `application_native_gate.mix`. All
control uses the owned ABP broker. The gate checks ordinary fixture omission,
actual Iced root layout, exact installed-stamp native presentation receipts,
held local zoom retaining its raster/stamp, release, authority activation on
the same window, and bounded shutdown while preparation is held. This is
nested software-compositor evidence; physical GPU scanout and VT isolation
remain separate checks.

`settings_app_recovery_gate.mix REPO BUILD_BIN_DIR [APP]` shares one cold-start
and authority-rejoin matrix across Term, BusViewer, Cap and Dopus. `APP` can
select `term`, `busviewer`, `cap` or `dopus`; omission runs all four. Use a
combined exact-commit binary directory with acceptance enabled for each app,
tiny-skia app rendering, and candidate Mix, noded, settingsd and compd. Term
also needs the owned PTY launch support documented above.

```
mix tests/desktop/settings_app_recovery_gate.mix /path/to/mixos /path/to/candidate/bin
```

The gate primes a real persisted cache, retires the app and settings authority,
then starts cached, missing-cache and malformed-cache cases. The app broker
remains live while the authority is absent; the compositor has its own broker.
Every cold app must present within 1000 ms, install unconfirmed fallback evidence,
and return an exact installed native frame receipt and real root layout.
Cached authority/resource identities must match the primed sources; a corrupt
cache must report fallback diagnostics. Authority restart applies a distinct
light/text-scale design and must install its live snapshot and the same resource
sources on the same PID/window, retaining actual PTY output and owners,
selected successful BusViewer call, Cap annotation/undo,
or Dopus selection. Per-case JSON artefacts include startup timing, descriptions,
resource sources, layouts, exact frame receipts and before/after product state.
Registry usage counters are recorded in descriptions but are not source identity.
Dopus icon keys include their prepared RGBA tint. Changed-authority comparisons
normalise only that exact suffix and collapse duplicate identical source
selections while retaining every icon selection/source field. The native
receipt can retain both old and new prepared tints. Cached and disconnected
comparisons keep the complete prepared key and receipt.
Set `COMPD_KEEP_RUN=1` through Mix's environment options to retain passing
artefacts; failing runs are retained automatically.

This first matrix does **not** prove a broker outage longer than 65 seconds,
invalid/missing resource retention or stale held preparation retirement. Those
remain required extensions, with the existing individual native settings gates
providing their separate short reconnect and preparation-hold checks. A passing
matrix cannot stand in for those remaining cases or physical VT/GPU acceptance.

`settings_app_outage_gate.mix REPO BUILD_BIN_DIR [APP]` uses the same product
adapters and owned retirement as the cold matrix. Its exact candidate directory
also needs `testkit-screenshot`. Omitting APP launches all four together and
runs one app-broker outage longer than 65 seconds:

```
mix tests/desktop/settings_app_outage_gate.mix /path/to/mixos /path/to/candidate/bin
```

The gate primes current authority and real product state, then stops settingsd
and the app broker. The separate compositor broker stays alive. During the
outage it checks each actual PID/window owner at both ends, polls process
liveness, and saves native screenshot artefacts with each app focused. App
descriptions and fixture verbs are deliberately unavailable during this period;
the screenshots require visual review and do not assert pixel equality.
After restarting the same broker endpoint, and before restarting settingsd,
every app must report unconfirmed LastGood with unchanged installed authority,
resource sources and product state. Every fixture generation must advance and
reject the prior generation. Exact native frame receipts and actual root layout
must still belong to the same PID/window. Authority then rejoins and must be
confirmed current with the same resources and retained products. An identical
authority design can confirm the existing activation without replacing it.
Cap saves its dirty document only after all retention checks; Term uses bounded
controlled signal retirement rather than claiming natural quit.

Required run evidence is the root/per-app acceptance JSON, descriptions,
resource receipts, before/reconnected/rejoined products, exact frame receipts,
layouts, and native before/offline-start/offline-end screenshots. Passing lint
does not constitute native acceptance.

Invalid/missing resource acceptance remains separate: apply a structurally valid
resource reference whose set is absent, then one whose manifest digest is wrong;
observe the rejected desired authority identity and diagnostic while installed
authority/resources/stamp and product remain LastGood, followed by successful
restoration and exact native presentation. Stale preparation remains separate:
hold each app's real preparation barrier, supersede its authority or connection
generation, release it, and prove the held candidate never installs using an
observable activation history, then present the winning current stamp. Merely
observing the final current snapshot cannot prove that intervening candidate
was never installed. Neither remaining case is claimed by the outage matrix.

`settings_app_resource_stale_gate.mix REPO BUILD_BIN_DIR [APP]` adds real
missing-set and wrong-manifest-digest rejection, then superseded held authority
preparation for the same four apps. It needs the candidate binaries and native
PTY support described for the cold matrix; no network fetch or screenshot tool
is needed:

```
mix tests/desktop/settings_app_resource_stale_gate.mix /path/to/mixos /path/to/candidate/bin
```

The shared asset helper invokes the candidate Mix's native
`share/assets/install.mix --root OWNED_ROOT --manifest share/assets/core.conf.mix`,
then `--verify`. Installation may fetch the immutable manifest's approved
payloads; failures stop the gate. A native XDG asset root selects this complete
core package. The actual GUI must report its exact non-null set/manifest and
selected source identities before failure tests. Each text record must select
its expected declared family/face and locked font digest without substituting
the requested weight. Each icon must match the real Material Symbols catalogue
name, glyph, family, default style/weight and locked icon-font source digest;
its asset descriptor must be absent. Dopus must report all 35 required names.
The package receipt retains the complete manifest/file table and the actual
installed catalogue's required glyphs. Installer verification alone is not
evidence that a GUI selected those sources. The strict owned SVG decoder is
unchanged; bundled indirect-painting Lucide SVGs are not used as accepted
resource fixtures.

Both rejected resource references are structurally valid authority mutations.
The absent set must report the actual unavailable-set diagnostic; a wrong
digest against the existing verified package must report the actual manifest
digest diagnostic. The desired current authority must name the rejected
snapshot while applied authority, complete prepared resource receipts, product
state and exact installed native stamp remain LastGood. Each reset must restore
confirmed current authority on the same PID/window with exact native frame and
layout receipts. Preparation-current is not required for a failed desired
resource snapshot.

For stale preparation, each real app worker is held separately because the
shared barrier expires after 10 seconds. Its captured observation must name
the held snapshot. A second authority snapshot supersedes that work while the
old installed stamp/resources/product remain unchanged. Explicit release must
occur before expiry. The winning snapshot must then present on the same window
with an activation epoch exactly one greater than the baseline. Production
Session increments the checked epoch on every installed replacement, so even
a transient installation of the superseded snapshot fails this proof. Local
context preparations do not increment that authority activation epoch.

Run evidence includes the owned package and lock manifest, verified baseline
descriptions/resource receipts, distinct desired/applied rejection diagnostics,
exact old-frame receipts while rejected/held/superseded, winning current frame
and root layout, retained products, and per-app/root acceptance JSON. This gate
tests authority supersession of held work; a held candidate cancelled by a
replacement connection generation remains a separate schedule. Physical GPU
scanout and VT isolation also remain separate acceptance.

`settings_app_generation_cancel_gate.mix REPO BUILD_BIN_DIR [APP]` covers a
real held preparation crossing an owned app-broker connection replacement.
It uses the same native complete-core installation, four real products and
independent compositor broker:

```
mix tests/desktop/settings_app_generation_cancel_gate.mix /path/to/mixos /path/to/candidate/bin
```

All four GUI owners stay alive while each app gets a separately bounded
old/fresh hold schedule. The old worker reaches B with the actual revision
observation and leaves the exact baseline installed. The gate stops authority
and restarts the app broker at the same endpoint, then requires unconfirmed
LastGood with the exact old native target, prepared sources and product state.
The live fixture generation must advance, and old frame/state/wait/release
references must refuse that generation. A successful fresh arm before eight
seconds proves the old Reached hold became terminal before its ten-second
automatic expiry; the actual actor closes it on LostGeneration.

Authority return exposes persisted B. A fresh real-worker hold captures that
new-generation B before it can install, then C supersedes it and must be
observed while the original baseline remains installed. Explicit release must
occur before expiry. C must become confirmed current on the same PID/window
with verified complete-core sources, retained product, real root layout and
an exact native frame whose activation epoch is precisely baseline plus one.
An installation of either old or fresh superseded B would add another epoch
and fail. The second hold avoids racing a legitimate new-generation B install
against the online C mutation.

Artefacts retain both actual reached observations, old-generation refusals,
absent/held/superseded descriptions and exact old-frame receipts, baseline and
winning frame/layout/product state, owned package manifest/catalogue identities,
unique authority logs and per-app/root acceptance JSON. This is a short held
connection schedule; the separate outage gate supplies the >65-second outage.
No synthetic activation or frame is introduced. Cap exports only after retained
state checks; Term retirement remains controlled signals. Native installation,
PTY support and source-identical binaries are required. Physical VT/GPU/input
acceptance remains separate.

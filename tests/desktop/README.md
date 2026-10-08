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
Set `COMPD_KEEP_RUN=1` through Mix's environment options to retain passing
artefacts; failing runs are retained automatically.

This first matrix does **not** prove a broker outage longer than 65 seconds,
invalid/missing resource retention or stale held preparation retirement. Those
remain required extensions, with the existing individual native settings gates
providing their separate short reconnect and preparation-hold checks. A passing
matrix cannot stand in for those remaining cases or physical VT/GPU acceptance.

# Desktop-tier gates

`application_native_gate.mix --bin-dir PATH` exercises Ced, DOpus and Term
using named candidate binaries, a private ABP broker and nested compositor.
It checks editor layout/input, file copy/cancellation and real terminal PTY
output, with captures at scales 1 and 2.5. It requires `bwrap` for an isolated
candidate shell binding and never restarts an existing desktop session.

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

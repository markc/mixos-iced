# compd

Native desktop world control is documented in
[`native-worlds.md`](../../docs/spec/compd/native-worlds.md). Bounded runtime
worlds share the initial world's native systems and renderer. New clients map
into the selected spawn world; existing clients retain their native home.

The MixOS Wayland compositor.

compd owns KMS scanout, the GLES renderer and one shared wgpu device,
in-compositor iced surfaces, the Xwayland window manager, frame pacing,
fractional scale, input devices and capture. On top of that sits the MixOS
policy layer: the agent seat, the `comp` Bus service (verbs, topics and the
props tree), the frame and presentation ledgers, panel holders and hot
corners, and strict idle discipline (no frames on a static screen).

Bevy, when enabled, may render effects on the shared wgpu device. It never
owns the frame loop, scheduling, compositing or protocol handling.

Quoin prepares cached or embedded settings while its existing Bus supervisor
registers. Its persistent cache is service-owned under the configured variable
data directory at `compd/cache/settings`; all validation and filesystem work
uses the shared application worker. `shell.props.get` exposes `settings` and
`settings_cache` evidence. A cache persistence receipt must match the current
applied identity before it describes that presentation. Transport outages and
terminal registration refusal preserve usable appearance. Only explicit initial
refusal permits the configured scene service override; an established shell
identity never switches names. Shutdown drains the newest activated capture
and closes the worker under one two-second budget with a bounded completion
margin. Timeouts report failure without claiming persistence.

## Layout

| Path | What |
|---|---|
| `src/` | the `compd` binary: CLI, event loop, Bus and scene wiring |
| `crates/` | compd's private crates (engine, protocols, policy, backends) |
| `layers.conf.mix` | the private crates' dependency order, checked by `tools/layering_gate.mix` |
| `tests/` | integration tests |

The desktop-tier gates that run compd nested or on hardware live in
`tests/desktop/` at the repository root.

## Build

```
cargo build --profile release-fast -p compd                     # nested backend
cargo build --profile release-fast -p compd --no-default-features \
    --features backend-native                                   # KMS backend
cargo build --profile release-fast -p compd --features backend-all   # both
```

`capture-ffmpeg` adds hardware video capture through libav; the default build
links no FFmpeg.

`desktop-dbus` explicitly enables the optional desktop compatibility adapters.
The default native session has no D-Bus client or portal dependency. It passes
environment directly to children; delegated cgroup placement before exec and
`cgroup.kill` provide complete cleanup when `MIXOS_CONTAIN_CHILDREN=1`. See
[`docs/native-session.md`](../../docs/native-session.md) for the profile and gates.

## Run

Nested, inside another Wayland session:

```
compd --nested --socket wayland-compd --config-file=settings.json
```

`--nested-size WIDTHxHEIGHT` requests the initial nested window size in logical
output pixels and requires explicit `--nested`. For example,
`--nested-size 1536x864 --scale 2.5` requests a 3840×2160 physical window.
The host may constrain the real window; query the actual output rather than
assuming the request was honoured. Omitting the size preserves the existing
backend default. This option does not apply to KMS outputs.

On a VT, through the seat (libseat/seatd):

```
compd kms-live --device /dev/dri/card1 --connector HDMI-A-1 --scale 2.5
```

compd waits for its own VT to become active and never switches VT itself.
`--bus-service NAME` and `--scene-service NAME` override the Bus names
(default `comp`); `compd --help` lists every flag. Settings are read from
`--config-file`, else `~/.config/compd/settings.json`.

## Window tiling

`comp.window.tile {id, generation, output?}` explicitly admits an active xdg
window into its output/workspace column group; ordinary new windows stay free.
`comp.window.untile {id, generation}` removes membership and restores its original
normal rectangle. Groups are bounded to 256 members and preflight every cell
against actual layer/panel work areas, committed client size hints and prepared
SSD extents. Output loss selects the first sorted mapped output. Minimise/unmap
suspend participation, workspace moves transfer the group, and destruction or a
new role/UUID generation retires membership. Fullscreen and maximise are overlays.

`windows.s<id>.requested_tiled` means membership; `native_requested_tiled` and
`tiled` mean requested and client-committed native flags. `tile_pending_reason`
reports a complete-group failure. A pending overlay return restores immutable
normal geometry with tiled flags clear, retaining membership for later reflow.
`compd.truth.tiles` carries group/order-independent identity and normal restores.
`comp.window.wait {until:"tiled"}` requires active current-workspace membership,
committed flags and the latest decided tile size; ACK alone cannot satisfy it.
`until:"untiled"` requires removed membership, committed clear flags and client
geometry matching the current decided slot. An active overlay restores normal
geometry on its subsequent exit. Use
`untile` before free placement or interactive move/resize. X11 tile admission is
refused because it cannot express the native tiled state contract.

See [the tiling contract](../../docs/spec/compd/window-tiling.md).

## Test

```
cargo test --workspace
mix tests/desktop/nested_smoke.mix --help
mix tests/desktop/parity_suite.mix --help
```

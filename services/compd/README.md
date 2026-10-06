# compd

The MixOS Wayland compositor.

compd owns KMS scanout, the GLES renderer and one shared wgpu device,
in-compositor iced surfaces, the Xwayland window manager, frame pacing,
fractional scale, input devices and capture. On top of that sits the MixOS
policy layer: the agent seat, the `comp` Bus service (verbs, topics and the
props tree), the frame and presentation ledgers, panel holders and hot
corners, and strict idle discipline (no frames on a static screen).

Bevy, when enabled, may render effects on the shared wgpu device. It never
owns the frame loop, scheduling, compositing or protocol handling.

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

On a VT, through the seat (libseat/seatd):

```
compd kms-live --device /dev/dri/card1 --connector HDMI-A-1 --scale 2.5
```

compd waits for its own VT to become active and never switches VT itself.
`--bus-service NAME` and `--scene-service NAME` override the Bus names
(default `comp`); `compd --help` lists every flag. Settings are read from
`--config-file`, else `~/.config/compd/settings.json`.

## Test

```
cargo test --workspace
mix tests/desktop/nested_smoke.mix --help
mix tests/desktop/parity_suite.mix --help
```

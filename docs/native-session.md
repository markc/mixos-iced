# Native isolated session

The initial native session profile builds `compd` with `backend-all` and
without `desktop-dbus`. ABP/noded carries application requests, discovery,
session grants and mesh routing. The session owns its seatd instance, XDG
runtime directory and a private compatibility session bus. It inherits no host
session bus or host activation environment. `session-dbus@.service` owns the
daemon and socket; its readiness helper checks the actual bus ID before
publishing the final environment file. Citizens load that file after their
profile, overriding any inherited host address and clearing system/starter bus
variables. The daemon receives the owned profile's runtime and data lookup
context for compatibility activation. Application control stays on ABP.

`desktop-dbus` is an explicit compatibility feature for RTKit, logind, MPRIS,
portal Save As and systemd user scopes. It is disabled by default. The native
profile retains direct CPU scheduling, DRM output control, native audio volume
and ordinary capture Save. Player transport, system suspend and portal Save As
report unavailable until their native owners/adapters are connected.

Applications receive the session environment directly at launch. No user-wide
environment is published or retracted by the native profile.

The image carries its own Material Symbols Rounded SVG icon theme and the
canonical M launcher mark. Install both into the session's shared data tree;
no host icon package or runtime download is needed:

```text
mix share/icons/install.mix --root /opt/mixos/share
mix share/brand/install.mix --root /opt/mixos/share
mix share/icons/install.mix --root /opt/mixos/share --verify
```

Include that root in `XDG_DATA_DIRS`. The apps registry selects the installed
MixOS theme; symbolic SVGs take the foreground colour and scale with the output.
`apps.reload` rescans entries and clears icon-theme misses after an asset update.

For complete child cleanup, the OS supervisor delegates the session's cgroup
and sets `MIXOS_CONTAIN_CHILDREN=1`. compd creates a fresh apps subgroup and
attaches children before exec. Kernel `cgroup.kill` collects the entire subtree,
including descendants that double-fork or change process group. Missing
delegation or kernel support is a launch failure. The supervisor must also use
`KillMode=control-group` for crash and timeout cleanup.

Build and dependency gates run on a build worker:

```text
cargo build --locked --profile release-fast -p compd --no-default-features --features backend-all
mix tools/native_session_gate.mix
cargo build --locked -p slots --example containment-probe
```

The Cargo gate checks the native Rust dependency closure. The private session
daemon is supplied by the image. `tests/desktop/session_bus_gate.mix` separately
tests the production unit/helper in an explicitly enabled, owned root worker
fixture: final environment precedence, actual socket/PID/cgroup identity,
independence from native broker restart, daemon replacement and target teardown.
Its root account and candidate paths are fixture substitutions; it starts no
desktop and changes no VT. This check does not establish image or hardware
readiness.

Run `containment-probe` inside a delegated test service with containment enabled
to verify actual descendant cleanup. Graph checks and nested rendering do not
prove VT handover: GPU acquisition, physical input, scan-out, audio and rollback
are separate hardware acceptance gates.

## Fullscreen state evidence

Window projections report client-committed fullscreen. A fullscreen or
unfullscreen request stages compositor intent; `configure_pending` remains true
while requested maximise or fullscreen differs from committed state. For xdg
clients, acknowledging a configure alone does not commit it: a subsequent
surface commit applies the acknowledged state. Native fullscreen state waits
use this same fence. X11 has no xdg ACK fence and reports its server-side state.

Run `mix tests/desktop/fullscreen_state_gate.mix REPO BUILD_BIN_DIR` against a
matching compd and testkit build. The gate owns its nested host and native
broker, checks delayed entry and exit ACKs against state-wait timeouts, then
verifies committed geometry, exact restore, pixels, input and stale-generation
refusal. The probe's `--delay-state-commit WxH:MS` leaves ACKed state uncommitted
until its replacement buffer; `--delay-size-commit` retains its existing policy
of committing ACKed state immediately with the old buffer. These nested checks
do not replace physical VT, GPU, input or seat acceptance.

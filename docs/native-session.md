# Native session without D-Bus

The initial native session profile builds `compd` with `backend-all` and
without `desktop-dbus`. ABP/noded carries application requests, discovery,
session grants and mesh routing. The session owns its seatd instance and XDG
runtime directory. It inherits no session bus or host activation environment.

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

Run `containment-probe` inside a delegated test service with containment enabled
to verify actual descendant cleanup. Graph checks and nested rendering do not
prove VT handover: GPU acquisition, physical input, scan-out, audio and rollback
are separate hardware acceptance gates.

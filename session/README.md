# Native VT session

`session@4.target` composes component-owned units for noded, compd, editd, settingsd,
the application registry, the scene loader and a private seatd instance.
It starts no session D-Bus server. Application control uses ABP through noded.
The compositor is built with `--no-default-features --features backend-all`;
`desktop-dbus` is an explicit compatibility feature.

Run `mix tools/collect_units.mix` to collect units under `target/mixos/units`.
Run `mix tools/collect_apps.mix` to collect desktop entries under
`target/mixos/share/applications`; install them in `/opt/mixos/share/applications`
and include `/opt/mixos/share` in the session's `XDG_DATA_DIRS`. A profile
that restricts that variable to its own application catalogue can set
`XCURSOR_PATH=/opt/mixos/share/icons:/usr/local/share/icons:/usr/share/icons`
to find the image's cursor themes separately. These paths refer to the
image's filesystem. Compd falls back to its built-in arrow if no usable
theme image can be loaded; explicitly hidden cursors and client-supplied
cursor surfaces retain their behaviour.
The collector never installs or starts them. An image installer supplies the
`mixos` account, the `seat` group and `/etc/mixos/session/4.env`. Its account
must have the numeric identity selected for the desktop profile; a controlled
test image can override `User=` in its private unit drop-ins.

Seatd listens at its built-in `/run/seatd.sock`; it has no `-s` option.
Each instance gets a private mount view of `/run`, backed by
`/run/mixos/seat/<instance>`, so `SEATD_SOCK` must name
`/run/mixos/seat/<instance>/seatd.sock` for its clients. This leaves other
seat brokers' sockets alone. The image supplies `seatd`, `wpctl` and `pactl`;
audio uses the selected native PipeWire/Pulse sockets. No session D-Bus
server is required for this profile. A distribution's `libpulse` can still
link `libdbus-1`; absence of a session bus does not imply a library-free
dependency closure.

Provision the profile's writable state directory for its service account,
including `MIXOS_VAR/edit/recovery` with mode `0700`. Editor readiness must
include `edit.info` reporting `volatile:false` and `recovery.ok:true`.
Also provision the service account's writable `MIXOS_ETC/settings` parent.
The installer explicitly provisions the default profile with
`settingsd seed --allow-create --instance example` as the service account;
session startup uses plain seed to validate existing state and refuses a wholly
missing directory, missing primary or unsupported data. Its
instance binding is the machine hostname, while the unit number identifies the
session's VT. The target wants/upholds settingsd without making applications or
the compositor wait for it: degraded startup is allowed. Native GUI settings
adapters remain in development; adding the unit does not enable live theming.
The native image has one session target per machine instance. Several desktops
beside the host use separately named machines and separate config/runtime roots;
the host desktop's preferences are not the authority store. Sharing a profile
across consumers means using its existing single writer and broker, rather than
starting another settingsd unit against the same root.

The environment file defines absolute `MIXOS_ETC`, `MIXOS_VAR`, `MIXOS_RUN`,
`MIXOS_SHARE`, `MIXOS_NODE_CONFIG`, `MIXOS_COMPD_CONFIG`, `HOME`,
`XDG_RUNTIME_DIR`, `WAYLAND_DISPLAY`, `SEATD_SOCK`, `MIXOS_OUTPUT_SCALE` and
`MIXOS_BROKER_ACCOUNT`, with a PATH beginning `/opt/mixos/bin`. The node config
pins its unique name and Unix socket inside this runtime directory. Compd's
`preferences.json` beside its settings file enables `scene_host`; the loader's `SCENES_DIR` holds the profile's
private scene state, and `SCENES_TEMPLATES` points to the shipped catalogue.
Every service clears inherited D-Bus variables even if the environment file
sets them. The compositor's VT comes from the unit instance, not a host seat.

Each app-launching service owns its descendants through a delegated cgroup.
Stopping the target stops its services and app descendants. Before switching
VTs, the operator prepares a bounded rollback to the VT that was active,
checks ABP readiness and keeps the previous session available. Nested tests
and actual GPU/input/audio acceptance are recorded separately.

The target uses systemd's `Upholds=` relationship (systemd 249 or newer) to
recover stopped members while the session is active. A broker crash leaves
compd running so its native client can reconnect; a seatd or compositor crash
recovers the dependent services. Stopping the target ends this recovery.
Target activation alone is not a readiness verdict: the operator must check
the broker, compositor and scene ports through ABP within the startup deadline.

Set `MIXOS_RUN=/run/mixos/session/4` and
`XDG_RUNTIME_DIR=/run/mixos/session/4/desktop`. Noded owns only the `noded`
child directory and compd owns only `desktop`. Restarting the broker therefore
preserves the compositor's Wayland socket and the clients' runtime directory.

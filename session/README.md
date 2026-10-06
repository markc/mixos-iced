# Native VT session

`session@4.target` composes component-owned units for noded, compd, editd,
the application registry, the scene loader and a private seatd instance.
It starts no session D-Bus server. Application control uses ABP through noded.
The compositor is built with `--no-default-features --features backend-all`;
`desktop-dbus` is an explicit compatibility feature.

Run `mix tools/collect_units.mix` to collect units under `target/mixos/units`.
Run `mix tools/collect_apps.mix` to collect desktop entries under
`target/mixos/share/applications`; install them in `/opt/mixos/share/applications`
and include `/opt/mixos/share` in the session's `XDG_DATA_DIRS`.
The collector never installs or starts them. An image installer supplies the
`mixos` account, the `seat` group and `/etc/mixos/session/4.env`. Its account
must have the numeric identity selected for the desktop profile; a controlled
test image can override `User=` in its private unit drop-ins.

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

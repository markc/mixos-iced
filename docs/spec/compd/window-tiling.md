# Native window tiling — compd 0.1.16

`comp.window.tile` requires unsigned `id` and `generation`, and accepts
an optional output key or native name. `comp.window.untile` accepts only the same
required identity. Unknown fields, incomplete identities and invalid types refuse
before mutation. Native generation resolution precedes output selection or any
configure. A session lock refuses both verbs. Tile admission supports active
mapped xdg toplevels with protocol version 2 or newer only; older clients cannot
receive the tiled edges and refuse `tiled_state_protocol`. It never maps a dormant
world's client into this one.

The compositor owns ordered output/workspace groups with at most 256 members
across the whole owner.
No setting invents membership and ordinary mapping never admits a client. A
complete deterministic column allocation uses real output logical geometry after
layer-shell exclusive zones and compositor panel reservations, committed client
size hints and target-normal SSD extents. Every content size is positive, columns
conserve odd widths and the whole plan is refused if any cell cannot fit. Admission
is atomic; repeating it is idempotent and output transfer preserves original normal
restore and order. Output loss falls back to the first sorted real mapped output.

Minimise and null-buffer unmap suspend participation. Restore/remap reuses order.
Untile accepts the same live generation while minimised, unmapped or stored in a
dormant world. It removes membership and restores geometry in the window's owning
Space; it never transfers the window into the active world.
Workspace transfer keeps original normal geometry; role destruction or generation
replacement retires membership. Fullscreen fills the selected whole output and
maximise fills its usable area. These overlays temporarily release group space;
untile during an overlay leaves that overlay in control and restores normal
geometry when the overlay subsequently exits.
An overlay exit with membership retained returns directly to the current tile allocation. If that complete plan is
infeasible, exit restores the immutable normal rectangle with native tiled flags
clear while membership and a pending reason remain. A feasible later event-driven
refresh rejoins the current group. No timer, separate transport or rectangle-only
facade performs placement.

The state reply and `windows.s<id>` expose `requested_tiled` (membership),
`native_requested_tiled` (four requested xdg tiled edges), `tiled` (four committed
edges), and `configure_pending`. Window rows also expose `tile_pending_reason`:
null or `no_output`, `invalid_area`, `capacity`, `invalid_constraints`,
`insufficient_area`. Tile replies add `tile_group` (native output name, workspace,
original normal rectangle) and `tile_pending_reason`; `compd.truth.tiles` provides
the same owner evidence beside actual native flags. These fields do not claim a
rendered tile or presentation receipt.

`comp.window.wait` adds `until:"tiled"` and `until:"untiled"`. Tiled waits require
active current-workspace membership, a feasible complete plan, requested and
committed tiled flags, and client geometry matching the current decided slot.
Untiled waits require removed membership, committed clear flags and client
geometry matching the current decided slot (an active overlay retains its slot).
Real tile participants retain
this geometry fence after removal; ordinary never-tiled clients keep their existing
state semantics. ACK without
a real surface commit, an older-sized buffer and a pending normal fallback cannot
satisfy tiled waits. Existing output/window generation fences and native transport
remain unchanged. Free placement and interactive move/resize refuse tiled ownership;
untile first. X11 admission is refused rather than reporting unsupported native flags.

Native protocol guards live in `testkit/tests/tiling_owner.rs`; pure allocation
and lifetime guards live in `policy/src/tiling/tests.rs`. These prove protocol
state, geometry and lifecycle, not hardware pixels or native presented frames.

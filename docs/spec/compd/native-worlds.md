# Native desktop worlds (compd 0.1.17)

These strict additive Bus verbs use the existing bounded engine control lane.
They create actual native spatial worlds, independent from virtual workspaces.

- `comp.world.list {}` returns `active`, `spawn_target`, `limit` and `worlds`.
  Each world has a canonical UUID `id`, native `name`, active/spawn flags and
  actual stored `windows`. Window evidence includes native id/generation/UUID,
  PID, mapped/minimised flags, local Space location, decided slot and immutable
  tile normal restore. Under session lock window evidence is redacted.
- `comp.world.create {}` creates one dormant native desktop and returns its UUID.
  The initial main world counts towards the finite limit of eight. At capacity
  `world_capacity` refuses before allocating systems, outputs or renderer state.
  Worlds remain for the compositor lifetime; there is no deletion or persistence
  contract. Creation does not activate or change where new clients map.
- `comp.world.activate {id:"canonical-hyphenated-uuid"}` atomically selects a
  known spatial world as active and spawn target. Unknown UUIDs are refused.
  Repeating the current world is idempotent. Session lock, exclusive seat/layer,
  region selection and interactive pointer ownership refuse the change. The
  existing native world-switch event owns focus, pointer and foreign-toplevel
  reconciliation; the reply does not claim a presented frame.

New native clients map into the selected spawn world's real Space. Existing
clients retain their owning world, UUID, generation, local placement and restores
when another world activates. No window reassignment verb is introduced. Initial
and runtime worlds use identical native systems, existing real output mappings
and the shared renderer. There is no separate broker, service or transport.

`comp.window.untile` can remove the same live member while its owning world is
dormant. It restores that world's actual local normal rectangle and clears its
native requested tiled flags without activating, focusing or migrating it.
Tile admission, free placement and requested fullscreen/maximise geometry need
the active world. Dormant window projections report invisible, while their
geometry and native state are read from their owning Space. Tiled group planning
and pending observation remain scoped to actual Space participants.

Worlds share the compositor's existing output/workspace/settings authorities;
this contract does not add per-world settings, persistence, navigation UI,
multi-seat session separation or a new VT. The VT-native self-contained session
requirements remain unchanged. Nested readback proves compositor behaviour,
not hardware scanout or host VT isolation.

# compd tools

Mix scripts that check the workspace. None of them compiles compd. Run them
from the repo root. `mix tools/<script>.mix --version` prints the version
without running the script.

The desktop-tier gates (nested compd runs, pixel checks, the desktop suite)
live in `tests/desktop/`; see the README there.

## layering_gate.mix

Maps workspace packages to layers from `services/compd/layers.conf.mix`. It
fails on every upward edge (from a lower layer to a higher one) and on every
workspace package missing from the table.

```
mix tools/layering_gate.mix (--manifest-path <Cargo.toml> | --metadata-file <json>)
    [--layers <conf>] [--allow-unlisted] [--include-dev] [--require] [--offline] [--locked] [--frozen]
```

- It uses `cargo metadata --format-version 1 --no-deps`, which reads declared
  deps only and does no resolution.
- It checks normal and build edges. Add `--include-dev` to check dev edges as
  well.
- With `--allow-unlisted`, unlisted crates produce a warning instead of a
  failure.
- Exit codes:
  - 0: clean, or nothing to check (no listed crate in the workspace);
  - 1: an upward edge, an unknown crate, or nothing to check under `--require`;
  - 2: bad arguments, a bad layer table, or `cargo metadata` failed.

The layer table (`services/compd/layers.conf.mix`, strict data) holds:
- `layers` (crate → layer, 0–7);
- `binary`;
- `effects_crates` (may reach Bevy);
- `core_crates` (gate (c));
- `nodefault_crates` (checked in the no-default-features run only).

The rule is that every edge goes to a lower layer or stays within the same
layer, and the graph is acyclic. A crate's layer is a design decision; change
it deliberately.

## bevy_gate.mix

Gate (c). It reads the resolved graph from `cargo metadata` twice, once with
`--all-features` and once with `--no-default-features`. Edges that are dev-only
are ignored.

```
mix tools/bevy_gate.mix --manifest-path <Cargo.toml> [--layers <conf>] [--allow-unlisted]
    [--require] [--offline] [--locked] [--frozen] [--filter-platform <triple>]
mix tools/bevy_gate.mix --metadata-all <json> --metadata-nodefault <json> [...]
```

It fails, printing the path `crate -> … -> bevy|ash`, when:
- a core crate reaches `bevy`/`bevy_*` in either run;
- in the all-features run, a workspace package outside `effects_crates` and the
  binary reaches Bevy (with `--allow-unlisted`, unlisted packages only warn);
- in the no-default-features run, which is the GLES build, a core or
  `nodefault_crates` crate reaches `ash` (no Vulkan in the GLES build).

In the all-features run, a package that reaches `ash` is reported as INFO only.
That is because Bevy's wgpu, unified across the build, turns on wgpu-hal's
Vulkan backend.

Exit codes:
- 0: clean, or nothing to check;
- 1: a violation, or nothing to check under `--require`;
- 2: bad arguments, a bad layer table, or `cargo metadata` failed.

Note: the full `cargo metadata` resolves the graph. That may download crate
manifests into `~/.cargo`, but it compiles nothing. Pass `--locked` so that it
never rewrites `Cargo.lock`.

## smithay_guards.mix

Runs every guard test that `vendor/smithay/PATCHES.md` cites, by building the
vendored smithay's own lib tests standalone with compd's feature set plus
`offline_test`.

```
mix tools/smithay_guards.mix [--reseed] [--json]
```

Each guard is reported PASS, FAIL, UNPARSED (a matching test ran but gave no
verdict) or MISSING (no test matched the guard's name). The vendored crate's
`Cargo.lock` is seeded from compd's own so shared dependencies resolve to the
versions compd builds; `--reseed` re-copies it.

Exit codes: 0 every guard passed; 1 a guard failed, is unparsed or missing;
2 the tests did not build or setup failed.

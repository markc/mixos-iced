# MixOS — layout, naming and conventions

**This file is binding for every addition to this repository**, whether made
by a person or an agent. Read it before creating a directory, a crate, a file
or a name. `CLAUDE.md` only points here.

The rationale, the alternatives weighed and the precedents are in
[`docs/decisions/2026-10-04-source-layout.md`](docs/decisions/2026-10-04-source-layout.md).
Change this file only through a new decision there.

## 0. The ground rules

- **One Cargo workspace, one pinned toolchain** (`rust-toolchain.toml`).
- **MIT OR Apache-2.0** for MixOS's own code. Dependencies must be permissive
  (`deny.toml`, checked by `cargo deny`). Assets are cleared file by file.
- **Mix, never Python.** Every script, gate, generator and helper is Mix
  (`*.mix`). A missing Mix capability is a gap to fix in Mix, not a reason to
  use another language.
- **This repository is public.** It holds runtime behaviour, contracts and
  public decisions. Deployment details, private hosts and plans live in a
  private hub, never here. A hygiene gate refuses private identifiers on
  commit.
- **Agents are the primary operators.** Prefer what is legible (state as
  queryable data), modifiable (structured, gated changes) and reconstructible
  (everything rebuildable from this tree).

## 1. Top-level layout

```
mixos/
├── AGENTS.md  CLAUDE.md  README.md  LICENSE-MIT  LICENSE-APACHE  NOTICE
├── Cargo.toml  Cargo.lock  rust-toolchain.toml  deny.toml  clippy.toml
├── apps/       GUI applications: Wayland clients people use (term, ced, dopus, email…)
├── services/   long-running daemons, named <role>d (noded, maild, webd, compd…)
├── cli/        command-line tools run by people and agents (mix-shell → the `mix` binary)
├── libs/       shared libraries (mix, toolkit, design, config, props…)
├── session/    the desktop session: targets and wiring that compose services + apps
├── tests/      cross-component gates and integration suites (Mix)
├── tools/      developer tooling that never ships: gates, generators, build helpers
├── etc/        system-wide integration not owned by one component (generated where possible)
├── share/      shared runtime data: asset-set manifests, brand art (share/brand), shared scenes, catalogues
├── docs/       the mixos.dev site source: manual, dev guides, specs, public decisions
└── vendor/     patched upstream code (one directory per upstream)
```

**The top level is classified by kind of artefact, one axis only.** Layer
(how low in the stack) and domain (mail, media, mesh…) are data, carried in
metadata and enforced by gates, never by directory. Adding a top-level
directory requires a decision record.

**The kind follows from the name** (§3):

| Kind | Directory | Name form | Examples |
|---|---|---|---|
| app | `apps/<name>/` | short app name | `term`, `ced`, `dopus`, `email` |
| service | `services/<name>/` | `<role>d` | `noded`, `maild`, `compd` |
| cli | `cli/<name>/` | command name | `mix-shell` (builds `mix`) |
| lib | `libs/<name>/` | plain noun | `mix`, `toolkit`, `design` |
| session | `session/` | — | the desktop session |

## 2. Components

Everything under `apps/`, `services/`, `cli/` and `libs/` is a
**component**: one directory, named exactly as its principal package, that
holds everything belonging to that component.

```
services/maild/
├── Cargo.toml     package `maild`, with [package.metadata.mixos] (§2.3)
├── src/  tests/  benches/  fuzz/
├── units/         maild.service, maild.socket, sysusers.conf, tmpfiles.conf
├── scripts/       Mix scripts this component ships
├── assets/        icons, images and data it owns (each cleared, §6)
├── i18n/          Fluent catalogues: i18n/en/maild.ftl
├── README.md      what it is, how to run and test it (the manual is in docs/)
└── AGENTS.md      optional: local rules that add to this file
```

Only create the subdirectories a component needs.

### 2.1 Multi-crate components

A component that needs private crates keeps them in its own `crates/`. They
are never siblings at the kind level.

```
services/compd/
├── Cargo.toml          package `compd` (the binary)
├── crates/<name>/      private crates: kms, render-gles, seat, furniture…
├── layers.conf.mix     this component's internal dependency order (optional)
└── units/  scripts/  tests/
```

Private crates still have globally unique names (Cargo requires it). Their
metadata names the owning component.

### 2.2 Promotion is a decision, not an event

A private crate does **not** move to `libs/` just because something else
imports it. It is promoted to `libs/<name>/` only when a second *external
owner* needs it, after a review of its API, name and contract, recorded in
the commit. An application or private crate becoming a shared library is an
architectural change, and the path move is deliberate. Plan a component's
shared exports when it lands. Don't discover them by accident.

### 2.3 Identity and metadata

Every package declares:

```toml
[package]
publish = false            # required on every package; a gate checks it

[package.metadata.mixos]
component = "maild"        # the owning component (= its directory)
kind = "service"           # app | service | cli | lib | session | tool | test
layer = "core"             # bus | mix | core | desktop
contract = "none"          # none | public  (public = semver'd, see §5)
```

- **Directory = component = principal package name.** The principal binary
  has the same name. The single declared exception is `cli/mix-shell`,
  which builds the `mix` binary, so that the library can be `libs/mix`
  (package `mix`).
- A gate checks that each package's path matches its `component` and
  `kind`, and generates the component index (`docs/dev/components.md`).
  That index is the agent's map of the tree.

### 2.4 Removing a component

Removing a component is a reviewed operation, never just `rm -r`. The
removal-impact report (`tools/`) lists:

- reverse dependencies and session references;
- generated install entries and daemon identities (users, groups, uids);
- on-disk data paths.

Removal never deletes user data. A removed component's names are retired
(§3.5).

## 3. Naming

1. **Plain names only.** Every package, binary and directory has a plain
   name that says what it is: `registry`, `toolkit`, `maild`, `ced`. There is
   **no brand prefix** (`mixos-` or any other brand) and **no layer
   prefix** (`bus-`, `mix-`, `core-`). Don't put a technology in a name when
   the technology may change: it's `toolkit`, not `iced-toolkit`.
2. **Form:**
   - lowercase;
   - a single word where possible;
   - kebab-case when more than one word is clearer (`render-gles`,
     `mix-shell`);
   - the only suffix is `-sys`, for a crate that is nothing but raw FFI
     bindings (the Rust convention).
3. **Unique across the resolved dependency graph,** not only the workspace.
   A name that shadows a crate already in `Cargo.lock` (e.g. `drm`, `winit`)
   or a well-known crate gets a distinctive name instead (e.g. `kms`,
   `nested`). A gate checks this. Very generic names (`client`, `world`,
   `seat`) need justification at review and are best kept inside a
   component.
4. **Binaries:**
   - daemons are `<role>d`;
   - apps use short distinctive names;
   - the shell is `mix`;
   - units are named after their binary (`maild.service`).
   Binary names are checked against the major distributions' file lists
   before first use (`mix` vs Elixir is the one accepted collision; MixOS
   installs to `/opt/mixos/bin`, which comes first in its PATH, and units call
   `/opt/mixos/bin/mix` by absolute path).
5. **Never reuse a name.** Retired package, binary, app and Bus verb names
   are recorded in `docs/spec/names/retired.conf.mix` and stay reserved
   forever. A gate refuses reuse.
6. **The brand lives in one layer only:** Wayland app IDs and desktop files
   (`dev.mixos.<app>`), plus the paths `/opt/mixos`, `/var/lib/mixos`,
   `$MIXOS` and the `mixos` XDG directories.
7. **Frozen wire constants are never renamed.** A byte string inside a
   signature, transcript or cross-process contract keeps its bytes, even if
   it contains an old name. Changing one requires a versioned v2 domain.
8. **Not published to crates.io.** If a crate is ever published, it is
   published under a scripted rename (`mixos-<name>`). The in-tree name stays
   plain.

## 4. Code and file conventions

- **Rust modules:** new code uses `foo.rs` + `foo/`. Don't create a new
  `mod.rs` or a `foo/foo.rs`. This is a ratchet: no new `mod.rs` anywhere,
  checked by `clippy::mod_module_files` in new crates. Existing trees migrate
  a crate at a time when substantially edited, never as a drive-by inside
  other work.
- **Mix files:** `snake_case.mix`. Strict-data config is `*.conf.mix`
  (loaded with `load_data`, never executed). Specs are `*.spec.mix`. Mix tests
  are `*_test.mix`. Code transplanted from elsewhere is renamed on entry, and
  its commit records the old path.
- **Generated files** that are tracked carry a marker line ("generated by …;
  do not edit") and have a `--check` mode that the pre-commit hook runs.
  Generated *install* output (collected units, sysusers fragments, packaged
  assets) goes under `target/mixos/` and is never tracked.
- **Units are owned by their component** (`<component>/units/`). The unit
  collector (`tools/`) gathers them for installation and fails on a
  duplicate or orphan. Never hand-copy a unit into `etc/`. `etc/` holds only
  system-wide integration, generated from component declarations where
  possible.
- **Licence headers:** each MixOS source file carries an SPDX header
  (`SPDX-License-Identifier: MIT OR Apache-2.0`), or is covered by an
  annotation in `REUSE.toml` (REUSE format). Third-party code and assets keep
  their own licence files and a `NOTICE` line, and attribution must survive
  every split, move and merge. These are checked by a Mix gate, not by
  external Python tooling.
- **i18n:** user-visible strings come from Fluent catalogues
  (`<component>/i18n/<lang>/<component>.ftl`), English as the source
  language. Don't hard-code UI strings in new GUI code.
- **Colours and sizes** in GUI code come from design tokens through
  `toolkit::theme`. A gate rejects hard-coded colours.

## 5. Contracts and versions

- **Internal crates** inherit the workspace version (`version.workspace = true`),
  which is the MixOS release version.
- **Public contracts** carry their own semantic version and a changelog, and
  are marked `contract = "public"`:
  - the Mix language;
  - the ABP/Bus wire protocol;
  - each daemon's Bus verbs;
  - `toolkit`'s API;
  - every `*.conf.mix` schema.
  A breaking change needs a deprecation period with both behaviours
  working, and a migration where data is involved.
- **Bus verbs are wire contracts.** Every verb is registered in
  `docs/spec/bus/verbs.conf.mix` (owner, version, status). A gate refuses an
  unregistered, duplicate or retired verb.
- **Specs** live in `docs/spec/<topic>/`. That means prose plus
  machine-checkable `*.spec.mix` fixtures that the `tests/` gates consume.
  Each spec states its status (draft, accepted, superseded). Moving a spec
  never upgrades its status.

## 6. Dependencies, vendor and assets

- **The dependency gates** (in `tools/`, run before every merge):
  - headless crates never depend on smithay, iced, wgpu or Bevy;
  - no second version of wgpu, iced, Bevy or smithay;
  - Bevy only in named effects crates;
  - no non-dev dependency on a `kind = "cli"` package;
  - declared `layer` matches the layer derived from the dependency closure
    (bus ← mix ← core ← desktop);
  - component-internal orders (`layers.conf.mix`) hold.
- **`vendor/<upstream>/`:** a patched upstream, with its licence, a recorded
  upstream base, `PATCHES.md` (every local change and why) and a guard test.
  It is refreshed only deliberately. A security fix may bypass the refresh
  cadence, but never the guard tests.
- **Assets:** each file is cleared individually, with its licence and a
  `NOTICE` line. Fonts may be OFL-1.1; everything else must fit the
  dependency allowlist.

## 7. Tests

| Tier | Where | What |
|---|---|---|
| unit | in the crate (`#[cfg(test)]`) | pure logic |
| integration | `<component>/tests/` | the crate's public behaviour |
| bus | `<component>/tests/*_test.mix` | Bus verbs against a test broker |
| gates | `tests/` | cross-component suites and the dependency, naming and layout gates |
| desktop | `tests/desktop/` | nested and on-hardware compositor and session runs |
| fuzz | `<component>/fuzz/` | parsers and wire formats, including frozen-constant byte-exactness |

## 8. Docs

`docs/` is the source of mixos.dev, built by `docs/build/gen-site.mix`. Pages
are prerendered, and every page is also published as Markdown.

- `docs/<topic>.md`: the user manual.
- `docs/dev/`: contributor and architecture guides, plus the generated
  component index.
- `docs/spec/`: contracts and registries (§5).
- `docs/decisions/YYYY-MM-DD-title.md`: **public decisions**, which are the
  public authority for this tree. Private plans may motivate a change, but a
  rule that binds contributors is stated here.

## 9. Building and bootstrapping

- Requirements: the toolchain in `rust-toolchain.toml`, `git`, and the system
  libraries listed in `docs/dev/building.md`. No Python.
- **Bootstrap:** `cargo build --profile release-fast -p mix-shell` produces
  `mix`. Every gate after that is `mix tools/<gate>.mix`.
- Gates run before merge. A gate that cannot run counts as failed.

## 10. Adding something: the checklist

1. Kind → directory (§1).
2. Plain, unique, unretired name (§3).
3. `Cargo.toml` with `publish = false` and `[package.metadata.mixos]` (§2.3).
4. Its units, scripts, assets, i18n and tests inside the component (§2).
5. Contracts registered (verbs, schemas) if it has any (§5).
6. SPDX headers; third-party attribution intact (§4).
7. Gates pass (§6, §9); the component index regenerated.
8. Manual page in `docs/`; a public decision record if it changes a rule.

---
title: Decision — source layout and naming
description: Why the MixOS repository is laid out by kind of artefact (apps, services, cli, libs) with plain, unique names and self-contained components, and what was rejected.
---

# Decision — source layout and naming

**Accepted 2026-10-04.** The binding rules are in the repository's
[`AGENTS.md`](https://github.com/markc/mixos/blob/main/AGENTS.md). This page records why.

## The decision

- **The top level is classified by kind of artefact, one axis only:**
  `apps/`, `services/`, `cli/`, `libs/` and `session/`, beside `tests/`,
  `tools/`, `etc/`, `share/`, `docs/` and `vendor/`. Layer and domain are
  metadata checked by gates, never directories.
- **Components are self-contained.** A component's crates, systemd units, Mix
  scripts, assets, translations and tests live in its one directory. Private
  crates nest in the component's own `crates/`.
- **Plain names:** no brand or layer prefixes. Names are unique across the
  resolved dependency graph and never reused once retired.
- **Promotion to `libs/` is a reviewed decision, not an automatic move.**
- **The Mix language is two packages:** the embeddable library `mix` in
  `libs/mix`, and the shell `mix-shell` in `cli/mix-shell`, which builds the
  `mix` binary. Daemons embed the interpreter without linking the shell.

## Evidence: how comparable projects do it

Checked against each project's repository on 2026-10-04.

| Project | Shape | Naming | Why |
|---|---|---|---|
| Zed | one workspace, flat `crates/` (249 crates) | plain | internal crates, never published |
| rust-analyzer | flat `crates/` (35) | plain; renamed when published | same |
| Servo, Deno | `components/`, `ext/` | plain | split by role |
| Bevy, uv, nushell | flat `crates/` | prefixed | every crate published to crates.io |
| Haiku | `src/{apps,kits,servers,libs,bin,…}` | plain | by kind of artefact |
| SerenityOS | `Userland/{Applications,Libraries,Services,Utilities,…}` | `Lib*` | by kind of artefact |

Two lessons:

- **Prefixes appear where crates are published** into a global namespace.
  Unpublished workspaces use plain names successfully at very large scale.
- **Operating-system projects divide by kind of artefact,** the most stable
  classifier: an app seldom becomes a library.

## What was weighed

- **Layer + kind + domain at one level** (the earlier plan: `bus/`, `mix/`,
  `core/`, `services/`, `desktop/`). Rejected: three axes compete at one
  level. A desktop daemon could plausibly sit in either of two places, and a
  crate that changes layer has to move.
- **One flat `crates/` directory** (Zed, rust-analyzer), with per-package
  co-location. Seriously considered: it gives fully stable paths, because
  nothing about classification is encoded in a path. Not chosen. With
  hundreds of siblings, a large compositor and a tiny parser look identical;
  the kind of a component (and therefore its lifecycle: units, man pages,
  install rules) would no longer be visible; and the case for stable paths
  is weaker than it looks, because an app becoming a library is exactly
  the kind of change that deserves a deliberate move and review.
- **Domain-first** (`mail/`, `media/`, `mesh/`…). Rejected: domains blur and
  multiply, and cross-domain code has no home.
- **One repository per component**. Rejected: MixOS keeps one
  workspace so one dependency graph can be gated, shared code can be
  consolidated, and changes across components are atomic. Its benefits
  (per-package distribution, separate teams) do not apply here.

## Review

The proposal was reviewed cold by four model families: Codex, GLM, DeepSeek
and, as tie-breaker, Gemini.

- **Kind-first layout:** three of four. The dissent preferred the flat
  directory for path stability. That argument is recorded above.
- **What all reviewers converged on** reshaped the rules:
  - artefacts co-located with their component;
  - `vendor/` shared at the top level;
  - no automatic promotion;
  - Mix out of `cli/` and split into two packages;
  - layers derived from dependencies and cross-checked;
  - removal as a reviewed operation;
  - a Bus verb registry and a name-retirement registry;
  - licence headers in the REUSE format, checked without Python;
  - Fluent for translations;
  - a test-tier taxonomy;
  - public decision records like this one.
- **Considered and declined:**
  - top-level `sys/`, `init/`, `hw/` and `patches/`;
  - a separate test workspace.
  FFI crates take a `-sys` suffix instead, and patch notes live with each
  vendored upstream.

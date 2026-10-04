---
title: Decision — toolkit is generic
description: The iced widget toolkit in libs/toolkit is written for any iced project; nothing MixOS-specific lives in it, and a gate enforces that.
---

# Decision — `toolkit` is generic

**Accepted 2026-10-04; landed 2026-10-05 as `libs/toolkit`.**

## The decision

`libs/toolkit` is an iced widget toolkit written for **any iced project**.
Another project must be able to take it, as a git dependency, by vendoring
or by copying single widgets, with nothing MixOS-specific coming along. The
posture is the one `libs/bus` already has: a generic core, with the project's
specifics kept apart from it.

1. **No MixOS in the crate.** No dependency on any MixOS crate. No MixOS
   name, path, environment variable, app ID, font-family label or wording in
   code, public API, strings or docs.
2. **Inputs are plain data the caller supplies:** a `Palette`/`Metrics`
   pair (`Tokens`) for theming, with built-in dark and light sets; a
   `FontSet` and an `IconFont` (bytes or paths, a `.codepoints` table) for
   fonts and icons; English defaults for its few strings.
3. **MixOS glue is a separate adapter crate** that maps the resolved
   design theme to `Tokens` and the pinned asset set to a
   `FontSet`/`IconFont`. MixOS apps use toolkit through it.
4. **MIT OR Apache-2.0** with SPDX headers, so taking it needs no
   permission. Code absorbed from elsewhere must be permissive and keep its
   attribution.
5. **Plain iced** at the vendored revision; its gallery and examples are
   ordinary iced (winit) programs.

## The gate

`libs/toolkit/tests/generic.rs` runs with every build. It fails if the
crate's files name the project: its current or former names, its install
prefix (`/opt/`), its environment-variable prefix (`MIXOS_`) or its app-ID
namespace (`dev.mixos`), case-insensitively, in any file under
`libs/toolkit/` (the `[package.metadata.mixos]` line that workspace
bookkeeping requires is the one exemption); or if toolkit's
normal-dependency closure contains a workspace crate other than itself or a
path dependency outside `vendor/`.

## Consequences

- The MixOS look is applied by the adapter, never by toolkit defaults.
- A widget that needs MixOS behaviour keeps that behaviour in the
  application or the adapter.
- The crate states its own version, edition and toolchain, and carries its
  licences, so a copy of `libs/toolkit/` is complete.

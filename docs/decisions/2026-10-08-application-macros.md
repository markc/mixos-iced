---
title: Decision — every application supports Mix macros
description: A shared macro facility makes every native application programmable through the ABP Bus.
---

# Decision — every application supports Mix macros

Status: accepted 2026-10-08. Binding for new GUI applications immediately;
existing applications must converge through the rollout. Runtime integration
is not yet complete. This extends the source-layout application requirements
and preserves the generic-toolkit decision.

## Purpose

Native MixOS applications are younger and often have fewer features than
established Linux desktop applications. Their reason to exist includes a
common ability to compose operations across applications, services and nodes.
Users and agents must be able to turn a useful sequence into a reusable action
without adding a separate automation system to each application.

This is the ARexx application-macro principle applied to Mix and the ABP Bus.
It complements ordinary application quality; it does not excuse unreliable
editing, inaccessible controls or poor performance. The distinguishing product
property is a common, supported programming interface across the native desktop,
not a claim that other desktops cannot run scripts.

## Decision

1. Every native GUI application exposes the shared Mix macro facility by
   default. Applications with menu bars have a **Macros** menu. Compact apps
   and scene-backed surfaces expose an equally discoverable **Macros** action
   in their existing menu or action surface. An empty catalogue retains useful
   creation, folder-opening and reload actions.
2. A macro is an ordinary executable `*.mix` script. The application invokes
   it with `/opt/mixos/bin/mix`, supplies application context and reports its
   output and result. No new macro or workflow language is introduced.
3. Application-local macros live in `<AppDirs component>/config/macros/`.
   Relative means relative to the resolved application data root, never the
   process working directory or binary directory.
4. Global, reusable Mix scripts/macros live in **`/opt/mixos/mix`**. This is the
   agreed initial home until real use warrants a documented migration. Generic
   scripts remain usable directly; only explicitly labelled eligible macros
   appear in application menus. There is no recursive scan of home directories.
5. Shared discovery, metadata, context, execution and result handling belong
   to `libs/application`. Applications provide small domain adapters and own
   their existing Bus and domain lifetimes. `libs/toolkit` remains generic.
6. Macros may coordinate any available Mix capability. Application and node
   control uses native ABP Bus verbs between noded instances. The facility adds
   no parallel transport, broker, scheduler or mandatory background daemon.
7. Applications expose useful, documented domain operations through the Bus;
   macro execution is not restricted to the invoking application. Relevant
   mutation fences and undo semantics remain owned by those operations.
8. Existing Ced filenames, `ced-macro`/`ced-key` headers, `CED_*` context and
   edit-origin behaviour remain supported during adoption of the shared contract.
9. New applications ship a working example, native execution evidence and
   macro documentation. Current standalone apps and Quoin scene surfaces are
   rollout obligations, not permanent exemptions. Daemons and CLI tools have
   no GUI menu obligation, but remain callable macro building blocks.

## Contract and delivery

The [application macro specification](../spec/macros/2026-10-08-application-macros.md)
defines version 1 of the target contract, migration and acceptance criteria.
The [macro guide](../macros.md) explains the two homes and current availability.
Implementation planning and operational evidence stay in the private control
hub. Accepting this decision does not claim that the shared runner or new Bus
verbs have already shipped.

Changes to the global home, compatibility rules or context contract require
an explicit versioned migration. Framework gates must enumerate principal
`kind = "app"` components automatically, so future apps cannot evade the rule
by being absent from a manually maintained list.

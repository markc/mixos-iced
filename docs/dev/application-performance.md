---
title: Application performance
description: A practical profiling and review method for tiny-skia apps, shared rendering improvements and later GPU acceleration.
---

# Application performance

Follow the [application rendering decision](../decisions/2026-10-07-application-rendering-and-performance.md).
Develop ordinary 2D apps on tiny-skia for now, keep a usable software path,
and use measurements to decide which work to improve or accelerate.

## Establish the workload

For each new app, describe a small set of repeatable workloads in its README
or component-owned benchmark documentation. Extend these when a slow case
is found. Use synthetic or public fixtures; keep private user documents and
deployment details out of the public repository.

Choose cases that exercise the app's actual work. Examples include:

| App or interaction | Representative cases |
|---|---|
| Terminal | sustained output, scrollback scrolling, typing, cursor blink, resize |
| Editor | ordinary text, very long lines, horizontal and vertical scrolling, selection, edits |
| Lists and trees | many entries, scrolling, filtering, expanding, changing selection |
| Shared UI | window resize, high display scale, theme/font changes, static idle |

Include relevant Unicode, ligatures and colour emoji where the app supports
them. Record document or grid size, viewport dimensions, display scale,
font settings, build profile, revision and renderer. State whether the run
is offscreen, nested with software GL, or presented on native hardware.
Separate cold cache behaviour from steady-state behaviour.

## Find the expensive stage

Use existing tracing and timing facilities before adding instrumentation.
Measure these stages separately where the path allows it:

1. Input, parsing and application model updates.
2. Visible-content selection, text shaping and layout.
3. Primitive preparation, damage calculation and CPU painting.
4. Buffer copies, texture uploads and app presentation.
5. Compositor rendering and display presentation.

Record repeated samples, including typical and tail latency, rather than
one favourable frame. When available, include CPU use, allocations, painted
area, bytes copied/uploaded and cache behaviour to explain the timing.
An asynchronous submit time is not GPU completion or display presentation
time. Do not add timings from overlapping stages as if they were sequential.

Compare the same workload under the same conditions. Label estimates and
unmeasured stages. Offscreen benchmarks help isolate drawing costs; native
hardware checks establish interaction latency, frame pacing and perceived
smoothness. Use the self-contained VT-native session for that validation.

## Refine the software path

Start with the measured cost. Useful techniques include bounding processing
to the viewport, avoiding repeated shaping and layout, reducing allocations
and copies, grouping compatible primitives, and reusing unchanged pixels.
Skipping offscreen drawing is insufficient if the app still processes an
entire long line or document before clipping it; measure that upstream work.

Every cache needs an explicit validity rule. Account for content revision,
font and size, scale, geometry and style changes as appropriate. Damage must
cover pixels exposed by scrolling and both old and new bounds of moved or
removed content. Check selections, carets, overlapping glyphs and resize.

Keep renderer-specific optimisations behind the rendering interface.
Application state and layout must not require a particular framebuffer,
texture atlas or graphics API. Reuse current interfaces where they fit;
introduce new abstractions only for demonstrated needs.

Apply improvements at the shared layer when suitable, then check the apps
affected by that layer. Preserve input responsiveness and event-driven idle
behaviour: a static screen must not acquire continuous redraws or polling.

## Review a performance change

Include the following evidence in the change description or durable
component benchmark notes:

- The user-visible slow case and the stage responsible.
- The repeatable workload, environment and comparable before/after results.
- The change's cache, damage or scheduling assumptions, where relevant.
- Relevant correctness and regression checks, including invalidation cases.
- The affected apps and backends checked, and any unavailable checks.

Use existing component and desktop gates for the changed behaviour. Add a
focused regression check when it catches a real failure; avoid timing
thresholds tied to one machine in portable correctness tests. Stable
performance gates may use controlled workers with recorded baselines.

These are contributor and review requirements. This document does not
claim that a new automated performance gate already enforces them.

## Introduce GPU acceleration

Keep existing optional GPU backends compatible with shared API changes and
run proportionate checks when those paths are affected. A new app does not
need a second renderer immediately.

When painting or pixel movement remains a material bottleneck, prototype
acceleration behind the renderer boundary. Compare equivalent workloads,
including small updates, scrolling, resize and cold caches. Measure both
CPU work and presented latency, including upload and synchronisation costs.

Distinguish uploading CPU-painted pixels from drawing cached glyphs and
background primitives on the GPU. The latter can eliminate substantial CPU
pixel compositing; it still needs shaping, layout and glyph cache management.
CPU scrolling via framebuffer copies need not dictate the GPU strategy.

Before enabling acceleration as the normal path, verify rendering
correctness, input latency, frame pacing, resource use and the supported
software path. Test renderer initialisation failures and fallback behaviour
where implemented; otherwise state the gap before changing the default.
Record the measured benefit and trade-offs in a decision and implementation
review. Retain tiny-skia's improvements rather than abandoning the fallback.

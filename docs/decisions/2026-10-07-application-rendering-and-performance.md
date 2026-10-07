---
title: Decision — application rendering and performance
description: Refine the software rendering path through measurement, keep app state independent of painting, and introduce GPU acceleration against demonstrated bottlenecks.
---

# Decision — application rendering and performance

**Accepted 2026-10-07.** The binding contributor rules are in
[`AGENTS.md`](https://github.com/markc/mixos/blob/main/AGENTS.md#41-application-rendering-and-performance).
The working method is in the [application performance guide](../dev/application-performance.md).

## Decision

Develop and refine ordinary 2D GUI applications using tiny-skia for now.
Find slow paths with representative workloads and tracing, then improve
the algorithms and the software renderer. Preserve a usable software path
when GPU acceleration is added.

This applies to existing and new apps and their shared rendering libraries.
It does not change compd's GLES compositing or its existing internal wgpu
surfaces. Existing optional app GPU paths may be maintained and measured;
this decision does not require removing them or building a GPU backend for
every new app. An app whose core function needs GPU rendering requires an
explicit design decision describing that need and its software behaviour.

Keep application state, visible-content selection, shaping and layout
independent of the mechanism used to paint pixels. Implement suitable
improvements in shared libraries when several apps need them, following
the repository's component and API rules. Backend-specific framebuffer
copies and texture operations stay inside the renderer.

Introduce GPU acceleration when measurements identify rendering or pixel
movement as a material remaining cost and an equivalent prototype improves
the relevant workloads. The date alone is not a reason to switch. A renderer
change must preserve correctness, input responsiveness, idle behaviour and
the software path.

## Why

Visible-content bounds, cached shaping and layout, fewer allocations,
correct damage and responsive scheduling benefit both software and GPU
renderers. Developing those first avoids carrying inefficient application
work into a faster drawing backend.

GPU drawing does not remove terminal parsing, document processing, text
shaping or layout costs. A renderer that uploads a CPU-painted window can
retain most of the CPU drawing cost. GPU glyph compositing, using cached
glyph textures and batched draw records, moves a different part of the work.
Glyphon's [rendering approach](https://github.com/grovesNL/glyphon) illustrates
that distinction; it is an architectural example, not a dependency decision
or a measured speedup for MixOS.

Software rendering also supplies a practical fallback and a path for
automated correctness tests. Headless software tests cannot establish
native desktop performance: real display presentation and driver behaviour
must be measured on hardware, including the VT-native distribution.

## Consequences

- Each app identifies repeatable workloads relevant to its users. Slow
  cases become regression workloads as they are discovered.
- Performance claims include comparable before/after evidence and name
  the stage improved. Offscreen drawing time and presented frame latency
  are reported separately.
- Renderer APIs expose the information each backend needs without making
  application state depend on CPU pixel buffers or GPU resources. Use
  existing abstractions where suitable; do not build a speculative framework.
- Maintain existing GPU paths with proportionate checks when shared APIs
  change. Backend availability and gaps are reported explicitly.
- GPU adoption is a separate measured implementation change, with no
  promised multiplier and no requirement to replace the compositor.

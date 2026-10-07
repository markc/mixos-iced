# vendor/iced: local patches

iced 0.15.0-dev from git master. The code delta is a local port of
`iced_wgpu` to the wgpu 30 snapshot in `vendor/wgpu`, plus the manifest
re-wiring that makes iced build against `vendor/wgpu` and `vendor/cryoglyph`,
and the native window drag bridge documented below. Other differences
are limited to the code and guard files recorded here.

## Upstream base: `3de451447bd28217bb535632867550908e29d5d0`

`iced-rs/iced` master, 2026-08-16 23:26:28 +0200, "Remove `From<u8>`
requirement for `slider` widgets". Workspace version `0.15.0-dev`. That commit
pins cryoglyph at `f4e7e4eb84dc…`, which is exactly the base of
`vendor/cryoglyph` (see its PATCHES.md), so the two bases corroborate each
other.

The import recorded no revision. The base was recovered by measurement:

1. Clone `https://github.com/iced-rs/iced` (bare).
2. Write this directory into that repository as a tree object, without touching
   any branch: with a temporary `GIT_INDEX_FILE`, run
   `git --work-tree=vendor/iced add -A`, then `git write-tree`.
3. For every master commit (first-parent) in the window, sum
   `git diff --numstat <commit> <tree> -- . ':!Cargo.lock'` (the vendored copy
   has no `Cargo.lock`; see below).
4. Take the minimum.

| commit | committed | whole-tree diff (lines / files) |
|---|---|---|
| `2b275718d` | 08-12 | 300 / 11 |
| `fded8807e` | 08-14 | 300 / 11 ("Update `cryoglyph`" to f4e7e4e) |
| `2cffa99b3` | 08-14 | 284 / 10 |
| **`3de451447`** | **08-16** | **255 / 8** |
| `e4f9b4a1d` | 08-27 | 273 / 9 |
| `69ee9c834` | 08-27 | 293 / 12 |

Further out the curve climbs fast (2,176 lines at 08-29, 28,382 at 10-02).

To re-check: `git diff 3de451447 <tree> -- . ':!Cargo.lock'` must list only the
files below (plus compd's guard: `wgpu/src/compd_patch_guard.rs` and the
`mod compd_patch_guard` line in `wgpu/src/lib.rs`).

## Local delta (+152/−103 in 8 files)

| file | +/− | what |
|---|---|---|
| `Cargo.toml` | +2/−2 | **re-wiring**: `cryoglyph = { path = "../cryoglyph", version = "0.1.0" }` (was git rev `f4e7e4e`); `wgpu = { path = "../wgpu/wgpu", version = "30.0.0", … }` (was `"29"`). Both paths resolve inside compd's `vendor/` only while `iced/`, `cryoglyph/` and `wgpu/` stay siblings |
| `wgpu/src/window/compositor.rs` | +27/−13 | wgpu 30 port: `RequestAdapterOptions { apply_limit_buckets: false, … }`; `SurfaceConfiguration { color_space: wgpu::SurfaceColorSpace::Auto, … }` (Auto = wgpu's historical sRGB / ExtendedSrgbLinear choice, so rendering is unchanged); present via `renderer.engine.queue.present(frame)` (wgpu 30 has no `SurfaceTexture::present`), with the view and `on_pre_present()` scoped so they end before the frame moves. Noise: unused `use iced_debug::render;`, a commented-out `use wgpu::hal::DynQueue` and `frame.present()` |
| `wgpu/src/lib.rs` | +5/−1 | wgpu 30 port: headless `RequestAdapterOptions { apply_limit_buckets: false }`; `slice.get_mapped_range()` now returns `Result` → `.expect("Failed to map buffer")`. Noise: unused `use futures::StreamExt;`, two blank lines |
| `wgpu/src/image/atlas.rs` | +1/−1 | wgpu 30 port: `get_mapped_range_mut().unwrap()` |
| `wgpu/src/image/mod.rs` | +2/−2 | wgpu 30 port: `VertexState::buffers` is `&[Option<VertexBufferLayout>]` → `&[Some(…)]` |
| `wgpu/src/quad/solid.rs` | +34/−26 | same `Option<VertexBufferLayout>` port, done by hoisting the attributes into a `const ATTRIBUTES` and a `Vec<Option<_>>` (the attribute lists are unchanged; the indentation is odd) |
| `wgpu/src/quad/gradient.rs` | +35/−28 | same |
| `wgpu/src/triangle.rs` | +46/−30 | same, for the solid and gradient mesh pipelines |

No marker comments. There is no behaviour change intended beyond following the
wgpu 30 API; the two values the port chose (`apply_limit_buckets: false`,
`color_space: Auto`) equal wgpu 30's defaults and historical behaviour.

Outside the code: the vendored copy has **no `Cargo.lock`** (upstream has
one). Harmless, since compd builds iced as path
dependencies under its own lock.

## Guards

`wgpu/src/compd_patch_guard.rs` (`#[cfg(test)]`, wired from `wgpu/src/lib.rs`,
marked `// compd`). The pure API port is not guarded: if it
is lost, `iced_wgpu` fails to compile against `vendor/wgpu`. The guards cover
what the compiler cannot:

| test | fails when |
|---|---|
| `compd_workspace_manifest_rewires_cryoglyph_and_wgpu` | a re-vendor restores the git cryoglyph or crates.io wgpu in `Cargo.toml` (a second wgpu in the graph) |
| `compd_rewired_paths_resolve_to_one_wgpu` | `vendor/cryoglyph` or `vendor/wgpu/wgpu` stops sitting beside `vendor/iced`, or iced and cryoglyph resolve different wgpu dirs, or cryoglyph's own manifest loses its wgpu path |
| `compd_surface_config_pins_auto_color_space` | the `color_space: wgpu::SurfaceColorSpace::Auto` choice is dropped or changed |
| `compd_adapter_requests_disable_limit_buckets` | `apply_limit_buckets: false` is dropped from the window or headless adapter request |
| `compd_present_goes_through_queue_after_pre_present_hook` | presenting stops going through `Queue::present(frame)`, or `on_pre_present()` moves after it |

Run:

```
cargo test --manifest-path vendor/iced/wgpu/Cargo.toml compd_
```

The needle set was proven both ways on 2026-10-03 without cargo: all seven
needles (these five tests' strings plus cryoglyph's) match the vendored tree,
and all seven fail on a pristine extract of upstream `3de451447` + cryoglyph
`f4e7e4e`.

Retire the API-port part of this delta (and the corresponding guards) when a
re-vendor lands an upstream iced that already targets wgpu ≥ 30; keep the
re-wiring guards for as long as the forks are vendored.

## Toolkit input accessors retired

The former public text-input state and cursor accessors were removed after
Toolkit adopted its own MIT-attributed input adapter over iced public editor
traits. Its isolated pristine-upstream gate passed on 2026-10-06, including
unit tests, rendered interaction tests and doctests.

## Native window drag bridge

`core/src/window/drag.rs` carries renderer-independent MIME/action/event
types. `runtime::window::drag_drop` queues an operation and returns an
explicit Unsupported/Invalid result if it cannot be queued. The winit
adapter converts events and dispatches requests to the window's existing
Wayland data device. Logical offer coordinates account for both output
scale and the application's own zoom. No raw-handle downcasts or second
connection are used. The toolkit library uses its own neutral session
types; tools/native-gallery is the concrete bridge and acceptance host.

The integration example passes native output scale as f64 to conversion.
Its two surface configurations use `SurfaceColorSpace::Auto` and presentation
uses `Queue::present`, matching the wgpu-30 renderer port. Guard: check the
integration manifest with the pinned native forks and `iced_winit/wayland`.

## Native CPU grid renderer

The CPU renderer also retains identical rectangular clip masks within each
draw and clears only the previous rectangle when the clip changes. Mask
coverage still comes from tiny-skia, including fractional edges. Engine
entry points restore their own clip before consuming the shared mask.
Empty glyph rasters are cached alongside visible glyphs, so spaces do not
repeatedly invoke Swash. Masked text skips glyph pixmaps whose actual ink
bounds miss the physical clip; unmasked overhang retains upstream behaviour.
The native-grid/presentation guard tests use iced's current structured Scale.
Guards: `clip::tests` (fractional/offscreen mask
equivalence and release benchmark) and `text::tests` (real shaped space,
visible glyph, cache eviction, glyph-ink pixel oracle and narrow-damage benchmark).

Ported from the frozen source e0297242305f3a3c3de09f1ca01e8faa771768da.
The existing tiny-skia crate gains immutable native grid generations and cell
revision damage, preserving draw order in the image sublayer. Opaque images
use exact translated pixel copies when scaling and placement permit it;
translucent, rotated and fractional cases retain the upstream raster path.
The iced 0.15 text, settings, local clip and shadow handling stay intact.
No second iced version or raster engine is added.

Guards are the retained grid and raster unit tests, including colour, clip,
fractional scale, damage lineage, overlay order and forced-fallback equivalence.
Term adds end-to-end sparse-damage and offscreen pixel comparisons.
The optional raster-probe counts copies; reference-raster disables the fast
path for comparative tests. Neither feature is enabled in production.

## Primary selection and CPU presentation

Term and Ced need primary selection independently of the regular clipboard.
Core clipboard kinds/content now represent primary text; the runtime queues
the same native action, and the existing arboard 3.6 Wayland backend selects
LinuxClipboardKind::Primary through its extension traits. Other platforms
return the existing unsupported result; no D-Bus integration is added.
Native acceptance must verify primary and clipboard stay distinct.

The CPU compositor retains the source PresentHistory implementation, including
buffer-age repair, front-buffer damage, outward-rounded physical damage and
pre-present callbacks even on empty frames. Failed presents do not enter
history. Its unit tests and Term offscreen pixel tests exercise the actual
production implementation, rather than a duplicate model. The iced 0.15
backend and renderer settings APIs remain intact.

## Idle transition redraw retries

`widget/src/transition.rs`: a repeated `RedrawRequested` at the same instant
still synchronises a changed target or reset. It requests another frame
while animating or finishing, then sleeps once idle.
Iced's host retries a frame after message reduction or layout invalidation;
the previous unconditional request could make an idle transition beside a
guarded redraw callback keep producing frames forever.

The actual `Keys` + idle `Transition` regression in compd's UI host guards
this change (`idle_transition_with_redraw_callback_sleeps_or_starts_a_new_target`).
Completion chaining is guarded at the same timestamp, with exactly one
completion notification and a wake for the next target.
Resetting an idle animation rebuilds its child even if no new animation
starts; the real reset operation and drawn style are regression-tested.
The Transition/Responsive relayout test also asserts that an active
transition continues requesting frames. Run `cargo test -p ui --lib`.
Retire this patch when upstream handles idle same-instant retries.

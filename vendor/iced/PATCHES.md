# vendor/iced: local patches

iced 0.15.0-dev from git master. The only code delta is a local port of
`iced_wgpu` to the wgpu 30 snapshot in `vendor/wgpu`, plus the manifest
re-wiring that makes iced build against `vendor/wgpu` and `vendor/cryoglyph`.
Nothing outside `iced_wgpu` and the root `Cargo.toml` differs from upstream.

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

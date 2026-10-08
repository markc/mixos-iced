# vendor/iced: local patches

## Failed pre-commit presentation and capacity recovery

The shared native presentation helper tracks whether the actual pre-present hook
ran. Recoverable Lost/Outdated and ordinary Other failures request Winit's one-shot
pacing retry only for that failed window; pre-hook failures use ordinary redraw,
and Occluded/OutOfMemory retain their visibility/fatal policies. The original
native callback and unsuccessful feedback tombstones remain owned until actual
retirement. No resize, extra commit, observer message or timer supplies recovery.

The actual commit ledger marks local/native capacity refusal independently of
already-proven or successfully pending bindings. At AboutToWait, before the idle
shortcut, one process release epoch reconciles live blocked windows against their
own native availability. A queued retry consumes its blocked flag; losing native
admission rearms it. An untracked successful commit is allowed to drain failed
requests, whose eventual native Presented still cannot prove success. Actual
Charge retirement wakes this path even when the original window's event is
suppressed. Epoch exhaustion reconciles each existing native wake without wrapping.
Ledger guards use this production module, including the root embedding.

The non-default core fault scope also supports BeforeCommit. Tiny-skia consumes
it after the actual pre-present callback and before buffer submission; AfterCommit
retains its original meaning. The recovery fixture presents its baseline and
consumes startup completion before changing to one fixed draw. It records one or
nine real BeforeCommit faults, requiring failed native-ID retirement and a fresh
successful proof. Failure nine still arms from the drawn binding when feedback
capacity suppresses its candidate; the later successful untracked commit must
drain the backlog. An additional unheld AfterCommit schedule accepts either real
terminal outcome and independently requires successful recovery. No fixture
redraw, resize or raw commit assists these recovery schedules. Physical draw size
and observer owner stay fixed. Metadata/observations are bounded and the runner
supplies finite execution/cleanup deadlines.

The separate process-capacity probe creates sixteen donors sequentially. Each
requests eight real feedback objects at its first successful buffer submission;
the fixture retains every actual terminal lease before closing that donor and
waiting for its actual native Destroyed event. Native IDs are matched only to
active incarnations and terminal request IDs remain distinct. One later immutable
target must receive actual Capacity while all 128 foreign leases remain held.
Only an actual blocked/unavailable AboutToWait scan signals a finite release
thread. That thread drops the whole native leases without sending an app message,
reply or user event. Read-only hooks require a changed release epoch and the
actual production capacity-retry branch before the exact target Presented
receipt. The target receives no Opened completion message, redraw, resize or view
change during recovery. Ordinary builds omit these bounded fixture holders and
notifications. Cross-loop undrained-queue wake acceptance is a separate backend
guard; the process probe does not claim that combined lifetime schedule.

## Non-default native frame ordering acceptance

`native-frame-probe` forwards through core, graphics, runtime, tiny-skia,
renderer, winit and the umbrella crate. Ordinary builds omit the scope, runtime
gate and holder. The separate native-gallery probe enables it explicitly.

The core thread-local scope is bound to one synchronous draw and cannot move
between threads. Its default AfterCommit point is consumed only after the actual
buffer commit succeeds. The strict ordering schedule then holds that window's
submissions until the failed ID's real terminal passes normal ledger delivery and
native lease retirement. Production error recovery still runs unchanged; the
probe releases one ordinary redraw after delivery. This preserves a real
Presented-on-failed-ID suppression proof even when faster production retries
could legitimately supersede that commit and cause Discarded. The separate
unheld recovery schedules establish automatic production retry behaviour.
Failed submissions cannot reach the production observer as Presented.

Each window gate can retain at most one actual successful old feedback lease.
It releases that lease only after recording a successful replacement submission,
using the same conversion/ledger-resolution/drop-before-observer delivery helper
as ordinary feedback. Shared fixture control retains bounded copied metadata and
two single-use notifications; no global native leases, observer-driven redraw,
extra application transport or second runtime is introduced. Window retirement
drops the holder and cancels unfinished waits. A separate close-held schedule
checks that retirement instead of fabricating successful replacement evidence.
The held notification waits for both the actual old successful lease and the
failed request's actual terminal, so out-of-order native dispatch cannot close
the window before the failed-ID terminal has been inspected.

The ordering schedule does not establish automatic pre-commit recovery after
winit has requested its pacing callback. The unheld recovery fixture above is
the separate owning acceptance path for that behaviour.

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
`mod compd_patch_guard` line in `wgpu/src/lib.rs`), plus `core/src/font.rs`
and `graphics/src/text.rs` from the "Numeric weights and the pinned
registration seam" section further down.

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

## Numeric weights and the pinned registration seam

`graphics/src/text.rs::Version::value()` exposes a read-only numeric revision
for bounded registry accounting and exact registration-delta guards. The
counter remains private; callers cannot construct or advance a Version.

`core/src/font.rs` adds `Weight::Numeric(u16)` and
`const fn value(self) -> u16`, so exact CSS weights are representable without
bucketing. The four exhaustive `Weight` matches in this tree now convert
through `value()`: `graphics/src/text.rs` (`to_weight`),
`libs/toolkit/src/fonts.rs` (`registered_font`),
`services/compd/crates/scene-host/src/appearance.rs` and
`apps/dopus/src/app.rs`. No other exhaustive match on `Weight` exists in the
tree (audited 2026-10-08; `apps/dopus/src/icons.rs` only constructs weights).

`graphics/src/text.rs::FontSystem::register_fonts` forwards the
cosmic-text registration transaction (see `vendor/cosmic-text/PATCHES.md`)
and bumps `Version` exactly once when faces or policies were actually added;
identical transactions do not bump, and the hypothetical version overflow is
checked before the cosmic commit. `load_font` now refreshes the derived
database indexes after a successful mutation instead of relying on the match
cache clear alone.

The fontdb 0.23 loader returns all inserted IDs (including collections), so
`load_font` tests that this result is non-empty before refreshing indexes or
advancing the version. Malformed input leaves the version unchanged.

The guard tests live in the root-owned toolkit integration target
(`libs/toolkit/tests/font_registration.rs`, feature
`font-registration-guards`), which path-includes the cosmic seam sources
(see `vendor/cosmic-text/PATCHES.md`) and ports the wrapper scenarios below
through the public font system, registration, version and paragraph APIs.
The exact-delta assertions use the read-only `Version::value()` accessor.
The guard feature also forwards the cosmic `monospace_fallback` feature
through a test-only toolkit alias, so the path-included per-script
monospace index guards run their full assertions. The private `cfg(test)` duplicates previously carried in this file and in
`core/src/font.rs` were removed so the patch documentation names one
executable owner; no upstream unit test was touched. The ported scenarios
use the packaged Noto Sans fixture for their text (an icon-only face does
not establish Latin paragraph coverage) and assert non-empty raster ink,
not just a returned image.

Run from the repository root at the checked/updated lock SHA:

```
cargo test --locked --profile release-fast -p toolkit --features font-registration-guards,tiny-skia --test font_registration
```

| test | fails when |
|---|---|
| `named_and_numeric_weights_keep_their_exact_values` | a named weight loses its 100..=900 value, or a numeric weight loses its exact value (1, 350, 650, 1000) |
| `registration_changes_version_and_noops_stay_stable` | a successful registration does not advance the version by exactly one, an identical or empty transaction advances it, or a failed registration changes the version or the live database/policy facts |
| `one_transaction_with_many_faces_activates_once` | one transaction with several faces and policies advances the version by more than one, or the no-op that follows it advances it again |
| `retained_paragraph_stays_pinned_across_registration_version` | a retained paragraph does not report a Shape difference through the comparison path after a version bump, its re-shape loses the original face/metrics, or it stops rasterising non-empty ink from the pinned face |


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

Solid rectangular quads with no border, radius or shadow restrict their
transformed geometry to the outward-rounded integer damage rectangle. The
original clip mask and tiny-skia antialiasing still determine pixel coverage;
transformed fractional quad edges are retained exactly. This bounds masked
colour-pipeline work during narrow caret redraws. Gradients and decorated
quads keep the upstream path. Original transformed coordinates outside the
conservative ±8191 supersampling envelope also retain upstream behaviour,
including its rejection of extreme finite geometry. Guards: `engine::quad_tests` (full-path pixel
oracle across fractional scales, clips, negative positions and alpha; actual
path-area bound; decorated fallback; ignored release comparison benchmark).

Ported from the frozen source e0297242305f3a3c3de09f1ca01e8faa771768da.
The existing tiny-skia crate gains immutable native grid generations and cell
revision damage, preserving draw order in the image sublayer. Opaque images
use exact translated pixel copies when scaling and placement permit it;
translucent, rotated and fractional cases retain the upstream raster path.
The iced 0.15 text, settings, local clip and shadow handling stay intact.
No second iced version or raster engine is added.

`tiny_skia/src/window/cpu_profile.rs`: `ICED_CPU_PROFILE=1` enables one
aggregate per active second at log target `iced_tiny_skia::cpu_profile`.
It reports buffer ages, physical surface size, submitted damage rectangle
counts/area, full/empty frames and acquisition, damage, raster and present
timings. Rectangle areas are summed, including repeated overlapping work.
Disabled frames perform no clock reads or logging. No document or input
content is collected. Use `RUST_LOG=iced_tiny_skia::cpu_profile=info` when
the application's default filter hides dependency logs.

Guards are the retained grid and raster unit tests, including colour, clip,
fractional scale, damage lineage, overlay order and forced-fallback equivalence.
Term adds end-to-end sparse-damage and offscreen pixel comparisons.
The optional raster-probe counts copies; reference-raster disables the fast
path for comparative tests. Neither feature is enabled in production.

## Native layout inspection hooks

Read-only layout queries, enabled by the existing `selector` features and
not by any default:

- `core/src/widget/operation.rs`: a default-no-op `Operation::clip(Rectangle)`
  hook that clipped containers call within a traversal scope. It forwards
  through the `Box`, `black_box`, `map`, `map-ref` and `then` adapters. Guard:
  `clip_hook_forwards_through_every_adapter`.
- `core/src/window/id.rs`: `Id::from_raw(u64)` / `Id::raw()` for diagnostic
  queries that select an explicit window.
- `selector/src/find.rs`: the `Finder` gains an initial viewport
  (`with_viewport`), bounded traversal limits (`with_limits`), a visited
  count, a `truncated` flag, and a `clip` intersection scoped by its
  `traverse` save/restore so a clip cannot leak into the next subtree.
- `selector/src/query.rs`: a bounded raw-record traversal (`query`) that
  records alias index, candidate kind, layout bounds and clipped visible
  bounds only — never text, editor state, unique id debug strings or
  reconstructed rectangles. Guards: `clip_intersects_the_viewport_and_never_leaks_siblings`,
  `nested_clips_intersect_and_restore`, `limits_truncate_the_traversal`.
- `runtime/src/lib.rs` (feature `selector`): an `Action::Query` carrying a
  `QueryTarget`, a `Layer`, a bounded query operation and a oneshot reply.
- `runtime/src/widget/selector.rs`: the public `query` helper, which accepts
  a `Selector<Output = u8>` (alias indices), never a mutating `Operation`.
- `runtime/src/user_interface.rs`: `UserInterface::inspect` walks the real
  base layout or the already-laid-out cached overlay (`false` = `NOT_READY`);
  it never calls `overlay.layout` merely to answer. `UserInterface` records a
  `layout_sequence` whenever a real layout is completed or replaced — layout
  evidence only, never a presentation revision counter.
- `winit/src/lib.rs` (feature `selector`): the `Action::Query` branch routes
  to the chosen live interface without the `Action::Widget` redraw branch; no
  update, message or redraw is produced by a query. `window::Manager::len`
  lets `QueryTarget::Only` fail on more than one window instead of silently
  picking the first.
- `test/src/emulator.rs` (feature `selector`): the equivalent handling for
  the headless test runtime.

Run the guards:

```
cargo test --manifest-path vendor/iced/selector/Cargo.toml
cargo test --manifest-path vendor/iced/core/Cargo.toml clip_hook_forwards
```

The toolkit `VirtualList` advertises its exact row drawing clip through the
new hook (its own guard tests in `libs/toolkit`). Retire this delta when a
re-vendor lands an upstream iced that already carries a read-only query.

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
# Rendered-view presentation binding

The ledger also retains one latest drawn binding, including request refusal or
deduplicated proof, so unsupported-only windows notify their owner on retirement.
Replacing or removing a host's observer requires explicitly closing that host;
the ledger is bounded and cannot retain an archive of forgotten observers.

`Program::frame_presentation` supplies an owned immutable stamp and metadata
observer beside construction of each view. All standard decorators, application
and daemon builders, devtools and the tester preserve that actual state/window
binding. UserInterface retains it during relayout, never in a reusable Cache.

The Winit runtime requests native feedback synchronously after painting just
before its renderer commits the real surface. A bounded per-window ledger
records success or abort of each requested submission. Only terminal feedback
for a successful matching submission reaches the captured observer; an aborted
request cannot attest a later buffer. Deduplication includes observer identity.
Metadata observation goes directly to the sink after native event retirement,
without an app message, UI event, redraw or view rebuild. Ordinary rendering
continues on unsupported backends with explicit evidence unavailability.

Ledger guards cover late/superseded feedback, same-stamp replacement observers,
aborted submissions and duplicate feedback. Actual renderer/native and complete
application adaptation gates remain required; these unit guards are not native
presentation acceptance.
### Per-request lifecycle provenance for retained native views

`core/src/window/presentation.rs` adds observation-only capture factories to
`FrameObserver` and `FrameBinding`. `winit/src/presentation.rs` captures the
terminal observer when admitting a native feedback request. A retained view
keeps its immutable rendered stamp while each new request acquires the current
owner generation. Pending requests retain their old terminal sink. This retires
late old-generation callbacks without disabling future frames of the same
window, and does not request a redraw or create presentation evidence.

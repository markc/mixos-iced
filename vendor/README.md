# vendor/

Forks and patched crates the compd workspace builds against. Each directory is
kept intact as imported: no files are trimmed, and their own `[workspace]` roots
are kept, which is why the root `Cargo.toml` lists `vendor` under
`[workspace] exclude`. Workspace crates reach them through
`[workspace.dependencies]` path entries or `[patch.crates-io]`.

Each crate's local edits against its upstream base are listed in that crate's
`PATCHES.md`, with the method used to recover the base revision and the guard
tests that fail if a re-vendor drops an edit.

| Crate | Dir | Upstream version | Wired via | Local edits | Notes |
|---|---|---|---|---|---|
| smithay | `smithay/` | 0.7.0, git master `7ddcd1736b47` (2026-08-17), recovered by diff (trough 1774 lines / 28 files; see `smithay/PATCHES.md`) | `[workspace.dependencies]` path | **yes**: +1,574/−200 in 28 `src` files (53 `local` marker lines; unmarked rows flagged in PATCHES.md) | compd's hooks, the pre-request hook and the local patches are on top; all listed in `smithay/PATCHES.md` with guards |
| wgpu (+ wgpu-core/-hal/-types, naga) | `wgpu/` | 30.0.0 = tag `v30.0.0`, commit `8bf3e5ff4ab4` (2026-07-01), recovered by diff (trough 79 lines; see `wgpu/PATCHES.md`) | `[workspace.dependencies]` path (`wgpu/wgpu`) | **yes**: one function, `texture_from_dmabuf_fd_planar` (+76, Vulkan HAL; the single-plane `texture_from_dmabuf_fd` is upstream), plus 3 blank lines of noise; `Cargo.lock` not vendored. Guard: `wgpu-hal/tests/compd_patch_guard.rs` | GL-only feature set in the root manifest (no Vulkan, no ash), so the delta is not compiled in compd; its only caller was a Bevy import path compd does not carry |
| iced (core, graphics, runtime, wgpu, widget, …) | `iced/` | 0.15.0-dev, git master `3de451447bd2` (2026-08-16), recovered by diff; see `iced/PATCHES.md` | `[workspace.dependencies]` paths (`iced_core`, `iced_graphics`, `iced_runtime`, `iced_wgpu`, `iced_widget`) | **yes, port only**: `iced_wgpu` ported to wgpu 30 (+152/−103 in 8 files: `Option<VertexBufferLayout>`, `Result` mapped ranges, `apply_limit_buckets: false`, `color_space: Auto`, `Queue::present`) + manifest rewiring (`cryoglyph` → `../cryoglyph`, `wgpu` → `../wgpu/wgpu`) | `iced_wgpu` without default features, `web-colors` on; guards `iced_wgpu::compd_patch_guard` |
| cryoglyph | `cryoglyph/` | iced-rs glyphon fork 0.1.0, git master `f4e7e4eb84dc` (2026-08-07, the rev iced `3de451447` pins), recovered by diff; see `cryoglyph/PATCHES.md` | iced's workspace (`../cryoglyph`) | **yes, port only**: wgpu-30 path rewiring + a 2-site API port (+9/−4). The `last_used` eviction and `MipmapFilterMode`/`immediate_size`/`multiview_mask` are **upstream** | must stay beside `iced/` and `wgpu/` (relative paths); guard `compd_patch_guard::compd_manifest_takes_wgpu_from_vendor` |
| input (input-rs) | `input/` | crates.io 0.10.0 (SHA-256 `f9793345…`, upstream `691c8502…` = master HEAD 2026-10-03), proved by archive diff | `[patch.crates-io]` | **yes**: +15 lines, tablet-pad DIAL routed in `event.rs` and parsed in `event/tablet_pad.rs` (both `libinput_1_26`, enabled by vendor/smithay). See `input/PATCHES.md` | guard `event::tablet_pad::compd_dial_patch_guard` (2 source tests); drop when upstream routes DIAL |
| calloop | `calloop/` | crates.io 0.14.4 (SHA-256 `4dbf9978…`, upstream `7e4d3ac5…`), proved by archive diff | `[patch.crates-io]` | **yes**: delta exactly `src/sources/channel.rs` +57/−2 (+ one CHANGELOG whitespace line). See `calloop/PATCHES.md` | see below |
| font (Inter variable) | `font/` | Inter 4.001 (`git-66647c0bb`), byte-identical to the Google Fonts download v20 (SHA-256 `0be2399e…`, `fonts.gstatic.com/s/inter/v20/UcCo3FwrK3iLTfvlaQc78lA2.ttf`); not an rsms release file | `include_bytes!` in `ui::font::default`: the no-set fallback (the asset set's `sans` role wins when one is installed) and scene-host's hermetic test renderer | none (unmodified) | SIL OFL 1.1, text in `font/OFL.txt` (rsms/inter `LICENSE.txt`) |

## calloop 0.14.4: composed-channel idle wakeups

The only runtime change against the crates.io 0.14.4 archive is in
`src/sources/channel.rs`: a bounded channel batch re-pings only when its own
ping callback actually ran. Composite event sources forward a readiness token
to every child, and a child whose token did not match used to treat that as an
exhausted batch and ping itself. Two child channels could then wake each other
forever, which is a busy loop in an event-driven compositor.

The manifest is byte-identical to crates.io's, so dependency resolution is
unchanged. compd's lock resolves only calloop 0.14.4, which this patch
replaces. There is no 0.13 in compd's graph.

Regression test (in-tree, carried with the source):
`sources::channel::tests::composed_channels_do_not_ping_each_other_for_unrelated_tokens`.
One queued message must produce exactly one readiness dispatch across eight
nonblocking loop rounds; before the fix it produced eight. Run it with:

```
cargo test --manifest-path vendor/calloop/Cargo.toml composed_channels
```

Check the routing with `cargo tree -i calloop@0.14.4`: the source must be
`vendor/calloop`. Drop the patch once upstream's channel has equivalent
wrong-token behaviour.

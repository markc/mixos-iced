# vendor/cryoglyph: local patches

iced-rs's glyphon fork, from git master (not crates.io: the published 0.1.0
predates the wgpu 29 port and the generation-based atlas tracker). The local
delta is a small port to the wgpu 30 snapshot in `vendor/wgpu` and the manifest
re-wiring onto it.

## Upstream base: `f4e7e4eb84dc1d2f335a98c67d8694640d53a433`

`iced-rs/cryoglyph` master, committed 2026-08-07 15:07:43 +0200 (authored
07-09), "Switch to generation-based usage tracker". Crate version `0.1.0`.
This is the rev pinned by upstream iced `3de451447`, the measured base of
`vendor/iced`, so the two bases agree.

The import recorded no revision. The base was recovered by the same method as
smithay/iced: write this directory into a
bare clone as a tree object (temporary `GIT_INDEX_FILE`,
`git --work-tree=vendor/cryoglyph add -A`, `git write-tree`), then sum
`git diff --numstat <commit> <tree> -- src Cargo.toml` for every commit on any
branch since 2025-10 and take the minimum.

| commit | date | `src`+`Cargo.toml` diff (lines / files) |
|---|---|---|
| `1d68895` | 02-19 | 66 / 5 |
| `e429a02` | 04-23 | 58 / 5 |
| `53ba3e8` | 04-28 | 52 / 5 ("Update `wgpu` to `29`") |
| **`f4e7e4e`** | **08-07** | **13 / 3** |
| `e6aec58` | 09-12 | 15 / 3 (later; cosmic-text fork) |

The whole tree (examples, benches, licences) differs in the same 3 files only.

**Verified upstream, not local:**

- `MipmapFilterMode`, `immediate_size`, `multiview_mask`: **upstream**, already
  in `53ba3e8` (the wgpu 29 port). Not local.
- `last_used` generation eviction (`lib.rs` `GlyphDetails::last_used`,
  `text_atlas.rs` `generation`, `text_render.rs`): **upstream** `f4e7e4e`. Not
  local, so it needs no preservation guard.
- `Option<&BindGroupLayout>` (`bind_group_layouts: &[Some(&atlas_layout),
  Some(&uniforms_layout)]`): **upstream** `53ba3e8`. The `Option` change that
  *is* local is `VertexState::buffers: &[Option<VertexBufferLayout>]`.

To re-check: `git diff f4e7e4e <tree>` must list only the files below (plus
compd's guard: `src/compd_patch_guard.rs` and its `mod` line in `src/lib.rs`).

## Local delta (+9/−4 in 3 files)

| file | +/− | what |
|---|---|---|
| `Cargo.toml` | +1/−1 | **re-wiring**: `wgpu = { path = "../wgpu/wgpu", version = "30.0.0", … }` (was `"29"` from crates.io). Resolves only while `cryoglyph/` and `wgpu/` are siblings in compd's `vendor/` |
| `src/cache.rs` | +7/−2 | wgpu 30 port: `VertexState::buffers` takes `&[Option<VertexBufferLayout>]`, so the cached layouts are cloned into a `Vec<Option<_>>` per pipeline build |
| `src/text_render.rs` | +1/−1 | wgpu 30 port: `get_mapped_range_mut()` returns `Result` → `.unwrap()` |

No marker comments. The examples and benches are not ported (`examples/` still
use the wgpu 29 API) and are not built by compd.

## Guards

`src/compd_patch_guard.rs` (`#[cfg(test)]`, wired from `src/lib.rs`, marked
`// compd`). The API port needs no guard: losing it is a
compile error against `vendor/wgpu`.

| test | fails when |
|---|---|
| `compd_manifest_takes_wgpu_from_vendor` | a re-vendor restores `wgpu = "29"` (crates.io), or `vendor/wgpu/wgpu` is no longer beside `vendor/cryoglyph` |

`vendor/iced`'s `compd_rewired_paths_resolve_to_one_wgpu` additionally checks
that iced and cryoglyph resolve the same wgpu directory.

Run:

```
cargo test --manifest-path vendor/cryoglyph/Cargo.toml --lib compd_
```

The needle was proven to match the vendored manifest and to fail on upstream
`f4e7e4e` (2026-10-03, without cargo).

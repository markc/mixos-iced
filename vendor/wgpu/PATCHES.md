# vendor/wgpu: local patches

The gfx-rs/wgpu workspace (wgpu, wgpu-core, wgpu-hal, wgpu-types, naga, …) at
the **v30.0.0 release**, plus one local function.

## Upstream base: `8bf3e5ff4ab45e2c150e0d6c70d01d25f5b126c1` = tag `v30.0.0`

`gfx-rs/wgpu` trunk, 2026-07-01 17:07:34 −0400, "Update to v30 (#9790)". Tag
`v30.0.0` (tag object `10239959…`) peels to this commit. All crate versions are
`30.0.0`. The vendored tree is the release itself, not a later snapshot: its
CHANGELOG ends `## Unreleased` (empty) followed by `## v30.0.0 (2026-07-01)`.

The import recorded no revision (no `.cargo_vcs_info.json`). The base was
recovered by measurement, as for smithay:

1. Bare-clone `https://github.com/gfx-rs/wgpu`.
2. Write this directory into it as a tree object without touching any branch:
   with a temporary `GIT_INDEX_FILE`, `git --work-tree=vendor/wgpu add -A`, then
   `git write-tree` (tree `8b91bbd75e0e…`, taken before the guard test below was
   added).
3. For every first-parent trunk commit from 2026-06-01 to 2026-08-25 (423
   commits), sum `git diff --numstat <commit> <tree> -- wgpu/src wgpu-core/src
   wgpu-hal/src wgpu-types/src naga/src`.
4. Pick the minimum.

| commit | date | src diff (lines / files) |
|---|---|---|
| `69e66d8e2a8a` | 07-01 | 224 / 7 |
| `2ea3a1d41b00` | 07-01 | 221 / 6 |
| `aa2b7907c020` | 07-01 | 91 / 3 |
| **`8bf3e5ff4ab4`** | **07-01** | **79 / 2 (v30.0.0)** |
| `d0264fcb6d8f` | 07-02 | 79 / 2 |
| `8416c058ef32` | 07-02 | 85 / 3 |
| `b29c1792b209` | 07-02 | 87 / 4 |
| `7418e2d1c1c2` | 07-02 | 276 / 12 |

The tie with `d0264fcb` ("update DXC and WARP to latest") is broken on the
whole tree: against `8bf3e5ff` the tree differs in 3 files, against `d0264fcb`
in 5 (its two extra files are the DXC/WARP bump, outside the crate sources).
That the minimum sits on the release tag settles it.

Whole-tree residual against `v30.0.0`:

```
git diff --stat 8bf3e5ff4ab4 <tree>
 Cargo.lock                    | 5522 -----   (absent from the vendor copy)
 wgpu-hal/src/vulkan/device.rs |   78 +
 wgpu-hal/src/vulkan/mod.rs    |    1 +
```

No `Cargo.toml` anywhere in the workspace differs from upstream: there is no
path re-wiring inside `vendor/wgpu`. (Consumers re-wire *to* it: compd's root
manifest, `vendor/iced`, `vendor/cryoglyph`.)

## Local edits

| file | +/− | what |
|---|---|---|
| `wgpu-hal/src/vulkan/device.rs` | +76/−0 | **local**: `Device::texture_from_dmabuf_fd_planar(fd, desc, drm_modifier, plane_layouts: &[(offset, row_pitch)])`, `#[cfg(unix)]`. Imports a multi-plane, non-disjoint DMA-buf (AMD DCC / Intel CCS: all planes in one BO) by building one `vk::SubresourceLayout` per plane into `ImageDrmFormatModifierExplicitCreateInfoEXT`, then reusing upstream's `create_image_without_memory_with_tiling`, `import_dmabuf_memory` and `texture_from_raw`. Upstream v30.0.0 has only the single-plane `texture_from_dmabuf_fd` (that one **is** upstream, at :525) |
| `wgpu-hal/src/vulkan/device.rs` | +2/−0 | noise: two blank lines inside upstream's `texture_from_dmabuf_fd` (:533, :579), residue of removed debugging. No effect |
| `wgpu-hal/src/vulkan/mod.rs` | +1/−0 | noise: one blank line before `profiling::scope!("vkQueueSubmit")` (:1489). No effect |
| `Cargo.lock` | −5522 | the workspace lock is not vendored. Harmless for compd (it builds `vendor/wgpu` through its own workspace lock); a standalone `cargo test` in `vendor/wgpu` regenerates one |

**compd does not use the delta.** compd builds wgpu with
`default-features = false, features = ["gles", "parking_lot", "std", "wgsl"]`
(root `Cargo.toml`), so the Vulkan HAL, and this function with it, is not
compiled. Its only caller was a Bevy import path that compd does not carry.
It is kept so a re-vendor does not silently lose it should a Vulkan path
return (Vulkan stays behind an off feature).

## Guards

`wgpu-hal/tests/compd_patch_guard.rs` (added by compd, marked
`// compd`) reads `src/vulkan/device.rs` with `include_str!`,
so it needs no Vulkan feature or device:

- `planar_dmabuf_import_is_present`: the planar function, its
  `plane_layouts: &[(u64, u64)]` signature, the per-plane `SubresourceLayout`
  map and the explicit-modifier create-info are present. Fails if a re-vendor
  drops the function.
- `upstream_helpers_the_planar_import_relies_on_still_exist`:
  `texture_from_dmabuf_fd`, `create_image_without_memory_with_tiling`,
  `import_dmabuf_memory` and `texture_from_raw` still exist. Fails when a newer
  upstream renames them and the planar function needs porting.

Run (from `vendor/wgpu`; writes a `Cargo.lock` there since none is vendored):

```
cargo test -p wgpu-hal --test compd_patch_guard
```

Cargo-free check, from the compd root; must print `1`:

```
grep -c 'pub unsafe fn texture_from_dmabuf_fd_planar(' vendor/wgpu/wgpu-hal/src/vulkan/device.rs
```

To re-check the base: `git diff --stat 8bf3e5ff4ab4 <tree> -- . ':!Cargo.lock'`
must list only `wgpu-hal/src/vulkan/device.rs` (+78) and
`wgpu-hal/src/vulkan/mod.rs` (+1), with `wgpu-hal/tests/compd_patch_guard.rs`
as the one compd addition.

Re-vendoring: take a later tag (v30.0.1 exists), re-apply the 76-line function,
drop the three blank lines, and run the guards.

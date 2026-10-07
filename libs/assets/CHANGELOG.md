# assets contract changes

## 0.1.2

Additive release on the 0.1.1 resolver: the v1 pipeline, error kinds and
search order are unchanged, and everything new is strict and bounded.

- `mixos.static-assets.v2`: a versioned manifest that declares icon
  metadata over the unchanged v1 fields — an `icon_default` selection,
  per-style `icon_catalogues` (family/style → locked font face →
  locked codepoints) and named non-font `icon_assets`. All declared
  references must be locked, pairs unique and counts capped
  (`MAX_ICON_CATALOGUES` 32, `MAX_ICON_ASSETS` 4096, face index and
  weight bounded). A v1 set derives its one default catalogue from the
  `icons` role as before, and a v1 nondefault-style request is still
  refused rather than guessed.
- The parsed v2 DTO is retained whole behind `manifest_v2()` on
  `AssetSet` and `VerifiedSet` (`None` for a v1 set). `manifest()` stays
  the read-only v1 projection of the shared fields: for a v2 set its
  `schema` reads `SCHEMA_V2`, so it cannot be re-validated or
  re-serialized as a v1 manifest.
- The `verified` feature: `read_verified` re-reads and captures every
  byte of a set through descriptor-relative opens into an owned
  `VerifiedSet`; `read_at`/`read_explicit` resolve an `ExplicitRequest`
  (set ID and optional exact manifest BLAKE3) under held approved root
  descriptors, selecting `sets/<id>` and never `current`. A pinned
  digest is checked immediately after the manifest re-parse, before the
  stylesheet or any locked payload is read; locked `.codepoints` files
  are refused at the 1 MiB catalogue bound before their bytes are
  allocated. `read_current` is the descriptor-owned analogue of
  `AssetSet::current`: only the initial omitted-resource selection
  follows `current`, exactly once, and nothing is re-opened through a
  path after the pin.
- Invalid UTF-8 in the manifest or an icon catalogue is classified as
  `Error::Invalid` on the open path too, matching the verified path and
  the documented error-kind contract; ordinary I/O failures keep their
  kind.

## 0.1.1

Initial release, shared with the workspace version: the `Lookup` root
search (XDG, environment or explicit roots), `AssetSet::open`/`current`
with layout, size and stylesheet checks, streaming `verify` over both
hashes, and the `mixos` search path.

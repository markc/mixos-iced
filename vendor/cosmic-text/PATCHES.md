# vendor/cosmic-text: local patches

cosmic-text 0.19.0 from crates.io, extracted unmodified. The local delta adds
a pinned-alias shaping path and an atomic font-registration transaction on
top of the existing `FontSystem`; no upstream line is changed outside the
files and tests listed below, and all legacy behaviour for unpinned
selections is preserved.

## Upstream base: crates.io 0.19.0

SHA-256 of the published `.crate`: `be17b688510d934ce13f48a2beba700e11583e281e0fda99c22bb256a14eda73`,
matching the `cosmic-text` entry in the workspace `Cargo.lock`. This
directory is that archive unpacked: `Cargo.toml.orig` is the authoring
manifest, and all upstream licence files (`LICENSE-MIT`, `LICENSE-APACHE`),
font licences (`fonts/*-LICENSE`) and headers are retained.

The published archive's normalized `Cargo.toml` already declares
`autotests = false` and eight explicit upstream `[[test]]` targets. These
were checked against the published archive identified above. The local
manifest adds only the `registration_seam` test target; all eight upstream
targets remain. `Cargo.toml.orig` is the authoring manifest, before Cargo's
publication normalization, and is not the comparison base.

To re-check: unpack the crate with the checksum above and diff; only the
files listed under "Local delta" may differ, with the fixture omission
below as an explicitly recorded exception.

## Fixture omission

The published archive's `fonts/InterVariable.ttf` is deliberately not
carried in this tree: the public hygiene gate rejects the raw bytes (an
accidental binary mesh-node-name match), and the allowlist stays closed.
The retained `fonts/InterVariable-Italic.ttf` covers the variable-weight
guards instead; the legacy `variable_font_weight` test loads those bytes
into an explicit empty database, so the face's family, style and ID are
discovered from the parsed bytes and no host font can supply a same-named
family. The archive base checksum above stays the recorded upstream
identity; this one omission is an explicit local difference on top of the
delta below, and the privately preserved boxed binary is unaffected.

## Local delta

| file | what |
|---|---|
| `Cargo.toml` | adds the `registration_seam` test target to the published normalized manifest, retaining all eight upstream targets |
| `src/font/mod.rs` | `Font` stores the weight it was constructed at and exposes `const fn weight()` |
| `src/font/system.rs` | `PinnedFaceRef`, `PinnedFontPolicy`, `FontRegistration`, `FontRegistrationResult`, `FontRegistrationError`; `FontSystem::register_fonts` (atomic transaction), `refresh_database` (legacy post-mutation rebuild), `pinned_aliases`; `derive_monospace_indexes` helper shared with the constructor; pinned branch in `get_font_matches` |
| `src/font/fallback/mod.rs` | `FontFallbackIter` pinned branch: declared candidates only, at the sealed policy weight, before any global fallback table |
| `src/shape.rs` | `ShapeGlyph.font_weight` comes from the instantiated `Font`, not the attributes; Basic swash metrics use the actual variation coords; pinned `shape_skip` branch and shared `shape_skip_replace_missing` helper |
| `tests/registration_seam.rs` | guard tests; the packaged fixtures are compiled in with source-relative `include_bytes!` instead of a `CARGO_MANIFEST_DIR` read, so the file builds unchanged from its own test target or from the root-owned guard target (see Guards) |
| `tests/variable_font_weight.rs` | legacy variable-weight guard: loads the retained italic fixture into an explicit empty database and asserts the parsed face ID at every weight; the fixture bytes are compiled in source-relatively |

## The seam

`FontSystem::register_fonts` is the single additive transaction on the
existing system. It rejects non-binary sources, missing existing IDs,
out-of-range added indices, empty policies/groups, duplicate or conflicting
aliases, invalid weights, unsupported face weights and unconstructible
faces. Hard bounds: faces per transaction (512), policies held in total
(1024 — the cap is checked against installed plus genuinely new staged
policies before any mutation, so repeated batches cannot grow the policy map
past it while no-op re-submissions stay stable), groups per policy (16),
face refs per policy (64) and alias length (256). A policy face must provide
the sealed weight exactly (static weight) or through a covering `wght` axis;
no nearest-weight substitution is implicit.

Alias conflict checks are ASCII-case-insensitive — against other aliases and
against public family names, including families declared by faces added in
the same transaction, so a new alias can never capture selections of a
public family that predates the registration. Pinned lookup itself is exact:
aliases are internal, caller-generated names and must be used verbatim.

Face metadata (families, post-script name, style, weight, stretch,
monospaced flag) is caller-supplied and trusted: the seam validates that the
source bytes parse at the declared index and can provide the sealed policy
weight, not that the declared metadata matches the bytes. The toolkit
registry derives intrinsic metadata from the bytes before constructing these
records; the guard tests' impostor faces are test-only examples of
caller-supplied metadata.

Staging clones the database (preserving every existing slotmap ID), appends
the faces with `push_face_info`, resolves policies to owned groups of actual
IDs, and instantiates every new `(face, policy weight)` with `Font::new`
before any live mutation. The commit swaps database, policy map, derived
monospace indexes and the instantiated font cache together; existing IDs,
loaded fonts and glyph caches are retained, the match and shape caches are
cleared, and no fallible semantic operation remains after the swap. Empty or
identical transactions do not mutate or invalidate. Identical aliases are
no-ops; conflicting aliases fail and can never be rebound.

Pinned aliases produce only their policy IDs in `get_font_matches` (group
order preserved, faces ranked within a group by requested style/stretch with
declared order breaking ties) and the global `db.query` promotion never runs
for them. `FontFallbackIter` detects the pinned alias at construction,
captures the sealed weight and ordered candidates, and yields them once
before terminating; the Basic `shape_skip` generic-fallback escape consumes
the same iterator, so no shaping path can promote a global font past a
pinned alias. The pinned iterator re-consumes candidates from the primary
when replacing missing glyphs; re-applying the primary face is a no-op (its
zero glyphs stay zero), so the repeat is deliberate and harmless. Distinct
policy `(face, weight)` instances are validated once per transaction;
policies sharing an instance are deduplicated. The policy weight is sealed
into the `Font` instance, and `ShapeGlyph.font_weight` reflects the instance
so raster cache lookups stay consistent; later attribute edits cannot
instantiate unbounded new weights.

`refresh_database` rebuilds the derived indexes after legacy `db_mut`
mutations (the old `db_mut` could only clear the match cache before handing
out its reference). The monospace index helper reads GPOS and GSUB
independently and merges their script tags, so a font carrying only one of
the tables still contributes its scripts.

## Guards

The executable owner is the root workspace's opt-in toolkit target, which
path-includes the two guard sources below unchanged and compiles them
against the same patched package the root patch resolves. The vendor test
targets above stay declared for upstream-style runs. Run from the
repository root at the checked/updated lock SHA:

```
cargo test --locked --profile release-fast -p toolkit --features font-registration-guards --test font_registration
```

The suite is the 15 registration guards below, the retained
`variable_font_all_weights_match` legacy guard and the ported iced wrapper
guards (see `vendor/iced/PATCHES.md`); a run that matches none of these
names is a failure.

| test | fails when |
|---|---|
| `declared_fallback_renders_and_global_cover_does_not` | Basic or Advanced shaping lets a global font cover a codepoint the pinned alias could not, or the declared fallback fails to cover it, or exhausted pinned coverage produces non-zero glyphs |
| `declared_group_order_beats_global_tables` | global fallback ordering (lower ID first) beats the declared within-group order |
| `duplicate_policy_and_empty_transaction_are_noops` | identical or empty transactions grow the database, install policies or mutate state |
| `failed_registrations_leave_the_system_unchanged` | any rejected registration (rebind, unsupported/invalid weight, bad refs, non-binary source, garbage bytes, empty policy/group, duplicate/conflicting alias, too many refs, alias too long) mutates the database, policy set or indexes |
| `missing_existing_face_is_rejected` | a policy referencing a removed ID is accepted |
| `old_paragraph_survives_same_named_registration` | adding a collection with identical public family names but different bytes re-binds a retained paragraph's IDs, metrics or raster output, or the same-named family fails to resolve its own alias to the new face |
| `sealed_weight_ignores_later_attribute_edits` | a later attribute weight escapes the sealed policy weight |
| `variable_policy_weights_change_basic_advanced_and_raster` | numeric sealed weights (350/650) fail to change Basic metrics, Advanced shaping and raster output, or lose their exact value |
| `registered_monospace_faces_reach_derived_indexes` | a newly registered monospaced face misses the general (and per-script) indexes |
| `single_layout_table_still_reaches_per_script_indexes` | a monospaced face whose GSUB is unreadable loses its GPOS scripts from the per-script index (the chained `gpos()?`/`gsub()?` failure mode) |
| `ttc_collection_faces_register_and_malformed_ttc_is_rejected` | a declared TTC face registers and shapes by index, an index past the declared count or a truncated declared TTC is accepted, or the rejections mutate state |
| `policy_total_reaches_boundary_then_rejects` | repeated batches grow the held policy map past its total cap, the boundary-crossing batch mutates state, or a no-op at the boundary grows it |
| `staged_family_capture_is_rejected` | an alias captures a public family name declared by a face added in the same transaction |
| `registered_bytes_survive_source_drop` | registered faces do not own their bytes |
| `unpinned_families_keep_legacy_behaviour` | pinned policies change fallback for ordinary named or generic families |
| `variable_font_all_weights_match` | a variable face is not matched at a weight inside its `wght` axis (100..=900), or any laid out glyph resolves to a face other than the fixture's own ID |

## Iced owner

`vendor/iced/graphics/src/text.rs` wraps the transaction
(`FontSystem::register_fonts`) and bumps its `Version` exactly once when
faces or policies were actually added; identical transactions do not bump,
and the (hypothetical) version overflow is checked before the cosmic commit.
Its `load_font` path now calls `refresh_database` after a successful
mutation. The wrapper's guard tests are owned by the root toolkit target
above, not by private test modules in the vendored tree. See
`vendor/iced/PATCHES.md`.
# Archive fixture storage

The upstream `.gitattributes` LFS filters are disabled in this vendored copy.
The crates.io archive contains actual font/image bytes; MixOS commits those
bytes directly so native worker guard tests and content checks use the same
immutable fixtures without an LFS service or clean/smudge transformation.

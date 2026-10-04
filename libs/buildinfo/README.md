# buildinfo

Compile-time build provenance for MixOS binaries: package name, version,
git sha (short and full), dirty bit and build time, plus the `--version`
contract every binary answers.

- `emit()` runs in a consumer's `build.rs` and captures the consumer
  repository's HEAD into `MIXOS_GIT_SHA`, `MIXOS_GIT_SHA_FULL`,
  `MIXOS_GIT_DIRTY`, `MIXOS_BUILD_TIME` and the embedded
  `MIXOS_BUILDINFO_MARKER`.
- `build_info!()` expands in the consumer crate and reads those variables
  into a `BuildInfo`.
- `exit_on_version!()` is `main`'s first statement: it answers
  `--version`/`-V` (and `--json`) and exits 0, or falls through.
- The binary also carries `MIXOS-BUILDINFO:1:{…}` as bytes, so an inventory
  can read provenance without running it (`find_markers`).

No dependencies. `SOURCE_DATE_EPOCH` pins the build time for reproducible
builds.

Test: `cargo test -p buildinfo`.

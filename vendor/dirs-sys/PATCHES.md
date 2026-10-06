# dirs-sys 0.5.0

Base: crates.io `dirs-sys` 0.5.0, archive SHA-256
`e01a3366d27ee9890022452ee61b2b63a67e6f13f58900b651ff5665f0bb1fab`,
VCS `8bcd4aa2c35990d57a2cff2953793525fc42709c` in
https://github.com/dirs-dev/dirs-sys-rs. The complete registry source and
original MIT and Apache-2.0 notices are retained.

The new workspace licence check exposed the existing MPL-2.0 `option-ext`
dependency through `dirs` and Term's terminal substrate. MixOS requires
permissive dependencies. Replace its one `OptionExt::contains` call with the
standard `Option` comparison, `user_dir == Some(key)`, and remove the import,
extern declaration and dependency. No option-ext source is incorporated.
Also add SPDX headers and an isolated workspace declaration.

Guard: `cargo test --locked -p dirs-sys` runs the upstream XDG parsing tests,
including matching/non-matching selection and all-directory enumeration.
`cargo deny check licenses bans sources` must pass without MPL exceptions;
the resolved graph must contain this patched dirs-sys and no option-ext.
Remove the patch when upstream carries the equivalent dependency removal.

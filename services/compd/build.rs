// Embed build provenance (git sha, dirty bit, build time, and the
// MIXOS-BUILDINFO:1 marker) for `compd --version`.
fn main() {
    buildinfo::emit();
}

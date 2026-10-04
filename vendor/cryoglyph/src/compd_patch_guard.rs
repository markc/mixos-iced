// compd: preservation guards for the local delta carried on top of upstream
// cryoglyph f4e7e4e (see vendor/cryoglyph/PATCHES.md).
//
// The wgpu-30 API port itself (Option<VertexBufferLayout>, Result-returning
// get_mapped_range_mut) is enforced by the compiler against vendor/wgpu. The
// generation-based eviction (`last_used`) is upstream (f4e7e4e) and needs no
// guard. What the compiler cannot catch is the manifest re-wiring: a re-vendor
// that restores `wgpu = "29"` builds a second wgpu.

const MANIFEST: &str = include_str!("../Cargo.toml");

#[test]
fn compd_manifest_takes_wgpu_from_vendor() {
    assert!(
        MANIFEST.contains(r#"wgpu = { path = "../wgpu/wgpu", version = "30"#),
        "cryoglyph must build against vendor/wgpu (wgpu 30), not crates.io"
    );
    let wgpu = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../wgpu/wgpu/Cargo.toml");
    assert!(
        wgpu.is_file(),
        "vendor/wgpu/wgpu must sit beside vendor/cryoglyph"
    );
}

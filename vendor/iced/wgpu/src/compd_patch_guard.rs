// compd: preservation guards for the local delta carried on top of upstream
// iced 3de451447 (see vendor/iced/PATCHES.md).
//
// Each test fails if a re-vendor drops one of the local edits. The pure
// wgpu-30 API port (Option<VertexBufferLayout>, Result-returning
// get_mapped_range*) is not guarded here: losing it is a compile error against
// vendor/wgpu. These guard what the compiler cannot: the path re-wiring and
// the values the port chose.

use std::path::{Path, PathBuf};

const WORKSPACE_MANIFEST: &str = include_str!("../../Cargo.toml");
const COMPOSITOR: &str = include_str!("window/compositor.rs");
const LIB: &str = include_str!("lib.rs");

fn iced_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

#[test]
fn compd_workspace_manifest_rewires_cryoglyph_and_wgpu() {
    assert!(
        WORKSPACE_MANIFEST.contains(r#"cryoglyph = { path = "../cryoglyph""#),
        "iced's workspace must take cryoglyph from vendor/cryoglyph, not git"
    );
    assert!(
        WORKSPACE_MANIFEST.contains(r#"wgpu = { path = "../wgpu/wgpu""#),
        "iced's workspace must take wgpu from vendor/wgpu, not crates.io"
    );
}

#[test]
fn compd_rewired_paths_resolve_to_one_wgpu() {
    let root = iced_root();
    let cryoglyph = root.join("../cryoglyph");
    assert!(
        cryoglyph.join("Cargo.toml").is_file(),
        "vendor/cryoglyph missing beside vendor/iced"
    );

    let wgpu_from_iced = root
        .join("../wgpu/wgpu")
        .canonicalize()
        .expect("vendor/wgpu/wgpu beside vendor/iced");
    let wgpu_from_cryoglyph = cryoglyph
        .join("../wgpu/wgpu")
        .canonicalize()
        .expect("vendor/wgpu/wgpu beside vendor/cryoglyph");
    assert_eq!(
        wgpu_from_iced, wgpu_from_cryoglyph,
        "iced and cryoglyph must build against the same wgpu"
    );

    let cryoglyph_manifest = std::fs::read_to_string(cryoglyph.join("Cargo.toml")).unwrap();
    assert!(
        cryoglyph_manifest.contains(r#"wgpu = { path = "../wgpu/wgpu""#),
        "vendor/cryoglyph must take wgpu from vendor/wgpu"
    );
}

#[test]
fn compd_surface_config_pins_auto_color_space() {
    assert!(COMPOSITOR.contains("color_space: wgpu::SurfaceColorSpace::Auto"));
}

#[test]
fn compd_adapter_requests_disable_limit_buckets() {
    // Window compositor and headless renderer both pass it explicitly.
    assert!(COMPOSITOR.contains("apply_limit_buckets: false"));
    assert!(LIB.contains("apply_limit_buckets: false"));
}

#[test]
fn compd_present_goes_through_queue_after_pre_present_hook() {
    let hook = COMPOSITOR
        .find("on_pre_present();")
        .expect("on_pre_present() call");
    let present = COMPOSITOR
        .find("renderer.engine.queue.present(frame);")
        .expect("wgpu 30 Queue::present(frame)");
    assert!(
        hook < present,
        "on_pre_present must run before the frame is presented"
    );
}

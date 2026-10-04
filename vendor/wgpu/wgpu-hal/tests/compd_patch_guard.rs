// compd: preservation guard for the local delta on wgpu v30.0.0.
//
// The local delta adds `Device::texture_from_dmabuf_fd_planar` to
// the Vulkan HAL. Upstream v30.0.0 has only the single-plane
// `texture_from_dmabuf_fd`. The function lives in the Vulkan backend, so it is not
// compiled in compd's GL-only build and cannot be exercised without a Vulkan
// device; this test reads the source instead and fails if a re-vendor drops it.
// See ../../PATCHES.md.

const DEVICE_RS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/vulkan/device.rs"
));

#[test]
fn planar_dmabuf_import_is_present() {
    for needle in [
        // the local entry point and its signature
        "pub unsafe fn texture_from_dmabuf_fd_planar(",
        "plane_layouts: &[(u64, u64)],",
        // one SubresourceLayout per (offset, row_pitch) plane, explicit modifier
        ".map(|&(offset, row_pitch)| vk::SubresourceLayout {",
        "vk::ImageDrmFormatModifierExplicitCreateInfoEXT::default()",
        ".plane_layouts(&layouts);",
    ] {
        assert!(
            DEVICE_RS.contains(needle),
            "local delta lost from wgpu-hal/src/vulkan/device.rs: missing `{needle}` \
             (re-apply the planar import, see vendor/wgpu/PATCHES.md)"
        );
    }
}

#[test]
fn upstream_helpers_the_planar_import_relies_on_still_exist() {
    for needle in [
        "pub unsafe fn texture_from_dmabuf_fd(",
        "fn create_image_without_memory_with_tiling(",
        "fn import_dmabuf_memory(",
        "pub unsafe fn texture_from_raw(",
    ] {
        assert!(
            DEVICE_RS.contains(needle),
            "upstream helper `{needle}` changed; port texture_from_dmabuf_fd_planar to it"
        );
    }
}

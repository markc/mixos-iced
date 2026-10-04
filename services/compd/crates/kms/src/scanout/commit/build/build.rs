//! A real implementation with working mechanism bodies, compiled under the
//! `native-scanout` cargo feature and exercised by the assembly self-test
//! (TEST_ONLY atomic commits, kernel-validated on the real device).
//!
//! Atomic request construction — the heart of the native commit path:
//! property discovery by name (cached per object) and request assembly for
//! the two commit shapes (full modeset, page flip). SRC_* are 16.16 fixed
//! point per the KMS uapi.
//!
//! Failure policy: a scanout object missing a required property is a device
//! the native path cannot drive — panic with the property name.

#[cfg(feature = "native-scanout")]
pub use gated::*;

#[cfg(feature = "native-scanout")]
mod gated {
    use smithay::backend::drm::DrmDeviceFd;
    use smithay::reexports::drm::control::atomic::AtomicModeReq;
    use smithay::reexports::drm::control::{
        connector, crtc, framebuffer, plane, property, Device as ControlDevice, Mode as DrmMode,
        ResourceHandle, ResourceHandles,
    };
    use std::collections::HashMap;

    /// Name -> handle map for one KMS object.
    pub struct PropMap(HashMap<String, property::Handle>);

    impl PropMap {
        pub fn get(&self, name: &str) -> property::Handle {
            *self
                .0
                .get(name)
                .unwrap_or_else(|| abort!("KMS object lacks required property {name}"))
        }

        pub fn has(&self, name: &str) -> bool {
            self.0.contains_key(name)
        }
    }

    /// Discover the property table of any KMS object.
    pub fn discover<H: ResourceHandle>(drm: &DrmDeviceFd, handle: H) -> PropMap {
        let set = drm
            .get_properties(handle)
            .unwrap_or_else(|e| abort!("property enumeration failed: {e}"));
        let (handles, _values) = set.as_props_and_values();
        let mut map = HashMap::with_capacity(handles.len());
        for h in handles {
            let info = drm
                .get_property(*h)
                .unwrap_or_else(|e| abort!("property info read failed: {e}"));
            map.insert(info.name().to_string_lossy().into_owned(), *h);
        }
        PropMap(map)
    }

    /// The primary plane compatible with a pipe.
    pub fn primary_plane(
        drm: &DrmDeviceFd,
        res: &ResourceHandles,
        pipe: crtc::Handle,
    ) -> plane::Handle {
        let planes = drm
            .plane_handles()
            .unwrap_or_else(|e| abort!("plane enumeration failed: {e}"));
        planes
            .into_iter()
            .find(|p| {
                let Ok(info) = drm.get_plane(*p) else { return false };
                if !res.filter_crtcs(info.possible_crtcs()).contains(&pipe) {
                    return false;
                }
                let props = discover(drm, *p);
                if !props.has("type") {
                    return false;
                }
                // type enum: 1 == PRIMARY (stable uapi value).
                let set = drm.get_properties(*p).unwrap();
                let (handles, values) = set.as_props_and_values();
                handles
                    .iter()
                    .zip(values.iter())
                    .any(|(h, v)| {
                        drm.get_property(*h)
                            .map(|i| i.name().to_bytes() == b"type" && *v == 1)
                            .unwrap_or(false)
                    })
            })
            .unwrap_or_else(|| abort!("no primary plane for the claimed pipe"))
    }

    /// The discovered property tables for one scanout pipeline.
    pub struct PipelineProps {
        pub connector: PropMap,
        pub crtc: PropMap,
        pub plane: PropMap,
    }

    pub fn pipeline_props(
        drm: &DrmDeviceFd,
        conn: connector::Handle,
        pipe: crtc::Handle,
        plane: plane::Handle,
    ) -> PipelineProps {
        PipelineProps {
            connector: discover(drm, conn),
            crtc: discover(drm, pipe),
            plane: discover(drm, plane),
        }
    }

    /// Plane geometry for the request (src in pixels; converted to 16.16).
    #[derive(Debug, Clone, Copy)]
    pub struct PlaneFrame {
        pub fb: framebuffer::Handle,
        pub src: (u32, u32),
        pub dst: (i32, i32, u32, u32),
    }

    fn set_plane(
        req: &mut AtomicModeReq,
        plane: plane::Handle,
        props: &PipelineProps,
        pipe: crtc::Handle,
        frame: PlaneFrame,
    ) {
        use property::Value;
        let p = &props.plane;
        req.add_property(plane, p.get("CRTC_ID"), Value::CRTC(Some(pipe)));
        req.add_property(plane, p.get("FB_ID"), Value::Framebuffer(Some(frame.fb)));
        req.add_property(plane, p.get("SRC_X"), Value::UnsignedRange(0));
        req.add_property(plane, p.get("SRC_Y"), Value::UnsignedRange(0));
        req.add_property(
            plane,
            p.get("SRC_W"),
            Value::UnsignedRange((frame.src.0 as u64) << 16),
        );
        req.add_property(
            plane,
            p.get("SRC_H"),
            Value::UnsignedRange((frame.src.1 as u64) << 16),
        );
        req.add_property(plane, p.get("CRTC_X"), Value::SignedRange(frame.dst.0 as i64));
        req.add_property(plane, p.get("CRTC_Y"), Value::SignedRange(frame.dst.1 as i64));
        req.add_property(plane, p.get("CRTC_W"), Value::UnsignedRange(frame.dst.2 as u64));
        req.add_property(plane, p.get("CRTC_H"), Value::UnsignedRange(frame.dst.3 as u64));
    }

    /// Full modeset request: connector -> pipe binding, mode blob, ACTIVE,
    /// primary plane frame.
    #[allow(clippy::too_many_arguments)]
    pub fn build_modeset(
        drm: &DrmDeviceFd,
        conn: connector::Handle,
        pipe: crtc::Handle,
        plane: plane::Handle,
        props: &PipelineProps,
        mode: &DrmMode,
        frame: PlaneFrame,
    ) -> AtomicModeReq {
        use property::Value;
        let mode_blob = drm
            .create_property_blob(mode)
            .unwrap_or_else(|e| abort!("mode blob creation failed: {e}"));

        let mut req = AtomicModeReq::new();
        req.add_property(conn, props.connector.get("CRTC_ID"), Value::CRTC(Some(pipe)));
        req.add_property(pipe, props.crtc.get("MODE_ID"), mode_blob);
        req.add_property(pipe, props.crtc.get("ACTIVE"), Value::Boolean(true));
        set_plane(&mut req, plane, props, pipe, frame);
        req
    }

    /// Page-flip request: plane frame only (mode and routing already live).
    pub fn build_flip(
        pipe: crtc::Handle,
        plane: plane::Handle,
        props: &PipelineProps,
        frame: PlaneFrame,
    ) -> AtomicModeReq {
        let mut req = AtomicModeReq::new();
        set_plane(&mut req, plane, props, pipe, frame);
        req
    }
}

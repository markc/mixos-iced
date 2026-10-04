//! Render / scanout / offload role assignment per node.
//! Single-node era: the primary is RenderAndScanout. The decision seam exists
//! so multi-GPU lands here, not in a renderer.

use smithay::backend::drm::DrmNode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeRole {
    RenderAndScanout,
    RenderOnly,
    ScanoutOnly,
    Offload,
}

/// Assign a role to `node` given the current primary. Today's policy: the
/// primary renders and scans out; any other node is offload-only.
pub fn assign(node: DrmNode, primary: Option<DrmNode>) -> NodeRole {
    match primary {
        Some(p) if p == node => NodeRole::RenderAndScanout,
        Some(_) => NodeRole::Offload,
        None => NodeRole::RenderAndScanout,
    }
}

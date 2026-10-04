//! Frame planning: Status -> ordered list of passes with tap points.
//!
//! This crate owns the knowledge that used to be duplicated as a
//! `(render_scene, render_lock)` match in both backends. Backends execute the
//! plan; they do not derive it.

use crate::draw::plan::tap::tap::{TapPoint, POST_SCENE};
use world::state::state::Status;

/// A render pass kind. The *meaning* of each kind (which element source feeds
/// it) is compositor vocabulary; backends only map kinds to element sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FramePass {
    /// The desktop scene (windows, layers).
    Scene,
    // The session-lock and picker-overlay passes are not part of this plan.
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanStep {
    Pass(FramePass),
    Tap(TapPoint),
}

#[derive(Debug, Clone, Default)]
pub struct FramePlan {
    pub steps: Vec<PlanStep>,
}

impl FramePlan {
    pub fn is_empty(&self) -> bool {
        !self.steps.iter().any(|s| matches!(s, PlanStep::Pass(_)))
    }

    pub fn has_pass(&self, pass: FramePass) -> bool {
        self.steps.iter().any(|s| matches!(s, PlanStep::Pass(p) if *p == pass))
    }

    pub fn has_tap(&self, tap: TapPoint) -> bool {
        self.steps.iter().any(|s| matches!(s, PlanStep::Tap(t) if *t == tap))
    }
}

/// Encode the pass ordering:
/// - Running   -> Scene, Tap(post-scene)
/// - Terminate -> (empty)
pub fn plan(status: &Status) -> FramePlan {
    use PlanStep::*;
    let steps = match status {
        Status::Running => vec![Pass(FramePass::Scene), Tap(POST_SCENE)],
        Status::Terminate => vec![],
    };
    FramePlan { steps }
}

use smithay::desktop::Window;
use smithay::utils::{Logical, Point, Rectangle};
use uuid::Uuid;

pub enum CanvasGrab {
    None,

    Target(TargetOption),
    Active(ActiveOption),
}

pub enum ActiveOption {
    Moving {
        candidates: ActiveTransformCandidate,
        start_cursor: Point<f64, Logical>,
        Anchor: Anchor,
        /// Snap targets captured once at grab start (see [`SnapMap`]).
        snap: SnapMap,
    },
    Scaling {
        candidates: ActiveTransformCandidate,
        start_cursor: Point<f64, Logical>,
        Anchor: Anchor,
        /// Snap targets captured once at grab start (see [`SnapMap`]).
        snap: SnapMap,
    },
    SelectBox {
        start_cursor: Point<f64, Logical>,
        current_cursor: Point<f64, Logical>,
        start_selection: Vec<Uuid>,
    },

    Hand,
}

pub enum ActiveTransformCandidate {
    Window(Vec<(Window, Rectangle<i32, Logical>)>),
    Placeholder(Uuid, Rectangle<i32, Logical>),
}
pub enum TargetOption {
    Scale,
    Move,
    Select { Append: bool },
}

pub struct Anchor {
    pub Horizontal: bool,
    pub Vertical: bool,
}

/// A snap source captured at grab start: a window's or placeholder's rect plus
/// whether it was on-screen (in the exact viewport) at that moment. The distance
/// exclusion is skipped for `visible` sources, so two windows both visible on
/// screen snap regardless of how far apart they are.
#[derive(Clone, Copy)]
pub struct SnapSource {
    pub rect: Rectangle<i32, Logical>,
    pub visible: bool,
}

/// Snap targets captured at grab start, fixed for the grab's lifetime. Built from
/// the other windows' + visible placeholders' rects (`sources`) plus the screen
/// edges (`vertical`/`horizontal`). The transformed windows/placeholders are
/// excluded. `sources` are world-space rects whose edges become snap lines, gated
/// per motion frame by the zoom-scaled exclusion range against the moving geom
/// (only for non-visible sources); `vertical` (x) and `horizontal` (y) are the
/// always-on screen-edge lines.
#[derive(Clone, Default)]
pub struct SnapMap {
    pub sources: Vec<SnapSource>,
    pub vertical: Vec<f64>,
    pub horizontal: Vec<f64>,
}

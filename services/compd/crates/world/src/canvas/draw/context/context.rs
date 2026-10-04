use smithay::utils::{Logical, Point, Rectangle};

pub struct Context {
    pub cursor: Cursor,
    pub viewport: (f64, f64),
    pub bound: Rectangle<i32, Logical>
}

pub struct Cursor {
    pub position: Point<f64, Logical>,
}
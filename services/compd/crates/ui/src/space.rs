//! World/screen space and camera transform.
//!
//! Each `IcedItem` has an `IcedSpace`:
//! - `World`: position is in compositor world coords. Pan/zoom apply at
//!   render and hit-test time.
//! - `Screen`: position is in physical pixels on the output. Pan/zoom do
//!   not apply.
//!
//! The registry does **not** store camera state. The compositor passes a
//! `Transform` (and output size) to render and hit-test calls each frame,
//! mirroring how `window.render_elements(...)` accepts the screen
//! position and zoom as arguments.
//!
//! The transform is the **centered-origin** model, matching your
//! `global_to_canvas` / `logical_to_screen`:
//!
//!   `screen = (world - position) * zoom + output_size / 2`

use smithay::utils::{Physical, Point, Size};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IcedSpace {
    #[default]
    World,
    Screen,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform {
    pub position: Point<f64, Physical>,
    pub zoom: f64,
}

impl Transform {
    pub fn identity() -> Self {
        Self {
            position: Point::from((0.0, 0.0)),
            zoom: 1.0,
        }
    }

    /// World point → screen point. Centered origin.
    pub fn world_to_screen(
        &self,
        output_size: Size<f64, Physical>,
        world: Point<f64, Physical>,
    ) -> Point<f64, Physical> {
        Point::from((
            (world.x - self.position.x) * self.zoom + output_size.w / 2.0,
            (world.y - self.position.y) * self.zoom + output_size.h / 2.0,
        ))
    }

    /// Screen point → world point. Inverse of `world_to_screen`.
    pub fn screen_to_world(
        &self,
        output_size: Size<f64, Physical>,
        screen: Point<f64, Physical>,
    ) -> Point<f64, Physical> {
        Point::from((
            (screen.x - output_size.w / 2.0) / self.zoom + self.position.x,
            (screen.y - output_size.h / 2.0) / self.zoom + self.position.y,
        ))
    }
}

impl Default for Transform {
    fn default() -> Self {
        Self::identity()
    }
}

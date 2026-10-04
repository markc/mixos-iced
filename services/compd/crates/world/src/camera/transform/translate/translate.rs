use smithay::output::Scale;
use smithay::utils::{Logical, Point, Size};
use crate::camera::transform::state::state::Transform;

// Our world is anchored to center center while smithay logical space is top left.
// This functions receives logical space. It scales the results which is necessary due to anchor being different
// pub fn space_to_world(
//     camera: &Transform,
//     size: Size<f64, Logical>,
//     screen_pos: Point<f64, Logical>,
//     scale: Scale
// ) -> smithay::utils::Point<f64, Logical> {
//     let centered_x = screen_pos.x - (size.w / 2.0);
//     let centered_y = screen_pos.y - (size.h / 2.0);

//     let unscaled_x = centered_x / camera.zoom();
//     let unscaled_y = centered_y / camera.zoom();

//     smithay::utils::Point::from((
//         unscaled_x + camera.position().x,
//         unscaled_y + camera.position().y,
//     )).upscale(scale.fractional_scale())
// }

// pub fn global_to_canvas(
//     camera: &Transform,
//     size: Size<f64, Logical>,
//     pos: Point<f64, Logical>,
//     scale: Scale
// ) -> smithay::utils::Point<f64, Logical> {
//     logical_to_screen(&camera, size, pos, scale)
// }

// pub fn logical_to_screen(
//     camera: &Transform,
//     size: Size<f64, Logical>,
//     pos: Point<f64, Logical>,
//     scale: Scale
// ) -> smithay::utils::Point<f64, Logical> {
//     let sx = ((pos.x - camera.position.x) * camera.zoom) + (size.w as f64 / 2.0);
//     let sy = ((pos.y - camera.position.y) * camera.zoom) + (size.h as f64 / 2.0);

//     smithay::utils::Point::from((sx, sy)).downscale(scale.fractional_scale())
// }

// pub fn scale(camera: &Transform, point: f64) -> f64 {
//     return point * camera.zoom();
// }

//  space_to_world
// This is actually screen to world which is also space to world since they share coords.
// SpaceCoords = Screen Position, Smithay Position. both anchor top left as (0,0)
// WorldPos = Screen Position, Smithay Position, affected by zoom and pan. anchors the center. important for animations

// global_to_canvas
// This is actually space to world
// The space coordinate system is in 'global' space described by Smithay.
// For example:
// 1. Pointer.getCurrentLocation
// 2. window_bbox() and element_location() function from Space<Window>

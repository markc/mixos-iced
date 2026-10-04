use smithay::desktop::LayerSurface;
use smithay::utils::{Logical, Physical, Point, Size};
use dispatcher::wayland::layershell::dispatch::dispatch::OverlayPlacement;
pub use dispatcher::wayland::layershell::dispatch::dispatch::*;

/// Pure placement core. `cursor_world` is only consulted by `FollowCursor`; it is
/// the same point the rim reads from the seat and the hit-test is already probing.
/// The rim/draw `&Loop` wrapper lives in interface.base.
pub fn layer_surface_position_core(
    cursor_world: Point<f64, Logical>,
    ctx: crate::camera::transform::translate::transform::Context,
    layer: &LayerSurface,
    output_size: Size<i32, Logical>,
) -> Point<i32, Logical> {
    let wl_surface = layer.wl_surface();
    let surface_size = layer.bbox().size;

    let placement = smithay::wayland::compositor::with_states(wl_surface, |states| {
        states
            .data_map
            .get::<OverlayPlacement>()
            .cloned()
            .unwrap_or(OverlayPlacement {
                anchor_mode: AnchorMode::BottomCenter,
                margin: 12,
            })
    });

    match placement.anchor_mode {
        AnchorMode::BottomCenter => Point::from((
            (output_size.w - surface_size.w) / 2,
            output_size.h - surface_size.h - placement.margin,
        )),
        AnchorMode::TopCenter => {
            Point::from(((output_size.w - surface_size.w) / 2, placement.margin))
        }
        AnchorMode::FollowCursor { offset_x, offset_y } => {
            let t: crate::camera::transform::translate::transform::Transform =
                (cursor_world, ctx).into();
            let cursor_screen: Point<f64, Physical> = t.into();
            Point::from((
                cursor_screen.x as i32 + offset_x,
                cursor_screen.y as i32 + offset_y,
            ))
        }
        AnchorMode::Free { x, y } => Point::from((x, y)),
    }
}

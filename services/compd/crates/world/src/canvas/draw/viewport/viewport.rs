use smithay::backend::renderer::{ImportAll, ImportMem, Texture};
use smithay::utils::{Logical, Physical, Point, Size};
use crate::state::Loop;

pub fn context<R>(
    state: &mut Loop,
    renderer: &mut R,
    size: Size<i32, Physical>,
) -> crate::canvas::draw::context::context::Context
where
    R: smithay::backend::renderer::Renderer + ImportAll + ImportMem,
    R::TextureId: Texture + Clone + Send + 'static,
{
    let pointer = state.state.seat.seat.get_pointer().unwrap();
    let cursor_logical = pointer.current_location();

    let logical_w = size.w as f64 / state.inner.camera_mut().transform.zoom();
    let logical_h = size.h as f64 / state.inner.camera_mut().transform.zoom();

    // Create a bounding box for custom culling (in logical world coordinates)
    let camera_bbox = smithay::utils::Rectangle::new(
        Point::<i32, Logical>::new(
            (state.inner.camera_mut().transform.position().x - logical_w / 2.0).floor() as i32,
            (state.inner.camera_mut().transform.position().y - logical_h / 2.0).floor() as i32,
        ),
        Size::<i32, Logical>::new(logical_w.ceil() as i32, logical_h.ceil() as i32),
    );

    crate::canvas::draw::context::context::Context {
        cursor: crate::canvas::draw::context::context::Cursor {
            position: cursor_logical,
        },
        viewport: (logical_w, logical_h),
        bound: camera_bbox,
    }
}

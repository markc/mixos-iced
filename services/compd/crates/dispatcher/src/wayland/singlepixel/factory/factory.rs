use smithay::{
    reexports::{
        wayland_protocols::wp::single_pixel_buffer::v1::server::wp_single_pixel_buffer_manager_v1::WpSinglePixelBufferManagerV1,
        wayland_server::{Dispatch, DisplayHandle, GlobalDispatch},
    },
    wayland::{GlobalData, single_pixel_buffer::SinglePixelBufferState},
};
use crate::state::state::DispatchWire;
use protocols::singlepixel::state::SinglePixel;

pub fn new<I: DispatchWire>(display_handle: &DisplayHandle) -> SinglePixel
where
    I: GlobalDispatch<WpSinglePixelBufferManagerV1, GlobalData>,
    I: Dispatch<WpSinglePixelBufferManagerV1, GlobalData>,
    I: 'static,
{
    
    let single_pixel_buffer_state = SinglePixelBufferState::new::<I>(&display_handle);
    SinglePixel {
        single_pixel_buffer_state,
    }
}

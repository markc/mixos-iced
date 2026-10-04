use smithay::desktop::Window;
use dispatcher::wire::trait_::surface_event::SurfaceHandle;
use crate::state::Loop;

pub trait DrawWindow {
    fn visible(&self, _loop: &Loop) -> bool;
}

impl DrawWindow for Window {
    // The comp registry decides: a window minimised or off the current
    // workspace is not drawn, gets no frame callbacks and takes no pointer
    // hits (`CompState::hidden`).
    fn visible(&self, _loop: &Loop) -> bool {
        SurfaceHandle::of_window(self).is_none_or(|handle| !_loop.inner.comp.hidden(&handle))
    }
}

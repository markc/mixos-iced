use smithay::reexports::wayland_protocols::wp::presentation_time::server::{wp_presentation, wp_presentation_feedback};
use smithay::reexports::wayland_server::{Dispatch, DisplayHandle, GlobalDispatch};
use smithay::wayland::GlobalData;
use smithay::wayland::presentation::{PresentationData, PresentationState};
use crate::state::state::DispatchWire;
use protocols::presentation::state::Presentation;

pub fn new<I: DispatchWire>(display_handle: &DisplayHandle) -> Presentation  where I: GlobalDispatch<wp_presentation::WpPresentation, PresentationData>
+ Dispatch<wp_presentation::WpPresentation, PresentationData>
+ Dispatch<wp_presentation_feedback::WpPresentationFeedback, GlobalData>
+ 'static,{
    // Part of loop construction. Shouldn't be here at all.
    let mut presentation_state = PresentationState::new::<I>(&display_handle, 1);
    return Presentation { presentation_state };
}

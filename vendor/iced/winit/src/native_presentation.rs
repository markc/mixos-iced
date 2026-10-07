// SPDX-License-Identifier: MIT
//! Shared native feedback admission, recovery pacing and capacity metadata.

pub(crate) use platform::{capacity, feedback, recover_redraw};

#[cfg(all(feature = "wayland", any(target_os = "linux", target_os = "freebsd",
    target_os = "dragonfly", target_os = "netbsd", target_os = "openbsd")))]
mod platform {
    use crate::core::window::presentation::FrameOutcome;
    use winit::{platform::wayland::WindowExtWayland, presentation::{PresentationCapacity, PresentationError}, window::Window};

    pub(crate) fn feedback(window: &Window) -> Result<u64, FrameOutcome> {
        window.request_presentation_feedback().map(|id| id.get()).map_err(|error| match error {
            PresentationError::Unsupported => FrameOutcome::Unsupported,
            PresentationError::Capacity => FrameOutcome::Capacity,
            PresentationError::Exhausted => FrameOutcome::Exhausted,
            PresentationError::Closed => FrameOutcome::Closed,
        })
    }

    pub(crate) fn capacity(window: &Window) -> Result<PresentationCapacity, PresentationError> {
        window.presentation_capacity()
    }

    pub(crate) fn recover_redraw(window: &Window, pre_present_called: bool) {
        if pre_present_called {
            window.request_redraw_after_present_failure();
        } else {
            window.request_redraw();
        }
    }
}

#[cfg(not(all(feature = "wayland", any(target_os = "linux", target_os = "freebsd",
    target_os = "dragonfly", target_os = "netbsd", target_os = "openbsd"))))]
mod platform {
    use crate::core::window::presentation::FrameOutcome;
    use winit::{presentation::{PresentationCapacity, PresentationError}, window::Window};

    pub(crate) fn feedback(_: &Window) -> Result<u64, FrameOutcome> {
        Err(FrameOutcome::Unsupported)
    }

    pub(crate) fn capacity(_: &Window) -> Result<PresentationCapacity, PresentationError> {
        Err(PresentationError::Unsupported)
    }

    pub(crate) fn recover_redraw(window: &Window, _: bool) {
        window.request_redraw();
    }
}

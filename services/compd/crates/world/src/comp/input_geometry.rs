// SPDX-License-Identifier: MIT OR Apache-2.0

//! Applied client shape notifications for stationary pointer reconciliation.
//! No buffer identity, damage or commit counter: unchanged repaints are quiet.

use std::sync::Mutex;

use smithay::desktop::Window;
use smithay::utils::{Logical, Rectangle, Size};
use smithay::wayland::seat::WaylandFocus;

use super::CompState;

impl CompState {
    pub fn mark_input_geometry_dirty(&mut self) {
        self.input_geometry_dirty = true;
    }

    pub fn input_geometry_dirty(&self) -> bool {
        self.input_geometry_dirty
    }

    pub fn clear_input_geometry_dirty(&mut self) {
        self.input_geometry_dirty = false;
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Shape {
    drawn: bool,
    geometry: Rectangle<i32, Logical>,
    root_destination: Option<Size<i32, Logical>>,
    bounds: Rectangle<i32, Logical>,
    decorated: bool,
}

#[derive(Default)]
struct Observed(Mutex<Option<Shape>>);

/// Called only after the commit drain applies the root's state and bbox.
/// Pending synchronised-child state cannot stand in for the applied root.
pub(crate) fn observe(window: &Window) -> bool {
    let shape = Shape {
        drawn: protocols::window::ident::ident::is_drawn(window),
        geometry: window.geometry(),
        root_destination: window.wl_surface().as_deref()
            .and_then(crate::surface::interface::core::hit::root_dst),
        bounds: window.bbox(),
        decorated: decor::window::decorated(window),
    };
    // Window-owned state also survives an X11 surface re-association; window
    // destruction retires it without a separate cleanup table.
    window.user_data().insert_if_missing_threadsafe(Observed::default);
    let Some(observed) = window.user_data().get::<Observed>() else {
        return false;
    };
    let mut previous = observed.0.lock().unwrap_or_else(|error| error.into_inner());
    if previous.as_ref() == Some(&shape) {
        return false;
    }
    *previous = Some(shape);
    true
}

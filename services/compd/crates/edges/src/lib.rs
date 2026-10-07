//! The desktop shell's edge model.
//!
//! One output has four edges. Each edge carries a panel with a persistent
//! mode (hidden, pinned or docked), a logical-pixel thickness and a
//! reversible reveal/conceal motion, and a carousel of named pages of which
//! one is shown at a time. The four corners summon their counter-clockwise
//! edge; a detector turns pointer samples into corner engagement events.
//! [`ShellModel`] aggregates all of it for one output.
//!
//! Everything here is pure state driven by monotonic time. Nothing in this
//! crate depends on a UI engine, a window system, a compositor implementation
//! or the Bus; the host feeds inputs in and reads snapshots out.

#![forbid(unsafe_code)]
#![warn(clippy::mod_module_files)]

mod carousel;
mod corner;
mod keyboard;
mod motion;
mod panel;
mod shell;
mod types;

pub use carousel::{Carousel, CarouselError};
pub use corner::{
    CornerDetector, CornerDetectorConfig, CornerDetectorError, CornerDiagnostics, CornerEvent,
    CornerTrigger, PointerSample,
};
pub use keyboard::{FocusDirective, FocusStop, keyboard_target_output, next_focus_stop};
pub use motion::{MotionError, PanelMotion};
pub use panel::{
    ConcealReason, HORIZONTAL_RESIZE_RANGE, PanelConfig, PanelConfigError, PanelEffect,
    PanelInput, PanelMode, PanelSnapshot, PanelStateMachine, PanelTimeError, PanelUpdate,
    PanelWake, RevealTrigger, VERTICAL_RESIZE_RANGE, resize_thickness_range,
};
pub use shell::{FOCUS_GRANT_TIMEOUT, PanelPreference, PersistentPanel, ShellError, ShellModel};
pub use types::{
    Corner, Edge, GeometryError, LogicalPoint, LogicalSize, LogicalVector, Orientation, OutputKey,
    OutputKeyError, seed_panel_thickness,
};

//! The message type shared by every capture overlay UI.
//!
//! Used three ways:
//! - as each capture iced UI's `IcedUi::Message` (the widgets emit these),
//! - carried over the surface message channel as `SurfaceMessageType::Capture`,
//! - dispatched *into* indicator UIs (`SetRegion` / `SetCountdown`) by the
//!   interface to update their drawn state live.

/// What kind of thing the capture targets. Carries no geometry — the concrete
/// target (with data) is [`crate::capture::session::session::CaptureTarget`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetKind {
    /// The windows currently selected in the canvas (`canvas.Select`).
    Windows,
    /// A rectangle anchored in world: pans/zooms with the camera.
    WorldRegion,
    /// A rectangle fixed in screen/output pixels: stays put under the camera.
    ScreenRegion,
    /// The whole output.
    FullScreen,
}

/// Screenshot vs. video. (Saving/encoding is deferred; this only changes the
/// session lifecycle — video arms the 5-minute keep-alive dialog.)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureMedia {
    Screenshot,
    Video,
}

/// Cursor position in overlay-local coordinates. The setup overlay is a
/// full-screen screen-space instance, so these map to output pixels (1:1 at
/// scale 1.0).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverlayPoint {
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverlayRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

#[derive(Clone, Debug)]
pub enum CaptureMessage {
    // ---- setup chooser (SetupOverlay → interface) ----
    SelectKind(TargetKind),
    SelectMedia(CaptureMedia),
    /// Toggle the transparent-background ("no background") option — when on, the
    /// capture omits the parallax/iced backdrop and emits transparent where no
    /// window is. Applies to window/world targets, video and screenshot.
    SetNoBackground(bool),
    /// Pointer pressed: begin a region drag at the last-tracked cursor
    /// (region targets only).
    DragStart,
    /// Pointer moved: tracks the cursor and, while dragging, extends the
    /// region rect.
    DragMove(OverlayPoint),
    /// Pointer released: finish the region drag.
    DragEnd,
    /// Commit the setup and begin capturing.
    Confirm,
    /// Abandon setup.
    Cancel,

    // ---- active (StopHud → interface) ----
    /// Stop the active capture (saving deferred — just stops).
    Stop,

    // ---- video keep-alive dialog (ContinueDialog → interface) ----
    ContinueCapture,
    StopFromDialog,

    // ---- save dialog (SaveDialog → interface) ----
    /// Write to the default location (XDG Pictures/Videos).
    SaveDefault,
    /// Open the XDG portal "Save As" dialog.
    SaveAs,
    /// Throw the capture away.
    Discard,
    /// Toggle the "Optimized encoding" checkbox in the save dialog (video only).
    ToggleOptimized(bool),

    // ---- inbound: interface → encoding dialog ----
    /// Update the optimized-encode progress shown (0..=100 percent).
    SetEncodeProgress(u32),

    // ---- inbound: interface → indicator UIs ----
    /// Update the region rect a border/dim element draws (screen pixels).
    SetRegion(OverlayRect),
    /// Update the countdown the continue dialog shows (seconds remaining).
    SetCountdown(u32),
}

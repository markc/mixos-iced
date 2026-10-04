// The effect vocabulary the policy decisions return. Each variant names a
// step the engine performs; the doc says which.

use comp_model::request::WindowState;
use surfaces::SurfaceId;

/// One thing the engine must do, in list order.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    /// Record `cause` as this surface's next `props.changed` cause.
    MarkDirty { id: SurfaceId, cause: &'static str },
    /// The window left the current workspace: end its chrome pointer grab and any
    /// client move/resize, discard its pending presentation feedback
    /// (`DiscardReason::Workspace`), re-derive an X11 window's suspended
    /// flag, and mark it dirty with `cause`.
    Withdraw { id: SurfaceId, cause: &'static str },
    /// The window arrived on the current workspace: re-derive an X11 window's suspended
    /// flag and mark it dirty with `cause`.
    Present { id: SurfaceId, cause: &'static str },
    /// The registry already relabelled this record's workspace: publish
    /// `_NET_WM_DESKTOP` for a managed X11 window (never for
    /// override-redirect) and mark it dirty (`workspace.move`, or
    /// `workspace.count` for a stranded window).
    Relabelled { id: SurfaceId, to: u32, cause: &'static str },
    /// Mark the `workspaces.*` subtree dirty with this cause.
    WorkspacesDirty(&'static str),
    /// Publish the EWMH root pair (`_NET_NUMBER_OF_DESKTOPS`,
    /// `_NET_CURRENT_DESKTOP`).
    PublishDesktops,
    /// Re-derive every managed X11 window's suspended flag and
    /// `_NET_WM_DESKTOP` (a count shrink or a topology change).
    ResyncAllX11,
    /// The one visibility settle every workspace change ends in: clear the
    /// titlebar click candidate, recompute effective visibility, arbitrate
    /// keyboard focus preferring `prefer`, retarget the pointer, sync X
    /// stacking.
    Settle { prefer: Option<SurfaceId> },
    /// Raise within the window's own stacking band.
    Raise(SurfaceId),
    /// Activate: raise, focus, retarget the pointer (Alt+Tab activation).
    Activate(SurfaceId),
    /// Keyboard focus only, no raise.
    Focus(SurfaceId),
    /// Stacking or visibility changed what is under the cursor.
    RetargetPointer,
    /// Request maximised/fullscreen through the configure machinery. The
    /// engine refuses `configure_refused` when the request did not stick.
    RequestWindowState {
        id: SurfaceId,
        state: WindowState,
        enabled: bool,
    },
    /// The fullscreen output selection (`None` clears it).
    SetFullscreenOutput { id: SurfaceId, output: Option<String> },
    /// Minimise / restore through the shared visibility funnel.
    Minimize(SurfaceId),
    Restore(SurfaceId),
    /// The polite close: xdg `close` / X11 `WM_DELETE_WINDOW`.
    ClosePolite(SurfaceId),
    /// Kill the window's Wayland client.
    KillClient(SurfaceId),
    /// End a client move/resize that would steer the window straight back.
    FinishInteractive(SurfaceId),
    /// Move the window-geometry origin to `(x, y)` (global logical).
    MoveTo { id: SurfaceId, x: f32, y: f32 },
    /// Configure a new size at `(x, y)`. When the engine sends no configure
    /// (the size is already current) it moves to `(x, y)` instead.
    Resize {
        id: SurfaceId,
        x: f32,
        y: f32,
        width: i32,
        height: i32,
    },
}

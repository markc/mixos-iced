use slots::storage::token::base::{Token, TokenMut};

/// Lid + display driver channel between the kernel (which owns the DRM topology
/// and the session/render loop) and the rim (which owns input + policy).
///
/// Direction is strictly kernel → orchestration for *crate* dependencies, so the
/// rim cannot reach the kernel's `Topology` directly. Instead the kernel writes a
/// primitive `DisplaySnapshot` the rim reads, and the rim writes a
/// `DisplayRequest` the kernel's loop drains and performs (DPMS / suspend /
/// output switch). Both live in `Orchestrator.kernel` storage by token, like the
/// other driver data (resume, capture, …).

/// Kernel → rim: the display facts a lid policy needs. Primitive-only so no
/// kernel/smithay types leak across the layer boundary.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DisplaySnapshot {
    /// An external (non-internal-panel) connector is currently connected.
    pub external_present: bool,
    /// The active output is the internal laptop panel.
    pub internal_active: bool,
}

/// Debounced physical lid position, tracked so repeated identical toggles don't
/// re-fire the policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LidPosition {
    Open,
    Closed,
}

/// Rim → kernel: a pending display action for the kernel loop to perform. The
/// loop drains at most one per iteration. Effects are implemented kernel-side
/// (logind suspend, DRM DPMS, connector switch).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayRequest {
    /// System suspend (logind). The internal panel has no external companion.
    Suspend,
    /// Power the internal panel off (DPMS) without suspending — docked / blank.
    PanelOff,
    /// Power the internal panel back on (DPMS).
    PanelOn,
    /// Move the active output to the external connector (docked lid-close).
    SwitchToExternal,
    /// Move the active output back to the internal panel (undock / lid-open).
    SwitchToInternal,
}

/// Kernel-written display facts, read by the rim lid policy.
pub static DISPLAY_SNAPSHOT: Token<DisplaySnapshot> = Token::new();
pub static DISPLAY_SNAPSHOT_MUT: TokenMut<DisplaySnapshot> = TokenMut::new(&DISPLAY_SNAPSHOT);

/// Last debounced lid position the rim acted on (`None` until the first event).
pub static LID_POSITION: Token<Option<LidPosition>> = Token::new();
pub static LID_POSITION_MUT: TokenMut<Option<LidPosition>> = TokenMut::new(&LID_POSITION);

/// Rim-issued display action, drained by the kernel loop (`None` when idle).
pub static DISPLAY_REQUEST: Token<Option<DisplayRequest>> = Token::new();
pub static DISPLAY_REQUEST_MUT: TokenMut<Option<DisplayRequest>> = TokenMut::new(&DISPLAY_REQUEST);

/// Render gate: `true` while the panel is DPMS-off. The frame executor skips
/// while set — a page-flip would re-power the connector (legacy DPMS auto-on on
/// commit), so rendering must halt in tandem with the DPMS-off.
pub static DISPLAY_OFF: Token<bool> = Token::new();
pub static DISPLAY_OFF_MUT: TokenMut<bool> = TokenMut::new(&DISPLAY_OFF);

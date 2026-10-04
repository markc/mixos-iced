//! surfaces: compd's all-role surface registry.
//!
//! Every `wl_surface` that takes a role gets one [`SurfaceRecord`]: a numeric
//! id that is never reused, the role, a role generation that fences
//! `{id, generation}` Bus targets, the map state, an optional workspace and
//! the identity leaves the `comp.*` wire publishes. Windows and focus are
//! *projections* of the records ([`Registry::windows`],
//! [`Registry::focus_window`]), never stored separately.
//!
//! The registry is engine-agnostic: it is generic over the engine's surface
//! handle `H` (a smithay `WlSurface`, an X11 window, a test integer). The
//! engine calls it from its role, map and destroy hooks.
//!
//! The engine stamps a uuid v7 on toplevels and X11 windows only. The id<->uuid
//! adapter ([`Registry::bind_uuid`], [`Registry::id_for_uuid`]) keeps that
//! uuid as a secondary index on the record: every role gets an id, only the
//! uuid-carrying roles get a uuid, and the `comp.*` wire stays numeric.
//!
//! Contents: `SurfaceId`, `StackBand`, `SurfaceRecord`, `SurfaceRole::kind`
//! and `managed_toplevel`, `next_role_generation`,
//! `toplevel_root_for_surface`; `SeatKind` and the client-visible seat names;
//! `WindowTargetError` and `resolve_window_target`; `carries_workspace`;
//! `band_name` and the `windows.*` / `focus.window` projection rules.

mod registry;

pub use registry::{
    BindOutcome, Registry, RegistryError, SurfaceRecord, WindowTargetError,
};

/// A surface's numeric id. Assigned when the `wl_surface` first takes a
/// role, kept across role changes, and never reused after the surface is
/// destroyed.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SurfaceId(pub u64);

/// The advertised `wl_seat` name of the human seat. Seat names are
/// client-visible.
pub const HUMAN_SEAT_NAME: &str = "seat0";
/// The advertised `wl_seat` name of the agent seat.
pub const AGENT_SEAT_NAME: &str = "agent";

/// Which seat an input belongs to: the human's, or the Bus agent's.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SeatKind {
    Human,
    Agent,
}

impl SeatKind {
    /// The wire name (`seat` argument values, `input.seats.<key>`).
    pub fn name(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::Agent => "agent",
        }
    }

    /// The advertised `wl_seat` name.
    pub fn seat_name(self) -> &'static str {
        match self {
            Self::Human => HUMAN_SEAT_NAME,
            Self::Agent => AGENT_SEAT_NAME,
        }
    }
}

/// Stacking tier, lowest first.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum StackBand {
    Background,
    Bottom,
    #[default]
    Normal,
    Top,
    Overlay,
    /// Pointer-following client artwork, above desktop layers, never an
    /// input target.
    DragIcon,
    Lock,
}

impl StackBand {
    pub const COUNT: usize = 7;

    pub const fn index(self) -> usize {
        match self {
            Self::Background => 0,
            Self::Bottom => 1,
            Self::Normal => 2,
            Self::Top => 3,
            Self::Overlay => 4,
            Self::DragIcon => 5,
            Self::Lock => 6,
        }
    }

    /// The `surfaces.s<id>.band` / `windows.s<id>.band` leaf value.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Background => "background",
            Self::Bottom => "bottom",
            Self::Normal => "normal",
            Self::Top => "top",
            Self::Overlay => "overlay",
            Self::DragIcon => "drag-icon",
            Self::Lock => "lock",
        }
    }
}

/// The role a surface currently holds. `Dormant` is a surface that held a
/// role and lost it while the `wl_surface` lives on: it keeps its id, takes
/// a new generation, and reads as an unknown window.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SurfaceRole {
    Toplevel,
    Popup,
    /// An input method's candidate window: rendered, never a focus
    /// candidate, never a managed toplevel.
    ImePopup,
    Layer,
    Lock,
    Subsurface,
    DragIcon,
    /// An X11 window. Override-redirect windows are rendered but are not
    /// managed toplevels.
    X11 { override_redirect: bool },
    Dormant,
}

impl SurfaceRole {
    /// The `surfaces.s<id>.role` leaf value (the `comp.*` wire string).
    pub fn kind(self) -> &'static str {
        match self {
            Self::Toplevel => "toplevel",
            Self::Popup => "popup",
            Self::ImePopup => "ime-popup",
            Self::Layer => "layer",
            Self::Lock => "lock",
            Self::Subsurface => "subsurface",
            Self::DragIcon => "drag-icon",
            Self::Dormant => "dormant",
            Self::X11 {
                override_redirect: true,
            } => "x11-override-redirect",
            Self::X11 {
                override_redirect: false,
            } => "x11-toplevel",
        }
    }

    /// A window the compositor manages: focus candidate, minimisable,
    /// exported as a foreign toplevel. Override-redirect X11 is excluded.
    pub fn managed_toplevel(self) -> bool {
        match self {
            Self::Toplevel => true,
            Self::X11 { override_redirect } => !override_redirect,
            _ => false,
        }
    }

    /// Whether a role has a workspace of its own: a managed toplevel, or an
    /// override-redirect X11 window (it has no parent, so without one an
    /// open menu would outlive the switch that hid its owner). Everything
    /// else is on every workspace.
    pub fn carries_workspace(self) -> bool {
        matches!(
            self,
            Self::X11 {
                override_redirect: true
            }
        ) || self.managed_toplevel()
    }

    /// The roles the engine stamps a uuid v7 on: toplevels and X11 windows.
    pub fn carries_uuid(self) -> bool {
        matches!(self, Self::Toplevel | Self::X11 { .. })
    }
}

#[cfg(test)]
mod tests;

//! X11 window policy as pure logic.
//!
//! - [`decoration_mode`]: a managed X11 toplevel wears the compositor's
//!   chrome unless it is override-redirect, fullscreen, decoration is off,
//!   or the client refused window-manager decorations through Motif hints.
//! - [`motif_refuses_server_decorations`]: the Motif interpretation,
//!   pinned against smithay's `X11Surface::is_decorated()` by a testkit
//!   test, so an inversion in a smithay bump fails a test instead of
//!   double-decorating Firefox.
//! - [`XwaylandRetryPolicy`]: the single Xwayland retry credit.

/// Who draws an X11 window's decorations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecorationMode {
    /// The compositor's chrome.
    ServerSide,
    /// The client's own, or none.
    ClientSide,
}

/// The X11 decoration mode. `client_decorated` is smithay's
/// `X11Surface::is_decorated()` (the Motif refusal).
pub const fn decoration_mode(
    override_redirect: bool,
    decoration_enabled: bool,
    fullscreen: bool,
    client_decorated: bool,
) -> DecorationMode {
    if override_redirect || !decoration_enabled || fullscreen || client_decorated {
        DecorationMode::ClientSide
    } else {
        DecorationMode::ServerSide
    }
}

/// `_MOTIF_WM_HINTS`: with the decorations flag (bit 1) set and a zero
/// decorations field the client refuses window-manager decorations; flag
/// unset, or any non-zero decorations, accepts them.
pub const fn motif_refuses_server_decorations(flags: u32, decorations: u32) -> bool {
    const MWM_HINTS_DECORATIONS: u32 = 1 << 1;
    (flags & MWM_HINTS_DECORATIONS) != 0 && decorations == 0
}

/// `_NET_WM_DESKTOP` from a client (decision §8.6.2): the 0-based desktop
/// to move the window to, or `None` when the
/// request is refused — all desktops (sticky, `0xFFFFFFFF`) or an index past
/// the `count`. Pure so the sticky refusal is testable: no stock pager can
/// send it (wmctrl reads `-t -1` as "the current desktop" and sends that).
pub const fn desktop_request_target(desktop: u32, count: u32) -> Option<u32> {
    if desktop == u32::MAX || desktop >= count {
        None
    } else {
        Some(desktop)
    }
}

// ── Server lifecycle ──

/// The one restart waits this long after an
/// unexpected death. A bounded recovery delay, not an interval (no-poll law:
/// a one-shot backstop of at least a minute).
pub const XWAYLAND_RETRY_DELAY_SECS: u64 = 60;

/// What an X server death leads to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XwaylandRetryDecision {
    /// Spawn one new generation after [`XWAYLAND_RETRY_DELAY_SECS`].
    Retry,
    /// No credit left: X11 stays down until the compositor restarts.
    StayFailed,
}

/// The single retry credit. The first failure spends it; every later one
/// stays failed. The credit is never restored (decision §8: one retry, and
/// the second death stays dead).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct XwaylandRetryPolicy {
    credit: bool,
}

impl Default for XwaylandRetryPolicy {
    fn default() -> Self {
        Self::new()
    }
}

impl XwaylandRetryPolicy {
    /// With its one credit (const, so a host can hold it in a static).
    pub const fn new() -> Self {
        Self { credit: true }
    }

    pub fn on_failure(&mut self) -> XwaylandRetryDecision {
        if std::mem::take(&mut self.credit) {
            XwaylandRetryDecision::Retry
        } else {
            XwaylandRetryDecision::StayFailed
        }
    }

    pub fn has_credit(&self) -> bool {
        self.credit
    }
}

// ── Placement ──

/// The cascade: the first managed window sits this far in from the usable
/// corner, each next one this much further (six steps, then round again).
pub const CASCADE_ORIGIN: f32 = 36.0;
pub const CASCADE_STEP: f32 = 48.0;
pub const CASCADE_SLOTS: u32 = 6;
/// The gap a default-sized window keeps from the usable edge.
pub const OUTPUT_MARGIN: f32 = 24.0;
/// The default size floor and its share of the output.
pub const DEFAULT_TOPLEVEL_WIDTH: i32 = 640;
pub const DEFAULT_TOPLEVEL_HEIGHT: i32 = 420;
pub const DEFAULT_TOPLEVEL_OUTPUT_SHARE: f32 = 0.72;

/// A logical rectangle (the usable area).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Area {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// The server-side chrome's thickness around the content (logical px).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Extents {
    pub top: f32,
    pub left: f32,
    pub right: f32,
    pub bottom: f32,
}

/// What an initial X11 placement reads.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InitialPlacement {
    pub usable: Area,
    /// The client's own origin (`X11Surface::last_configure().loc`, the
    /// CreateWindow position; `geometry().loc` is only the frame extents); (0, 0) is
    /// every X client's default and reads as "no preference".
    pub requested_origin: (i32, i32),
    pub requested_size: (i32, i32),
    pub min_hint: Option<(i32, i32)>,
    pub max_hint: Option<(i32, i32)>,
    /// `Some` when the window wears the compositor's chrome.
    pub extents: Option<Extents>,
    /// The cascade slot this window takes (the host counts placements).
    pub cascade_index: u32,
}

/// A sensible toplevel size: the output's share, floored at the default
/// size, capped at what fits from `(x, y)` with the margin.
pub fn sensible_size(usable: Area, x: f32, y: f32) -> (i32, i32) {
    let max_width = (usable.x + usable.width - x - OUTPUT_MARGIN).max(240.0) as i32;
    let max_height = (usable.y + usable.height - y - OUTPUT_MARGIN).max(160.0) as i32;
    let share_width = (usable.width * DEFAULT_TOPLEVEL_OUTPUT_SHARE) as i32;
    let share_height = (usable.height * DEFAULT_TOPLEVEL_OUTPUT_SHARE) as i32;
    (
        share_width.max(DEFAULT_TOPLEVEL_WIDTH).min(max_width),
        share_height.max(DEFAULT_TOPLEVEL_HEIGHT).min(max_height),
    )
}

/// Clamp the content size: per axis, a degenerate request (≤ 1) takes
/// the fallback; then WM_NORMAL_HINTS min/max, with the usable room a hard
/// ceiling even over the max hint, and nothing below 1.
pub fn clamp_content_size(
    requested: (i32, i32),
    min_hint: Option<(i32, i32)>,
    max_hint: Option<(i32, i32)>,
    fallback: (i32, i32),
    usable: (i32, i32),
) -> (i32, i32) {
    let requested = (
        if requested.0 > 1 { requested.0 } else { fallback.0 },
        if requested.1 > 1 { requested.1 } else { fallback.1 },
    );
    let min = min_hint.unwrap_or((1, 1));
    let max = max_hint.unwrap_or((i32::MAX, i32::MAX));
    let upper = (
        usable.0.max(1).min(max.0.max(1)),
        usable.1.max(1).min(max.1.max(1)),
    );
    (
        requested.0.clamp(min.0.max(1).min(upper.0), upper.0),
        requested.1.clamp(min.1.max(1).min(upper.1), upper.1),
    )
}

/// Clamp a normal restore: the content fits the usable area (less the
/// chrome), and the OUTER frame (content plus chrome) is moved inside it.
/// Returns the content origin and size.
pub fn clamp_normal(usable: Area, origin: (f32, f32), size: (i32, i32), extents: Option<Extents>) -> ((i32, i32), (i32, i32)) {
    let chrome = extents.unwrap_or_default();
    let available = (
        (usable.width - chrome.left - chrome.right).floor().max(1.0) as i32,
        (usable.height - chrome.top - chrome.bottom).floor().max(1.0) as i32,
    );
    let size = (size.0.min(available.0).max(1), size.1.min(available.1).max(1));
    let outer_size = (
        size.0 as f32 + chrome.left + chrome.right,
        size.1 as f32 + chrome.top + chrome.bottom,
    );
    let max_x = (usable.x + usable.width - outer_size.0).max(usable.x);
    let max_y = (usable.y + usable.height - outer_size.1).max(usable.y);
    let outer = (
        (origin.0 - chrome.left).clamp(usable.x, max_x),
        (origin.1 - chrome.top).clamp(usable.y, max_y),
    );
    (((outer.0 + chrome.left) as i32, (outer.1 + chrome.top) as i32), size)
}

/// The initial geometry: the client's own origin when it
/// stated one inside the usable area, else the cascade slot; with chrome the
/// origin is pushed in by the extents; the size is the request clamped by
/// hints and the room left, with the sensible size as fallback; finally the
/// outer frame is kept inside the usable area.
pub fn initial_geometry(placement: InitialPlacement) -> InitialGeometry {
    let usable = placement.usable;
    let chrome = placement.extents.unwrap_or_default();
    let (rx, ry) = (placement.requested_origin.0 as f32, placement.requested_origin.1 as f32);
    let requested_inside = placement.requested_origin != (0, 0)
        && rx >= usable.x
        && ry >= usable.y
        && rx < usable.x + usable.width
        && ry < usable.y + usable.height;
    let (mut x, mut y) = if requested_inside {
        (rx, ry)
    } else {
        let slot = (placement.cascade_index % CASCADE_SLOTS) as f32;
        (
            usable.x + CASCADE_ORIGIN + slot * CASCADE_STEP,
            usable.y + CASCADE_ORIGIN + slot * CASCADE_STEP,
        )
    };
    if placement.extents.is_some() {
        x = x.max(usable.x + chrome.left);
        y = y.max(usable.y + chrome.top);
    }
    let fallback = sensible_size(usable, x, y);
    let room = (
        (usable.x + usable.width - x - OUTPUT_MARGIN - chrome.right).max(1.0) as i32,
        (usable.y + usable.height - y - OUTPUT_MARGIN - chrome.bottom).max(1.0) as i32,
    );
    let size = clamp_content_size(placement.requested_size, placement.min_hint, placement.max_hint, fallback, room);
    let (origin, size) = clamp_normal(usable, (x, y), size, placement.extents);
    InitialGeometry { origin, size, cascaded: !requested_inside }
}

/// [`initial_geometry`]'s answer: the content origin and size (host Space,
/// logical), and whether the cascade slot was used (the host then advances
/// its counter).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InitialGeometry {
    pub origin: (i32, i32),
    pub size: (i32, i32),
    pub cascaded: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Motif decoration interpretation is pinned.
    #[test]
    fn the_motif_interpretation_is_pinned() {
        assert!(!motif_refuses_server_decorations(0, 0));
        assert!(!motif_refuses_server_decorations(0, 1));
        assert!(motif_refuses_server_decorations(1 << 1, 0));
        assert!(!motif_refuses_server_decorations(1 << 1, 1));
        assert!(!motif_refuses_server_decorations(1 << 1, u32::MAX));
        assert!(!motif_refuses_server_decorations(1 << 0, 0));
    }

    /// One credit; the second failure stays.
    #[test]
    fn one_retry_then_the_server_stays_down() {
        let mut policy = XwaylandRetryPolicy::default();
        assert!(policy.has_credit());
        assert_eq!(policy.on_failure(), XwaylandRetryDecision::Retry);
        assert!(!policy.has_credit());
        assert_eq!(policy.on_failure(), XwaylandRetryDecision::StayFailed);
        assert_eq!(policy.on_failure(), XwaylandRetryDecision::StayFailed);
    }

    /// Sticky and past-the-count desktop requests are refused.
    #[test]
    fn a_desktop_request_refuses_sticky_and_past_the_count() {
        assert_eq!(desktop_request_target(0, 4), Some(0));
        assert_eq!(desktop_request_target(3, 4), Some(3));
        assert_eq!(desktop_request_target(4, 4), None, "past the count");
        assert_eq!(desktop_request_target(u32::MAX, 4), None, "sticky");
        assert_eq!(desktop_request_target(u32::MAX, u32::MAX), None, "sticky even with no ceiling");
        assert_eq!(desktop_request_target(0, 0), None, "no desktops");
    }

    /// The table: SSD only for a managed, non-fullscreen window that did
    /// not refuse, while decoration is on.
    #[test]
    fn server_side_only_when_nothing_refuses() {
        assert_eq!(decoration_mode(false, true, false, false), DecorationMode::ServerSide);
        assert_eq!(decoration_mode(true, true, false, false), DecorationMode::ClientSide, "override-redirect");
        assert_eq!(decoration_mode(false, false, false, false), DecorationMode::ClientSide, "decoration off");
        assert_eq!(decoration_mode(false, true, true, false), DecorationMode::ClientSide, "fullscreen");
        assert_eq!(decoration_mode(false, true, false, true), DecorationMode::ClientSide, "Motif refusal");
    }

    /// The content size clamp honours hints, output and fallback.
    #[test]
    fn the_content_size_clamp_honours_hints_output_and_fallback() {
        let fallback = (640, 420);
        let usable = (1000, 800);
        assert_eq!(clamp_content_size((300, 200), None, None, fallback, usable), (300, 200));
        assert_eq!(clamp_content_size((0, 0), None, None, fallback, usable), fallback);
        assert_eq!(clamp_content_size((1, 1), None, None, fallback, usable), fallback);
        assert_eq!(clamp_content_size((1000, 1), None, None, fallback, usable), (1000, fallback.1));
        assert_eq!(clamp_content_size((0, 500), None, None, fallback, usable), (fallback.0, 500));
        assert_eq!(clamp_content_size((300, 200), Some((400, 300)), None, fallback, usable), (400, 300));
        assert_eq!(clamp_content_size((900, 700), None, Some((500, 400)), fallback, usable), (500, 400));
        assert_eq!(clamp_content_size((5000, 5000), None, Some((4000, 4000)), fallback, usable), usable);
        assert_eq!(clamp_content_size((300, 200), Some((0, 0)), Some((0, 0)), fallback, usable), (1, 1));
    }

    fn placement(origin: (i32, i32), size: (i32, i32), extents: Option<Extents>, cascade_index: u32) -> InitialPlacement {
        InitialPlacement {
            usable: Area { x: 0.0, y: 30.0, width: 1280.0, height: 770.0 },
            requested_origin: origin,
            requested_size: size,
            min_hint: None,
            max_hint: None,
            extents,
            cascade_index,
        }
    }

    const SSD: Extents = Extents { top: 32.0, left: 1.0, right: 1.0, bottom: 1.0 };

    /// A stated origin inside the usable area is honoured at initial
    /// placement (`xmessage -center`); (0, 0) and an outside origin cascade.
    #[test]
    fn a_stated_origin_inside_is_honoured_else_the_cascade() {
        assert_eq!(initial_geometry(placement((400, 300), (200, 100), None, 0)).origin, (400, 300));
        assert_eq!(initial_geometry(placement((0, 0), (200, 100), None, 0)).origin, (36, 66));
        assert_eq!(initial_geometry(placement((0, 0), (200, 100), None, 1)).origin, (84, 114));
        assert_eq!(initial_geometry(placement((5000, 5000), (200, 100), None, 2)).origin, (132, 162));
        assert_eq!(initial_geometry(placement((0, 0), (200, 100), None, 6)).origin, (36, 66), "six slots, then round again");
    }

    /// With chrome the content origin is pushed in so the frame starts
    /// inside, and the whole frame (content plus chrome) stays inside.
    #[test]
    fn chrome_keeps_the_whole_frame_inside_the_usable_area() {
        let InitialGeometry { origin: (x, y), size: (w, h), cascaded } = initial_geometry(placement((1, 31), (200, 100), Some(SSD), 0));
        assert!(!cascaded, "a stated origin inside is not a cascade");
        assert!(x as f32 - SSD.left >= 0.0 && y as f32 - SSD.top >= 30.0, "origin ({x}, {y})");
        let InitialGeometry { origin: (x, y), size: (w2, h2), .. } = initial_geometry(placement((1200, 700), (400, 300), Some(SSD), 0));
        assert!(x as f32 + w2 as f32 + SSD.right <= 1280.0, "right edge {x}+{w2}");
        assert!(y as f32 + h2 as f32 + SSD.bottom <= 800.0, "bottom edge {y}+{h2}");
        assert_eq!((w, h), (200, 100));
    }

    /// No usable request: the sensible size, the output's share floored at
    /// 640x420 and capped by the room from the origin.
    #[test]
    fn no_request_takes_the_sensible_size() {
        let InitialGeometry { size, cascaded, .. } = initial_geometry(placement((0, 0), (0, 0), None, 0));
        assert!(cascaded);
        assert_eq!(size, sensible_size(Area { x: 0.0, y: 30.0, width: 1280.0, height: 770.0 }, 36.0, 66.0));
        assert!(size.0 >= 640 && size.1 >= 420);
    }
}

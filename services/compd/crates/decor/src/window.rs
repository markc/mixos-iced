//! Which windows get chrome, and how much room it takes.
//!
//! The installed theme is per compositor thread, like every other piece of
//! compd frame state: the backend installs it once at startup
//! ([`install`]); a later reinstall (a theme change over the Bus) bumps the
//! generation, which repaints every chrome once.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::layout::{ChromePart, DecoExtents, Vec2};
use smithay::desktop::Window;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::utils::{Logical, Rectangle, Size};

use crate::theme::ChromeTheme;

thread_local! {
    static THEME: RefCell<Option<Rc<ChromeTheme>>> = const { RefCell::new(None) };
    static GENERATION: Cell<u64> = const { Cell::new(0) };
}

/// Install `theme` for this compositor thread (startup, or a theme change).
pub fn install(theme: ChromeTheme) {
    THEME.with_borrow_mut(|slot| *slot = Some(Rc::new(theme)));
    GENERATION.set(GENERATION.get().wrapping_add(1));
}

/// The installed theme, if any (none: no chrome is drawn).
pub fn installed() -> Option<Rc<ChromeTheme>> {
    THEME.with_borrow(Clone::clone)
}

/// Moves on every [`install`]: chrome state carries it, so a new theme
/// repaints every window's chrome once.
pub fn generation() -> u64 {
    GENERATION.get()
}

/// Whether server-side decorations are enabled (the `decorations_ssd`
/// preference, as the xdg-decoration handlers read it).
pub fn ssd_enabled() -> bool {
    model::environment::preference::base::decorations_ssd()
}

/// Whether compd draws `window`'s chrome, with a theme installed:
/// - an xdg toplevel that acked server-side decorations and still has its
///   decoration object, not fullscreen (fullscreen owns its whole output);
/// - an X11 window by `policy::x11::decoration_mode`: not
///   override-redirect, decorations on, not fullscreen, and no Motif
///   refusal. Read live, so a MotifHints change (property_notify schedules
///   the frame) takes effect at the next frame with no stored mode.
pub fn decorated(window: &Window) -> bool {
    if let Some(x11) = window.x11_surface() {
        return installed().is_some()
            && policy::x11::decoration_mode(
                x11.is_override_redirect(),
                ssd_enabled(),
                x11.is_fullscreen(),
                x11.is_decorated(),
            ) == policy::x11::DecorationMode::ServerSide;
    }
    let Some(toplevel) = window.toplevel() else {
        return false;
    };
    installed().is_some()
        && dispatcher::wayland::xdg::decoration::mode::server_side(toplevel)
        && !toplevel.with_committed_state(|state| {
            state.is_some_and(|state| state.states.contains(xdg_toplevel::State::Fullscreen))
        })
}

/// The room `window`'s chrome takes around its content (logical px), when it
/// is [`decorated`].
pub fn extents(window: &Window) -> Option<DecoExtents> {
    if !decorated(window) {
        return None;
    }
    installed().map(|theme| DecoExtents::of(&theme.deco))
}

/// How far `window`'s chrome reaches outside its content on any side (logical
/// px; 0 without chrome): what a cull of the content's slot must grow by so it
/// never drops a window whose titlebar still shows. The shadow is not counted:
/// losing it under an occluder costs nothing visible.
pub fn margin(window: &Window) -> f64 {
    extents(window).map_or(0.0, |e| e.top.max(e.left).max(e.right).max(e.bottom) as f64)
}

/// What a point over a window's chrome hits: the part, and whether it is over
/// the caption-button cluster (mac shows its glyphs on cluster hover).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChromeHit {
    pub part: ChromePart,
    pub in_cluster: bool,
}

/// The chrome part of `window` under a point `content_relative` to its slot's
/// top-left (logical px), for a slot of `slot_size`. `None` without chrome,
/// over the content, or outside the chrome and its resize band: those are not
/// the chrome's to answer.
pub fn hit(
    window: &Window,
    content_relative: (f64, f64),
    slot_size: Size<i32, Logical>,
) -> Option<ChromeHit> {
    let extents = extents(window)?;
    let theme = installed()?;
    let layout = crate::layout::ChromeLayout::compute(
        &theme.deco,
        crate::layout::vec2(slot_size.w.max(0) as f32, slot_size.h.max(0) as f32),
    );
    let p = frame_point(extents, content_relative);
    match layout.hit_test(p) {
        ChromePart::Content | ChromePart::Outside => None,
        part => Some(ChromeHit {
            part,
            in_cluster: layout.button_cluster.contains(p),
        }),
    }
}

/// Where `window`'s chrome parts are in its Space (logical px), for a slot at
/// `slot_origin` of `slot_size`: what a gate needs to aim injected input at a
/// titlebar or a caption button without assuming any style's layout.
#[derive(Clone, Debug, PartialEq)]
pub struct SpaceParts {
    /// The whole frame: what the content is cut to, rounded by the radius.
    pub frame: Rectangle<f64, Logical>,
    pub titlebar: Rectangle<f64, Logical>,
    pub buttons: Vec<(crate::layout::CaptionButton, Rectangle<f64, Logical>)>,
}

pub fn parts(
    window: &Window,
    slot_origin: smithay::utils::Point<i32, Logical>,
    slot_size: Size<i32, Logical>,
) -> Option<SpaceParts> {
    let extents = extents(window)?;
    let theme = installed()?;
    let layout = crate::layout::ChromeLayout::compute(
        &theme.deco,
        crate::layout::vec2(slot_size.w.max(0) as f32, slot_size.h.max(0) as f32),
    );
    let (ox, oy) = (
        slot_origin.x as f64 - extents.left as f64,
        slot_origin.y as f64 - extents.top as f64,
    );
    let place = |r: crate::layout::Rect| -> Rectangle<f64, Logical> {
        Rectangle::new(
            (ox + r.x as f64, oy + r.y as f64).into(),
            (r.w as f64, r.h as f64).into(),
        )
    };
    Some(SpaceParts {
        frame: place(layout.window),
        titlebar: place(layout.titlebar),
        buttons: layout
            .buttons
            .iter()
            .map(|&(button, rect)| (button, place(rect)))
            .collect(),
    })
}

/// The content rectangle that leaves room for `window`'s chrome inside
/// `outer` (an output's usable area, for a maximised window). `outer` itself
/// when the window has no chrome.
pub fn content_area(window: &Window, outer: Rectangle<i32, Logical>) -> Rectangle<i32, Logical> {
    match extents(window) {
        Some(extents) => inset(outer, extents),
        None => outer,
    }
}

/// `outer` less `extents`, rounded outward so the chrome always fits.
pub fn inset(outer: Rectangle<i32, Logical>, e: DecoExtents) -> Rectangle<i32, Logical> {
    let (left, top) = (e.left.ceil() as i32, e.top.ceil() as i32);
    let (right, bottom) = (e.right.ceil() as i32, e.bottom.ceil() as i32);
    Rectangle::new(
        (outer.loc.x + left, outer.loc.y + top).into(),
        (
            (outer.size.w - left - right).max(1),
            (outer.size.h - top - bottom).max(1),
        )
            .into(),
    )
}

/// The window-local frame point for a point relative to the content's
/// top-left: the chrome's layout space has the frame's corner at 0,0.
pub fn frame_point(extents: DecoExtents, content_relative: (f64, f64)) -> Vec2 {
    crate::layout::vec2(
        content_relative.0 as f32 + extents.left,
        content_relative.1 as f32 + extents.top,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{ChromeLayout, ChromeStyle, Mode, Scheme, presets, vec2};

    #[test]
    fn a_maximised_content_area_leaves_the_chrome_on_the_output() {
        for style in ChromeStyle::ALL {
            let theme = presets::resolve(style, Scheme::Ocean, Mode::Light);
            let e = DecoExtents::of(&theme);
            let outer = Rectangle::new((0, 30).into(), (1920, 1050).into());
            let content = inset(outer, e);
            assert_eq!(
                content.loc.y - outer.loc.y,
                e.top.ceil() as i32,
                "{style:?}"
            );
            // The frame around the content fits inside the usable area.
            let layout =
                ChromeLayout::compute(&theme, vec2(content.size.w as f32, content.size.h as f32));
            assert!(layout.window.w <= outer.size.w as f32 + 0.5, "{style:?}");
            assert!(layout.window.h <= outer.size.h as f32 + 0.5, "{style:?}");
        }
    }

    #[test]
    fn content_points_map_into_the_frame_by_the_extents() {
        let theme = presets::resolve(ChromeStyle::Win11, Scheme::Ocean, Mode::Light);
        let e = DecoExtents::of(&theme);
        let layout = ChromeLayout::compute(&theme, vec2(400.0, 300.0));
        let origin = frame_point(e, (0.0, 0.0));
        assert_eq!(origin, layout.content_offset());
        // Just above the content's top edge is the titlebar.
        let above = frame_point(e, (200.0, -2.0));
        assert_eq!(
            layout.hit_test(above),
            crate::layout::ChromePart::TitlebarDrag
        );
    }

    #[test]
    fn install_bumps_the_generation() {
        let before = generation();
        install(ChromeTheme::from_source(ChromeStyle::Mac, None));
        assert_ne!(generation(), before);
        assert!(installed().is_some());
    }
}

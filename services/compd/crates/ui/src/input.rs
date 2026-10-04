//! Helpers for translating smithay input events to `iced_core::Event`.
//!
//! Optional. The `IcedRegistry::dispatch_event` API takes `iced_core::Event`
//! directly, so the compositor can perform translation in any way it likes.
//! These helpers are here for the common cases.
//!
//! Lifted from the layer-shell reference (`input.rs` in the original
//! integration), with smithay input types substituted for sctk's. The
//! Linux input-event-codes and xkb keysym mappings are identical.

use iced_core::{
    Event as IcedEvent, Point as IcedPoint,
    keyboard::{self, Key, Location, Modifiers as IcedMods, key::Named},
    mouse::{self, Button, ScrollDelta},
};

// We don't depend on `smithay::input::*` types directly to keep this crate's
// surface narrow — the compositor passes us already-extracted primitive
// fields. If you want a one-call API that takes a full `PointerMotionEvent`,
// add a thin wrapper in your compositor binary that calls these.

/// Build an Iced `CursorMoved` event from a surface-local logical point.
pub fn cursor_moved(local: IcedPoint) -> IcedEvent {
    IcedEvent::Mouse(mouse::Event::CursorMoved { position: local })
}

pub fn cursor_entered(local: IcedPoint) -> Vec<IcedEvent> {
    vec![
        IcedEvent::Mouse(mouse::Event::CursorEntered),
        IcedEvent::Mouse(mouse::Event::CursorMoved { position: local }),
    ]
}

pub fn cursor_left() -> IcedEvent {
    IcedEvent::Mouse(mouse::Event::CursorLeft)
}

pub fn button_pressed(linux_code: u32) -> Option<IcedEvent> {
    translate_button(linux_code).map(|b| IcedEvent::Mouse(mouse::Event::ButtonPressed(b)))
}

pub fn button_released(linux_code: u32) -> Option<IcedEvent> {
    translate_button(linux_code).map(|b| IcedEvent::Mouse(mouse::Event::ButtonReleased(b)))
}

/// `discrete_x/y` are tick counts (mouse wheel), `pixel_x/y` are continuous
/// pixels (touchpad). Prefer discrete when present.
pub fn wheel_scrolled(
    discrete_x: i32,
    discrete_y: i32,
    pixel_x: f64,
    pixel_y: f64,
) -> Option<IcedEvent> {
    let delta = translate_axis(discrete_x, discrete_y, pixel_x, pixel_y)?;
    Some(IcedEvent::Mouse(mouse::Event::WheelScrolled { delta }))
}

// ── Button table ──────────────────────────────────────────────────────

/// Linux input-event-codes → iced mouse buttons. From `<linux/input-event-codes.h>`.
fn translate_button(linux_code: u32) -> Option<Button> {
    match linux_code {
        0x110 => Some(Button::Left),    // BTN_LEFT
        0x111 => Some(Button::Right),   // BTN_RIGHT
        0x112 => Some(Button::Middle),  // BTN_MIDDLE
        0x113 => Some(Button::Back),    // BTN_SIDE
        0x114 => Some(Button::Forward), // BTN_EXTRA
        other => Some(Button::Other(other as u16)),
    }
}

// ── Axis ──────────────────────────────────────────────────────────────

fn translate_axis(dx: i32, dy: i32, px: f64, py: f64) -> Option<ScrollDelta> {
    if dx != 0 || dy != 0 {
        // Wayland discrete is reversed sign vs iced; observed empirically
        // in the reference integration.
        Some(ScrollDelta::Lines {
            x: -dx as f32,
            y: -dy as f32,
        })
    } else if px != 0.0 || py != 0.0 {
        Some(ScrollDelta::Pixels {
            x: -px as f32,
            y: -py as f32,
        })
    } else {
        None
    }
}

// ── Keyboard ──────────────────────────────────────────────────────────

/// Build an Iced KeyPressed or KeyReleased from xkb info.
///
/// `keysym_raw` is the raw u32 xkb keysym. `utf8` is the pre-composed UTF-8
/// text for the press (None for releases / non-printable). `pressed` selects
/// press vs release. `is_repeat` is true for held-key autorepeats.
pub fn keyboard_event(
    keysym_raw: u32,
    utf8: Option<&str>,
    pressed: bool,
    is_repeat: bool,
    modifiers: IcedMods,
) -> Option<IcedEvent> {
    let key = keysym_to_iced_key(keysym_raw, utf8)?;
    // let modifiers = mods.to_iced();

    let text = if pressed {
        utf8.and_then(|s| {
            if s.is_empty() {
                None
            } else {
                Some(s.into())
            }
        })
    } else {
        None
    };

    let logical_key = key.clone();
    let physical_key =
        keyboard::key::Physical::Unidentified(keyboard::key::NativeCode::Xkb(keysym_raw));

    Some(if pressed {
        IcedEvent::Keyboard(keyboard::Event::KeyPressed {
            key,
            modified_key: logical_key.clone(),
            physical_key,
            location: Location::Standard,
            modifiers,
            text,
            repeat: is_repeat,
        })
    } else {
        IcedEvent::Keyboard(keyboard::Event::KeyReleased {
            key,
            modified_key: logical_key,
            physical_key,
            location: Location::Standard,
            modifiers,
        })
    })
}

/// Modifier state, in a form independent of any specific keyboard library.
#[derive(Debug, Clone, Copy, Default)]
pub struct KeyboardModifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub logo: bool,
}

impl KeyboardModifiers {
    pub fn to_iced(self) -> IcedMods {
        let mut out = IcedMods::empty();
        if self.shift {
            out |= IcedMods::SHIFT;
        }
        if self.ctrl {
            out |= IcedMods::CTRL;
        }
        if self.alt {
            out |= IcedMods::ALT;
        }
        if self.logo {
            out |= IcedMods::LOGO;
        }
        out
    }
}

/// xkb keysym → iced Key.
///
/// Covers the named keys most UIs care about; printable chars fall through
/// to `Key::Character` via the supplied `utf8` text. xkb keysym constants
/// are stable: <https://www.x.org/releases/X11R7.7/doc/xproto/x11protocol.html>
fn keysym_to_iced_key(raw: u32, utf8: Option<&str>) -> Option<Key> {
    // xkb keysym constants (subset).
    const XK_RETURN: u32 = 0xff0d;
    const XK_KP_ENTER: u32 = 0xff8d;
    const XK_ESCAPE: u32 = 0xff1b;
    const XK_BACKSPACE: u32 = 0xff08;
    const XK_TAB: u32 = 0xff09;
    const XK_DELETE: u32 = 0xffff;
    const XK_INSERT: u32 = 0xff63;
    const XK_HOME: u32 = 0xff50;
    const XK_END: u32 = 0xff57;
    const XK_PAGE_UP: u32 = 0xff55;
    const XK_PAGE_DOWN: u32 = 0xff56;
    const XK_UP: u32 = 0xff52;
    const XK_DOWN: u32 = 0xff54;
    const XK_LEFT: u32 = 0xff51;
    const XK_RIGHT: u32 = 0xff53;
    const XK_SHIFT_L: u32 = 0xffe1;
    const XK_SHIFT_R: u32 = 0xffe2;
    const XK_CONTROL_L: u32 = 0xffe3;
    const XK_CONTROL_R: u32 = 0xffe4;
    const XK_ALT_L: u32 = 0xffe9;
    const XK_ALT_R: u32 = 0xffea;
    const XK_SUPER_L: u32 = 0xffeb;
    const XK_SUPER_R: u32 = 0xffec;
    const XK_F1: u32 = 0xffbe;
    const XK_F12: u32 = 0xffc9;
    const XK_SPACE: u32 = 0x0020;

    let named = match raw {
        XK_RETURN | XK_KP_ENTER => Named::Enter,
        XK_ESCAPE => Named::Escape,
        XK_BACKSPACE => Named::Backspace,
        XK_TAB => Named::Tab,
        XK_DELETE => Named::Delete,
        XK_INSERT => Named::Insert,
        XK_HOME => Named::Home,
        XK_END => Named::End,
        XK_PAGE_UP => Named::PageUp,
        XK_PAGE_DOWN => Named::PageDown,
        XK_UP => Named::ArrowUp,
        XK_DOWN => Named::ArrowDown,
        XK_LEFT => Named::ArrowLeft,
        XK_RIGHT => Named::ArrowRight,
        XK_SHIFT_L | XK_SHIFT_R => Named::Shift,
        XK_CONTROL_L | XK_CONTROL_R => Named::Control,
        XK_ALT_L | XK_ALT_R => Named::Alt,
        XK_SUPER_L | XK_SUPER_R => Named::Super,
        XK_SPACE => return Some(Key::Character(" ".into())),
        n if (XK_F1..=XK_F12).contains(&n) => match n - XK_F1 {
            0 => Named::F1,
            1 => Named::F2,
            2 => Named::F3,
            3 => Named::F4,
            4 => Named::F5,
            5 => Named::F6,
            6 => Named::F7,
            7 => Named::F8,
            8 => Named::F9,
            9 => Named::F10,
            10 => Named::F11,
            11 => Named::F12,
            _ => return None,
        },
        _ => {
            let text = utf8?;
            if text.is_empty() || text.chars().all(|c| c.is_control()) {
                return None;
            }
            return Some(Key::Character(text.into()));
        }
    };
    Some(Key::Named(named))
}

pub fn keysym_to_iced_modifier(keysym_raw: u32) -> Option<IcedMods> {
    const XK_SHIFT_L: u32 = 0xffe1;
    const XK_SHIFT_R: u32 = 0xffe2;
    const XK_CONTROL_L: u32 = 0xffe3;
    const XK_CONTROL_R: u32 = 0xffe4;
    const XK_ALT_L: u32 = 0xffe9;
    const XK_ALT_R: u32 = 0xffea;
    const XK_SUPER_L: u32 = 0xffeb;
    const XK_SUPER_R: u32 = 0xffec;
    match keysym_raw {
        XK_SHIFT_L | XK_SHIFT_R => Some(IcedMods::SHIFT),
        XK_CONTROL_L | XK_CONTROL_R => Some(IcedMods::CTRL),
        XK_ALT_L | XK_ALT_R => Some(IcedMods::ALT),
        XK_SUPER_L | XK_SUPER_R => Some(IcedMods::LOGO),
        _ => None,
    }
}
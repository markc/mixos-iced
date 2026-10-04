//! Runtime parse/format of a `KeyCombo` to/from a human string like
//! `"Super+Shift+K"` (used by keybinding.json + the settings Keys tab).
//! Names match the `Key` enum variant (Debug) names exactly.
use crate::library::input::keyboard::combo::KeyCombo;
use crate::library::input::keyboard::enum_::Key;
use smithay::input::keyboard::ModifiersState;

/// (variant name, key) pairs for parsing. Names equal the `Debug` names.
const PAIRS: &[(&str, Key)] = &[
    ("A", Key::A), ("B", Key::B), ("C", Key::C), ("D", Key::D), ("E", Key::E), ("F", Key::F), ("G", Key::G), ("H", Key::H), ("I", Key::I), ("J", Key::J), ("K", Key::K), ("L", Key::L), ("M", Key::M), ("N", Key::N), ("O", Key::O), ("P", Key::P), ("Q", Key::Q), ("R", Key::R), ("S", Key::S), ("T", Key::T), ("U", Key::U), ("V", Key::V), ("W", Key::W), ("X", Key::X), ("Y", Key::Y), ("Z", Key::Z), 
    ("Num0", Key::Num0), ("Num1", Key::Num1), ("Num2", Key::Num2), ("Num3", Key::Num3), ("Num4", Key::Num4), ("Num5", Key::Num5), ("Num6", Key::Num6), ("Num7", Key::Num7), ("Num8", Key::Num8), ("Num9", Key::Num9), 
    ("F1", Key::F1), ("F2", Key::F2), ("F3", Key::F3), ("F4", Key::F4), ("F5", Key::F5), ("F6", Key::F6), ("F7", Key::F7), ("F8", Key::F8), ("F9", Key::F9), ("F10", Key::F10), ("F11", Key::F11), ("F12", Key::F12), ("F13", Key::F13), ("F14", Key::F14), ("F15", Key::F15), ("F16", Key::F16), ("F17", Key::F17), ("F18", Key::F18), ("F19", Key::F19), ("F20", Key::F20), ("F21", Key::F21), ("F22", Key::F22), ("F23", Key::F23), ("F24", Key::F24), 
    ("Kp0", Key::Kp0), ("Kp1", Key::Kp1), ("Kp2", Key::Kp2), ("Kp3", Key::Kp3), ("Kp4", Key::Kp4), ("Kp5", Key::Kp5), ("Kp6", Key::Kp6), ("Kp7", Key::Kp7), ("Kp8", Key::Kp8), ("Kp9", Key::Kp9), 
    ("SwitchVt1", Key::SwitchVt1), ("SwitchVt2", Key::SwitchVt2), ("SwitchVt3", Key::SwitchVt3), ("SwitchVt4", Key::SwitchVt4), ("SwitchVt5", Key::SwitchVt5), ("SwitchVt6", Key::SwitchVt6), ("SwitchVt7", Key::SwitchVt7), ("SwitchVt8", Key::SwitchVt8), ("SwitchVt9", Key::SwitchVt9), ("SwitchVt10", Key::SwitchVt10), ("SwitchVt11", Key::SwitchVt11), ("SwitchVt12", Key::SwitchVt12), 
    ("Up", Key::Up), ("Down", Key::Down), ("Left", Key::Left), ("Right", Key::Right), ("Super", Key::Super), ("Shift", Key::Shift), ("Ctrl", Key::Ctrl), ("Alt", Key::Alt), ("Return", Key::Return), ("Space", Key::Space), ("Tab", Key::Tab), ("Escape", Key::Escape), ("Backspace", Key::Backspace), ("Delete", Key::Delete), ("Insert", Key::Insert), ("Home", Key::Home), ("End", Key::End), ("PageUp", Key::PageUp), ("PageDown", Key::PageDown), ("PrintScreen", Key::PrintScreen), ("ScrollLock", Key::ScrollLock), ("Pause", Key::Pause), ("CapsLock", Key::CapsLock), ("NumLock", Key::NumLock), ("Grave", Key::Grave), ("Minus", Key::Minus), ("Equal", Key::Equal), ("BracketLeft", Key::BracketLeft), ("BracketRight", Key::BracketRight), ("BackSlash", Key::BackSlash), ("Semicolon", Key::Semicolon), ("Apostrophe", Key::Apostrophe), ("Comma", Key::Comma), ("Period", Key::Period), ("Slash", Key::Slash), ("KpAdd", Key::KpAdd), ("KpSubtract", Key::KpSubtract), ("KpMultiply", Key::KpMultiply), ("KpDivide", Key::KpDivide), ("KpEnter", Key::KpEnter), ("KpDecimal", Key::KpDecimal), ("AudioPlay", Key::AudioPlay), ("AudioPause", Key::AudioPause), ("AudioStop", Key::AudioStop), ("AudioNext", Key::AudioNext), ("AudioPrev", Key::AudioPrev), ("AudioMute", Key::AudioMute), ("AudioRaiseVolume", Key::AudioRaiseVolume), ("AudioLowerVolume", Key::AudioLowerVolume), ("AudioMicMute", Key::AudioMicMute), ("MonBrightnessUp", Key::MonBrightnessUp), ("MonBrightnessDown", Key::MonBrightnessDown), 
];

/// Parse a single key name (the `Key` Debug name) into a `Key`.
pub fn parse_key(s: &str) -> Option<Key> {
    PAIRS.iter().find(|(n, _)| *n == s).map(|(_, k)| *k)
}

/// Format a combo as `Super+Ctrl+Alt+Shift+Key` (only the set parts).
pub fn combo_string(c: &KeyCombo) -> String {
    let mut parts: Vec<String> = Vec::new();
    if c.modifiers.logo { parts.push("Super".into()); }
    if c.modifiers.ctrl { parts.push("Ctrl".into()); }
    if c.modifiers.alt { parts.push("Alt".into()); }
    if c.modifiers.shift { parts.push("Shift".into()); }
    if let Some(k) = c.key { parts.push(format!("{k:?}")); }
    parts.join("+")
}

/// Parse `Super+Shift+K` into a `KeyCombo`. Returns `None` if a token is
/// unknown or no key is present (modifier-only combos are rejected).
pub fn parse_combo(s: &str) -> Option<KeyCombo> {
    let mut m = ModifiersState::default();
    let mut key = None;
    for tok in s.split('+').map(str::trim).filter(|t| !t.is_empty()) {
        match tok {
            "Super" | "Logo" => m.logo = true,
            "Ctrl" | "Control" => m.ctrl = true,
            "Alt" => m.alt = true,
            "Shift" => m.shift = true,
            other => {
                if key.is_some() { return None; }
                key = Some(parse_key(other)?);
            }
        }
    }
    key.map(|k| KeyCombo { modifiers: m, key: Some(k) })
}

use smithay::backend::input::{InputBackend, KeyState};
use smithay::backend::session::Session;
use smithay::input::keyboard::{Keysym, ModifiersState};
use slots::library::input::keyboard::keyboard::combo::KeyCombo;
use slots::library::input::keyboard::keyboard::handler::ShortcutHandler;
use slots::library::input::keyboard::keyboard::key::Key;
use slots::library::input::keyboard::shortcut;
use slots::library::input::keyboard::format::format;
use model::environment::keybinding::base::{KeyBindings, KeyRow};

use crate::state::Loop;

pub fn input_received<I: InputBackend>(
    state: &mut Loop,
    keysym: Keysym,
    key_state: KeyState,
    modifiers: &ModifiersState,
) -> bool {
    let key = Key::from_keysym(keysym);
    if key.is_none() {
        return false;
    }

    let is_press = key_state == KeyState::Pressed;
    // Only handle presses.
    if !is_press {
        return false;
    }

    // Build the handler vector, applying the user's keybinding.json overrides.
    let handlers: Vec<ShortcutHandler<Loop>> =
        inline_shortcut_handlers(&state.inner.keybinding);

    for handler in handlers {
        if handler.combo.matches(modifiers, key) {
            if (handler.action)(state) {
                return true;
            }
        }
    }

    // Add Playback device control, TTY switches.
    return false;
}

const VOLUME_STEP: f64 = 0.05;

/// One bindable shortcut: a stable id (referenced by keybinding.json + the
/// settings Keys tab), a human label, the built-in default combo, and the action.
struct Bind {
    id: &'static str,
    label: &'static str,
    default: KeyCombo,
    /// Keep this one out of the settings Keys tab.
    ///
    /// For developer diagnostics: they are live, and still rebindable or
    /// disableable through `keybinding.json` by id, but listing them would put
    /// log-dumping tools in front of every user in a panel that is otherwise all
    /// window management.
    hidden: bool,
    action: Box<dyn Fn(&mut Loop) -> bool>,
}

/// The single source of truth for every compositor shortcut. `inline_shortcut_handlers`
/// turns these into live handlers (with overrides applied); `registry` exposes them
/// to the settings Keys tab.
fn bindings() -> Vec<Bind> {
    vec![
        // Diagnostic: queues a numbered test notification on the kernel host —
        // an OVERLAY shortcut, where the pill must draw. Each press queues one
        // more; a burst shows the FIFO.
        Bind { id: "notify_test", label: "Queue a test notification", default: shortcut!(Super + Alt + Shift + N), hidden: true, action: Box::new(|s| { notify_test(s); true }) },
        // (Settings has no global shortcut — reachable only via the overview Settings tab.)
        // No VT-switch or sink/media shortcuts here: deactivated AND not listed.
    ]
}

/// Build the live handlers, applying keybinding.json overrides (parse-or-default).
pub fn inline_shortcut_handlers(overrides: &KeyBindings) -> Vec<ShortcutHandler<Loop>> {
    let mut handlers: Vec<ShortcutHandler<Loop>> = bindings()
        .into_iter()
        .filter_map(|b| match overrides.combo_for(b.id) {
            // Empty override string = explicitly disabled: no handler at all.
            Some("") => None,
            Some(s) => Some(ShortcutHandler { combo: format::parse_combo(s).unwrap_or(b.default), action: b.action }),
            None => Some(ShortcutHandler { combo: b.default, action: b.action }),
        })
        .collect();

    // No nested remap here. Right Ctrl is substituted for Super at the INPUT
    // (`seat.keyboard/keyboard.input::shortcut_modifiers`), so a Super binding
    // already matches in a nested session and a Ctrl binding still means Ctrl.
    // Rewriting `logo` into `ctrl` here is what used to make those two
    // indistinguishable.

    // Always-on, non-configurable system handlers (cannot be rebound or disabled
    // — they are the escape hatches). Appended after the configurable bindings.
    handlers.extend(fixed_handlers());
    handlers
}

/// Critical, non-rebindable handlers: TTY/VT switches (the compositor holds the
/// VT in graphics mode, so without these Ctrl+Alt+F-keys do nothing) and the
/// Escape-cancels-picker fall-through.
fn fixed_handlers() -> Vec<ShortcutHandler<Loop>> {
    fn vt(n: u32) -> Box<dyn Fn(&mut Loop) -> bool> {
        Box::new(move |s| {
            tty(s, n);
            true
        })
    }
    vec![
        ShortcutHandler { combo: shortcut!(Ctrl + Alt + SwitchVt1), action: vt(1) },
        ShortcutHandler { combo: shortcut!(Ctrl + Alt + SwitchVt2), action: vt(2) },
        ShortcutHandler { combo: shortcut!(Ctrl + Alt + SwitchVt3), action: vt(3) },
        ShortcutHandler { combo: shortcut!(Ctrl + Alt + SwitchVt4), action: vt(4) },
        ShortcutHandler { combo: shortcut!(Ctrl + Alt + SwitchVt5), action: vt(5) },
        ShortcutHandler { combo: shortcut!(Ctrl + Alt + SwitchVt6), action: vt(6) },
        // Hardware media/volume keys — functional but not user-configurable.
        ShortcutHandler { combo: shortcut!(AudioRaiseVolume), action: Box::new(|s| {
            if let Some(a) = s.inner.kernel.get_mut(&drivers::audio::base::AUDIO_MUT) { let _ = a.adjust_volume(VOLUME_STEP); }
            true
        }) },
        ShortcutHandler { combo: shortcut!(AudioLowerVolume), action: Box::new(|s| {
            if let Some(a) = s.inner.kernel.get_mut(&drivers::audio::base::AUDIO_MUT) { let _ = a.adjust_volume(-VOLUME_STEP); }
            true
        }) },
        ShortcutHandler { combo: shortcut!(AudioMute), action: Box::new(|s| {
            if let Some(a) = s.inner.kernel.get_mut(&drivers::audio::base::AUDIO_MUT) { let _ = a.toggle_mute(); }
            true
        }) },
        ShortcutHandler { combo: shortcut!(AudioPlay), action: Box::new(|s| {
            if let Some(m) = s.inner.kernel.get_mut(&drivers::audio::base::MEDIA_MUT) { let _ = m.play_pause(); }
            true
        }) },
        ShortcutHandler { combo: shortcut!(AudioPause), action: Box::new(|s| {
            if let Some(m) = s.inner.kernel.get_mut(&drivers::audio::base::MEDIA_MUT) { let _ = m.pause(); }
            true
        }) },
        ShortcutHandler { combo: shortcut!(AudioStop), action: Box::new(|s| {
            if let Some(m) = s.inner.kernel.get_mut(&drivers::audio::base::MEDIA_MUT) { let _ = m.stop(); }
            true
        }) },
        ShortcutHandler { combo: shortcut!(AudioNext), action: Box::new(|s| {
            if let Some(m) = s.inner.kernel.get_mut(&drivers::audio::base::MEDIA_MUT) { let _ = m.next(); }
            true
        }) },
        ShortcutHandler { combo: shortcut!(AudioPrev), action: Box::new(|s| {
            if let Some(m) = s.inner.kernel.get_mut(&drivers::audio::base::MEDIA_MUT) { let _ = m.previous(); }
            true
        }) },
    ]
}

/// Read-only rows for the always-on system handlers (shown under "Built-in").
pub fn fixed() -> Vec<KeyRow> {
    let mk = |label: &str, combo: &str| KeyRow {
        id: String::new(),
        label: label.to_string(),
        default: combo.to_string(),
        combo: combo.to_string(),
        editable: false,
    };
    vec![
        mk("Switch to VT 1", "Ctrl+Alt+F1"),
        mk("Switch to VT 2", "Ctrl+Alt+F2"),
        mk("Switch to VT 3", "Ctrl+Alt+F3"),
        mk("Switch to VT 4", "Ctrl+Alt+F4"),
        mk("Switch to VT 5", "Ctrl+Alt+F5"),
        mk("Switch to VT 6", "Ctrl+Alt+F6"),
        mk("Volume up", "VolumeUp key"),
        mk("Volume down", "VolumeDown key"),
        mk("Mute", "Mute key"),
        mk("Play / Pause", "Play key"),
        mk("Pause media", "Pause key"),
        mk("Stop media", "Stop key"),
        mk("Next track", "Next key"),
        mk("Previous track", "Prev key"),
    ]
}

/// All shortcuts as `(id, label, default, effective)` rows for the settings Keys
/// tab. `hidden` bindings are omitted — they still run and can still be rebound
/// by id in `keybinding.json`, they are just not advertised.
pub fn registry(overrides: &KeyBindings) -> Vec<KeyRow> {
    bindings()
        .into_iter()
        .filter(|b| !b.hidden)
        .map(|b| {
            let default = format::combo_string(&b.default);
            let combo = overrides.combo_for(b.id).map(str::to_string).unwrap_or_else(|| default.clone());
            KeyRow { id: b.id.to_string(), label: b.label.to_string(), default, combo, editable: true }
        })
        .collect()
}

fn tty(state: &mut Loop, num: u32) {
    error!("VT switch to {:?}", num);
    state.loop_handle.insert_idle(move |state| {
        // Do not perform the action again when paused.
        if let crate::state::state::StatusSession::Paused =
            state.inner.status_session
        {
            error!("VT already paused");
            return;
        }

        // The libseat session is the only authority on VT switching.
        if let Some(ses) = &mut state.state.seat.libseat {
            ses.change_vt(num as i32);
        }
    });
}

fn notify_test(state: &mut Loop) {
    static COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
    let n = COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    crate::notify::state::base::announce(state.inner.kernel_channels(), format!("Test notification #{n}"));
}


//! Compositor key bindings: the filter that decides whether a key belongs to
//! the compositor or to the focused client: matcher, tables and listing.
//!
//! Pure: no Wayland, no xkb. The engine hands each human key edge over as
//! its raw keycode, the layout-agnostic keysym (the raw Latin sym, or the
//! raw current sym) and the effective modifier state, and executes what
//! [`KeyDisposition::Act`] names.
//!
//! The release rule: a release is swallowed on the strength of its
//! PRESS having been intercepted, never by re-matching the binding, because
//! the modifiers may have gone up first and a client must never see a
//! release for a press it did not get. Turning interception off mid-chord
//! still swallows the pending releases.

use std::collections::HashSet;

use comp_model::snapshot::{BindingRowSnapshot, BindingsSnapshot};

/// The xkb keysyms the default tables use (`<xkbcommon-keysyms.h>`).
pub mod keysym {
    pub const TAB: u32 = 0xff09;
    pub const ESCAPE: u32 = 0xff1b;
    pub const BRACKETLEFT: u32 = 0x005b;
    pub const BRACKETRIGHT: u32 = 0x005d;
    pub const M: u32 = 0x006d;
    pub const Q: u32 = 0x0071;
    /// `1`; `2`..`9` follow.
    pub const DIGIT_1: u32 = 0x0031;
    /// `F1`; `F2`..`F12` follow.
    pub const F1: u32 = 0xffbe;
    pub const F9: u32 = 0xffc6;
    pub const F12: u32 = 0xffc9;
}

/// The effective modifier state of one key edge (xkb's, not `depressed`, so
/// latched modifiers count).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub logo: bool,
    pub iso_level3_shift: bool,
    pub iso_level5_shift: bool,
    pub caps_lock: bool,
    pub num_lock: bool,
}

/// The non-lock modifiers a binding can require or forbid. Caps and Num
/// Lock are governed by [`ModifierPattern::ignore_locks`] instead.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ModifierSet {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub logo: bool,
    pub iso_level3_shift: bool,
    pub iso_level5_shift: bool,
}

impl ModifierSet {
    pub const NONE: Self = Self {
        ctrl: false,
        alt: false,
        shift: false,
        logo: false,
        iso_level3_shift: false,
        iso_level5_shift: false,
    };

    pub const fn logo() -> Self {
        Self { logo: true, ..Self::NONE }
    }

    pub const fn with_ctrl(mut self) -> Self {
        self.ctrl = true;
        self
    }

    pub const fn with_alt(mut self) -> Self {
        self.alt = true;
        self
    }

    pub const fn with_shift(mut self) -> Self {
        self.shift = true;
        self
    }

    const fn of(state: &Modifiers) -> Self {
        Self {
            ctrl: state.ctrl,
            alt: state.alt,
            shift: state.shift,
            logo: state.logo,
            iso_level3_shift: state.iso_level3_shift,
            iso_level5_shift: state.iso_level5_shift,
        }
    }

    const fn complement(self) -> Self {
        Self {
            ctrl: !self.ctrl,
            alt: !self.alt,
            shift: !self.shift,
            logo: !self.logo,
            iso_level3_shift: !self.iso_level3_shift,
            iso_level5_shift: !self.iso_level5_shift,
        }
    }

    /// The chord spelling of the set, in chord order.
    fn chord_names(self) -> Vec<&'static str> {
        [
            (self.ctrl, "Ctrl"),
            (self.alt, "Alt"),
            (self.shift, "Shift"),
            (self.logo, "Super"),
            (self.iso_level3_shift, "ISOLevel3Shift"),
            (self.iso_level5_shift, "ISOLevel5Shift"),
        ]
        .into_iter()
        .filter_map(|(held, name)| held.then_some(name))
        .collect()
    }
}

/// Which modifiers must be held and which must not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModifierPattern {
    pub required: ModifierSet,
    pub forbidden: ModifierSet,
    /// When true (the default), Caps Lock and Num Lock are not consulted.
    pub ignore_locks: bool,
}

impl ModifierPattern {
    /// Exactly these modifiers and no others: a superset chord belongs to
    /// the client.
    pub const fn exact(required: ModifierSet) -> Self {
        Self {
            required,
            forbidden: required.complement(),
            ignore_locks: true,
        }
    }

    pub fn matches(&self, state: &Modifiers) -> bool {
        let active = ModifierSet::of(state);
        let pairs = [
            (self.required.ctrl, self.forbidden.ctrl, active.ctrl),
            (self.required.alt, self.forbidden.alt, active.alt),
            (self.required.shift, self.forbidden.shift, active.shift),
            (self.required.logo, self.forbidden.logo, active.logo),
            (self.required.iso_level3_shift, self.forbidden.iso_level3_shift, active.iso_level3_shift),
            (self.required.iso_level5_shift, self.forbidden.iso_level5_shift, active.iso_level5_shift),
        ];
        let modifiers_ok = pairs
            .iter()
            .all(|&(required, forbidden, held)| (!required || held) && (!forbidden || !held));
        let locks_ok = self.ignore_locks || (!state.caps_lock && !state.num_lock);
        modifiers_ok && locks_ok
    }
}

/// What an intercepted binding does. The engine executes it; the names are
/// the wire's `bindings.table[].action`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingAction {
    /// `xdg_toplevel.close` (or the X11 close) to the focused window.
    RequestCloseFocused,
    /// Restore the most recently minimised live toplevel.
    RestoreMostRecentlyMinimized,
    /// Switch the default output to workspace `n` (Super+n).
    WorkspaceJump(u8),
    /// Move the focused window to workspace `n` and follow it (Super+Shift+n).
    WorkspaceMove(u8),
    /// Previous/next workspace, wrapping (Super+[ / Super+]).
    WorkspaceStep { prev: bool },
    /// Cycle mapped managed windows in stable creation order.
    CycleWindow { reverse: bool },
    /// Stop a nested compositor.
    ExitNestedCompositor,
    /// Turn normal interception off or back on. Always reserved.
    ToggleInterception,
    /// Switch to one Linux VT (kms-live).
    SwitchVt(u8),
    /// The opt-in F9 Bus key.
    SendBusKey,
}

impl BindingAction {
    pub const fn name(self) -> &'static str {
        match self {
            Self::RequestCloseFocused => "RequestCloseFocused",
            Self::RestoreMostRecentlyMinimized => "RestoreMostRecentlyMinimized",
            Self::WorkspaceJump(_) => "WorkspaceJump",
            Self::WorkspaceMove(_) => "WorkspaceMove",
            Self::WorkspaceStep { prev: false } => "WorkspaceNext",
            Self::WorkspaceStep { prev: true } => "WorkspacePrev",
            Self::CycleWindow { reverse: false } => "CycleWindowForward",
            Self::CycleWindow { reverse: true } => "CycleWindowBackward",
            Self::ExitNestedCompositor => "ExitNestedCompositor",
            Self::ToggleInterception => "ToggleInterception",
            Self::SwitchVt(_) => "SwitchVt",
            Self::SendBusKey => "SendBusKey",
        }
    }
}

/// Which default table: `bindings.profile`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingProfile {
    Nested,
    KmsLive,
}

impl BindingProfile {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Nested => "nested",
            Self::KmsLive => "kms-live",
        }
    }

    /// The profile a `bindings.profile` name selects (`nested` otherwise).
    pub fn from_name(name: &str) -> Self {
        if name == "kms-live" { Self::KmsLive } else { Self::Nested }
    }
}

/// One entry of the table.
#[derive(Clone, Copy, Debug)]
pub struct Binding {
    /// Stable identifier.
    pub id: &'static str,
    /// The layout-agnostic keysym.
    pub keysym: u32,
    pub keysym_name: &'static str,
    pub modifiers: ModifierPattern,
    pub action: BindingAction,
    /// Matchable while normal interception is off (the escape hatch).
    pub reserved: bool,
}

impl Binding {
    /// `Ctrl+Alt+F1`, `Super+q`.
    pub fn chord(&self) -> String {
        let mut parts = self.modifiers.required.chord_names();
        parts.push(self.keysym_name);
        parts.join("+")
    }
}

const WORKSPACE_IDS: [(&str, &str, &str); 9] = [
    ("1", "workspace-jump-1", "workspace-move-1"),
    ("2", "workspace-jump-2", "workspace-move-2"),
    ("3", "workspace-jump-3", "workspace-move-3"),
    ("4", "workspace-jump-4", "workspace-move-4"),
    ("5", "workspace-jump-5", "workspace-move-5"),
    ("6", "workspace-jump-6", "workspace-move-6"),
    ("7", "workspace-jump-7", "workspace-move-7"),
    ("8", "workspace-jump-8", "workspace-move-8"),
    ("9", "workspace-jump-9", "workspace-move-9"),
];

const VT_IDS: [(&str, &str); 12] = [
    ("switch-vt-1", "F1"),
    ("switch-vt-2", "F2"),
    ("switch-vt-3", "F3"),
    ("switch-vt-4", "F4"),
    ("switch-vt-5", "F5"),
    ("switch-vt-6", "F6"),
    ("switch-vt-7", "F7"),
    ("switch-vt-8", "F8"),
    ("switch-vt-9", "F9"),
    ("switch-vt-10", "F10"),
    ("switch-vt-11", "F11"),
    ("switch-vt-12", "F12"),
];

const fn binding(id: &'static str, keysym: u32, keysym_name: &'static str, required: ModifierSet, action: BindingAction) -> Binding {
    Binding {
        id,
        keysym,
        keysym_name,
        modifiers: ModifierPattern::exact(required),
        action,
        reserved: false,
    }
}

const fn restore_minimized() -> Binding {
    binding(
        "restore-recent-minimized",
        keysym::M,
        "m",
        ModifierSet::logo().with_shift(),
        BindingAction::RestoreMostRecentlyMinimized,
    )
}

const fn cycle_window(reverse: bool) -> Binding {
    // The raw symbol is Tab even with Shift held (not ISO_Left_Tab).
    let required = if reverse {
        ModifierSet::NONE.with_alt().with_shift()
    } else {
        ModifierSet::NONE.with_alt()
    };
    binding(
        if reverse { "cycle-window-backward" } else { "cycle-window-forward" },
        keysym::TAB,
        "Tab",
        required,
        BindingAction::CycleWindow { reverse },
    )
}

const fn workspace_step(prev: bool) -> Binding {
    binding(
        if prev { "workspace-prev" } else { "workspace-next" },
        if prev { keysym::BRACKETLEFT } else { keysym::BRACKETRIGHT },
        if prev { "bracketleft" } else { "bracketright" },
        ModifierSet::logo(),
        BindingAction::WorkspaceStep { prev },
    )
}

/// Super+n jumps, Super+Shift+n moves and follows, n in 1..=9, in both
/// profiles. The matcher sees the level-0 symbol, so Super+Shift+1 still
/// reads `1`.
fn workspace_bindings() -> impl Iterator<Item = Binding> {
    WORKSPACE_IDS.into_iter().zip(1u8..).flat_map(|((name, jump, step), n)| {
        let keysym = keysym::DIGIT_1 + u32::from(n) - 1;
        [
            binding(jump, keysym, name, ModifierSet::logo(), BindingAction::WorkspaceJump(n)),
            binding(step, keysym, name, ModifierSet::logo().with_shift(), BindingAction::WorkspaceMove(n)),
        ]
    })
}

/// The compiled table.
#[derive(Clone, Debug)]
pub struct BindingTable {
    bindings: Vec<Binding>,
}

impl BindingTable {
    /// The nested set. No terminal launch, deliberately: it proves no
    /// keyboard mechanism and drags in launcher policy.
    pub fn nested_defaults() -> Self {
        let mut bindings = vec![
            binding("close-focused", keysym::Q, "q", ModifierSet::logo(), BindingAction::RequestCloseFocused),
            restore_minimized(),
            cycle_window(false),
            cycle_window(true),
            binding(
                "exit-nested-compositor",
                keysym::ESCAPE,
                "Escape",
                ModifierSet::logo().with_shift(),
                BindingAction::ExitNestedCompositor,
            ),
            Binding {
                reserved: true,
                ..binding(
                    "toggle-interception",
                    keysym::F12,
                    "F12",
                    ModifierSet::logo().with_ctrl().with_shift(),
                    BindingAction::ToggleInterception,
                )
            },
        ];
        bindings.extend(workspace_bindings());
        bindings.extend([workspace_step(false), workspace_step(true)]);
        Self { bindings }
    }

    /// The kms-live set: Ctrl+Alt+F1..F12 switch VTs (reserved), no
    /// close or exit chord.
    pub fn kms_live_defaults() -> Self {
        let vt_modifiers = ModifierSet::NONE.with_ctrl().with_alt();
        let mut bindings: Vec<Binding> = VT_IDS
            .into_iter()
            .zip(1u8..)
            .map(|((id, name), vt)| Binding {
                reserved: true,
                ..binding(id, keysym::F1 + u32::from(vt) - 1, name, vt_modifiers, BindingAction::SwitchVt(vt))
            })
            .collect();
        bindings.push(restore_minimized());
        bindings.extend([cycle_window(false), cycle_window(true)]);
        bindings.extend(workspace_bindings());
        bindings.extend([workspace_step(false), workspace_step(true)]);
        Self { bindings }
    }

    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    fn find(&self, keysym: u32, state: &Modifiers, normal_interception_enabled: bool) -> Option<&Binding> {
        self.bindings.iter().find(|binding| {
            (normal_interception_enabled || binding.reserved)
                && binding.keysym == keysym
                && binding.modifiers.matches(state)
        })
    }
}

/// What to do with one key edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyDisposition {
    /// Not ours: the client sees it.
    Forward,
    /// Ours, and it means something.
    Act(BindingAction),
    /// Ours only because its press was.
    SwallowRelease,
}

/// The engine's binding state: the table, the interception switch and the
/// presses awaiting their release.
#[derive(Clone, Debug)]
pub struct BindingState {
    table: BindingTable,
    enabled: bool,
    profile: BindingProfile,
    intercepted: HashSet<u32>,
    bus_key_pressed: Option<u32>,
}

impl BindingState {
    pub fn for_profile(profile: BindingProfile, enabled: bool) -> Self {
        let table = match profile {
            BindingProfile::Nested => BindingTable::nested_defaults(),
            BindingProfile::KmsLive => BindingTable::kms_live_defaults(),
        };
        Self {
            table,
            enabled,
            profile,
            intercepted: HashSet::new(),
            bus_key_pressed: None,
        }
    }

    /// Add the opt-in F9 Bus key.
    pub fn with_bus_key(mut self, enabled: bool) -> Self {
        if enabled {
            self.table.bindings.push(binding(
                "bus-f9",
                keysym::F9,
                "F9",
                ModifierSet::NONE,
                BindingAction::SendBusKey,
            ));
        }
        self
    }

    pub fn profile(&self) -> BindingProfile {
        self.profile
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Flip interception; pending releases are kept (the release rule).
    pub fn toggle_interception(&mut self) -> bool {
        self.enabled = !self.enabled;
        self.enabled
    }

    /// `bindings.*` as the wire serves it.
    pub fn snapshot(&self) -> BindingsSnapshot {
        BindingsSnapshot {
            enabled: self.enabled,
            profile: self.profile.name(),
            table: self
                .table
                .bindings
                .iter()
                .map(|binding| BindingRowSnapshot {
                    chord: binding.chord(),
                    action: binding.action.name(),
                })
                .collect(),
        }
    }

    /// Decide one key edge. `keysym` is the layout-agnostic symbol; `None`
    /// (no usable symbol) never matches.
    pub fn dispatch(&mut self, keycode: u32, pressed: bool, keysym: Option<u32>, modifiers: &Modifiers) -> KeyDisposition {
        self.decide(keycode, pressed, keysym, modifiers, false)
    }

    /// Decide a key while ext-session-lock owns the seat: only the VT
    /// switch stays a binding; every other chord goes to the lock surface.
    pub fn dispatch_session_locked(
        &mut self,
        keycode: u32,
        pressed: bool,
        keysym: Option<u32>,
        modifiers: &Modifiers,
    ) -> KeyDisposition {
        self.decide(keycode, pressed, keysym, modifiers, true)
    }

    fn decide(&mut self, keycode: u32, pressed: bool, keysym: Option<u32>, modifiers: &Modifiers, locked: bool) -> KeyDisposition {
        if !pressed {
            if self.bus_key_pressed == Some(keycode) {
                self.bus_key_pressed = None;
            }
            return if self.intercepted.remove(&keycode) {
                KeyDisposition::SwallowRelease
            } else {
                KeyDisposition::Forward
            };
        }
        if self.bus_key_pressed == Some(keycode) {
            return KeyDisposition::SwallowRelease;
        }
        let Some(keysym) = keysym else {
            return KeyDisposition::Forward;
        };
        let Some(binding) = self.table.find(keysym, modifiers, self.enabled) else {
            return KeyDisposition::Forward;
        };
        let action = binding.action;
        if locked && !matches!(action, BindingAction::SwitchVt(_)) {
            return KeyDisposition::Forward;
        }
        if action == BindingAction::SendBusKey {
            self.bus_key_pressed = Some(keycode);
        }
        self.intercepted.insert(keycode);
        KeyDisposition::Act(action)
    }
}

#[cfg(test)]
#[path = "bindings_tests.rs"]
mod tests;

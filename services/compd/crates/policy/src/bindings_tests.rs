//! Tests for the matcher and the tables.

use super::*;

fn mods(logo: bool, shift: bool, ctrl: bool) -> Modifiers {
    Modifiers {
        logo,
        shift,
        ctrl,
        ..Modifiers::default()
    }
}

fn nested() -> BindingState {
    BindingState::for_profile(BindingProfile::Nested, true)
}

const Q_CODE: u32 = 24;

#[test]
fn super_q_is_intercepted_and_its_release_swallowed() {
    let mut state = nested();
    assert_eq!(
        state.dispatch(Q_CODE, true, Some(keysym::Q), &mods(true, false, false)),
        KeyDisposition::Act(BindingAction::RequestCloseFocused)
    );
    assert_eq!(
        state.dispatch(Q_CODE, false, Some(keysym::Q), &mods(true, false, false)),
        KeyDisposition::SwallowRelease
    );
    assert!(state.intercepted.is_empty());
}

#[test]
fn release_is_swallowed_even_after_the_modifier_is_let_go_first() {
    let mut state = nested();
    state.dispatch(Q_CODE, true, Some(keysym::Q), &mods(true, false, false));
    assert_eq!(
        state.dispatch(Q_CODE, false, Some(keysym::Q), &mods(false, false, false)),
        KeyDisposition::SwallowRelease
    );
}

#[test]
fn plain_keys_and_unswallowed_releases_reach_the_client() {
    let mut state = nested();
    assert_eq!(
        state.dispatch(Q_CODE, true, Some(keysym::Q), &mods(false, false, false)),
        KeyDisposition::Forward
    );
    assert_eq!(
        state.dispatch(Q_CODE, false, Some(keysym::Q), &mods(false, false, false)),
        KeyDisposition::Forward
    );
    assert_eq!(state.dispatch(Q_CODE, true, None, &mods(true, false, false)), KeyDisposition::Forward);
}

#[test]
fn a_superset_chord_does_not_match_an_exact_binding() {
    let mut state = nested();
    assert_eq!(
        state.dispatch(Q_CODE, true, Some(keysym::Q), &mods(true, false, true)),
        KeyDisposition::Forward
    );
}

#[test]
fn lock_modifiers_do_not_break_a_binding_unless_asked() {
    let mut state = nested();
    let with_locks = Modifiers {
        logo: true,
        caps_lock: true,
        num_lock: true,
        ..Modifiers::default()
    };
    assert_eq!(
        state.dispatch(Q_CODE, true, Some(keysym::Q), &with_locks),
        KeyDisposition::Act(BindingAction::RequestCloseFocused)
    );
    let mut pattern = ModifierPattern::exact(ModifierSet::logo());
    pattern.ignore_locks = false;
    assert!(!pattern.matches(&with_locks));
    assert!(pattern.matches(&mods(true, false, false)));
}

#[test]
fn disabled_interception_forwards_all_but_the_reserved_toggle() {
    let mut state = BindingState::for_profile(BindingProfile::Nested, false);
    assert_eq!(
        state.dispatch(Q_CODE, true, Some(keysym::Q), &mods(true, false, false)),
        KeyDisposition::Forward
    );
    assert_eq!(
        state.dispatch(96, true, Some(keysym::F12), &mods(true, true, true)),
        KeyDisposition::Act(BindingAction::ToggleInterception)
    );
}

#[test]
fn toggling_off_mid_chord_still_swallows_the_pending_release() {
    let mut state = nested();
    state.dispatch(Q_CODE, true, Some(keysym::Q), &mods(true, false, false));
    state.toggle_interception();
    assert_eq!(
        state.dispatch(Q_CODE, false, Some(keysym::Q), &mods(true, false, false)),
        KeyDisposition::SwallowRelease
    );
}

#[test]
fn super_digits_jump_and_super_shift_digits_move() {
    for profile in [BindingProfile::Nested, BindingProfile::KmsLive] {
        let mut state = BindingState::for_profile(profile, true);
        assert_eq!(
            state.dispatch(11, true, Some(keysym::DIGIT_1 + 1), &mods(true, false, false)),
            KeyDisposition::Act(BindingAction::WorkspaceJump(2))
        );
        assert_eq!(
            state.dispatch(18, true, Some(keysym::DIGIT_1 + 8), &mods(true, true, false)),
            KeyDisposition::Act(BindingAction::WorkspaceMove(9))
        );
        assert_eq!(
            state.dispatch(35, true, Some(keysym::BRACKETRIGHT), &mods(true, false, false)),
            KeyDisposition::Act(BindingAction::WorkspaceStep { prev: false })
        );
    }
}

#[test]
fn kms_live_maps_ctrl_alt_function_keys_to_vts_and_nested_never_does() {
    let ctrl_alt = Modifiers {
        ctrl: true,
        alt: true,
        ..Modifiers::default()
    };
    let mut kms = BindingState::for_profile(BindingProfile::KmsLive, false);
    assert_eq!(
        kms.dispatch(67, true, Some(keysym::F1), &ctrl_alt),
        KeyDisposition::Act(BindingAction::SwitchVt(1))
    );
    assert_eq!(
        kms.dispatch(96, true, Some(keysym::F12), &ctrl_alt),
        KeyDisposition::Act(BindingAction::SwitchVt(12))
    );
    let mut nested = nested();
    assert_eq!(nested.dispatch(67, true, Some(keysym::F1), &ctrl_alt), KeyDisposition::Forward);
}

#[test]
fn a_locked_session_keeps_only_the_vt_switch() {
    let mut kms = BindingState::for_profile(BindingProfile::KmsLive, true);
    let ctrl_alt = Modifiers {
        ctrl: true,
        alt: true,
        ..Modifiers::default()
    };
    assert_eq!(
        kms.dispatch_session_locked(11, true, Some(keysym::DIGIT_1 + 1), &mods(true, false, false)),
        KeyDisposition::Forward
    );
    assert!(kms.intercepted.is_empty());
    assert_eq!(
        kms.dispatch_session_locked(68, true, Some(keysym::F1 + 1), &ctrl_alt),
        KeyDisposition::Act(BindingAction::SwitchVt(2))
    );
}

#[test]
fn the_bus_key_is_opt_in_and_fires_once_until_release() {
    let none = Modifiers::default();
    assert_eq!(nested().dispatch(75, true, Some(keysym::F9), &none), KeyDisposition::Forward);
    let mut state = nested().with_bus_key(true);
    assert_eq!(
        state.dispatch(75, true, Some(keysym::F9), &none),
        KeyDisposition::Act(BindingAction::SendBusKey)
    );
    assert_eq!(state.dispatch(75, true, Some(keysym::F9), &none), KeyDisposition::SwallowRelease);
    assert_eq!(state.dispatch(75, false, Some(keysym::F9), &none), KeyDisposition::SwallowRelease);
    assert_eq!(
        state.dispatch(75, true, Some(keysym::F9), &none),
        KeyDisposition::Act(BindingAction::SendBusKey)
    );
}

#[test]
fn chords_and_ids_are_unique_and_spelled_as_comp_spells_them() {
    for profile in [BindingProfile::Nested, BindingProfile::KmsLive] {
        let state = BindingState::for_profile(profile, true);
        let bindings = state.table.bindings();
        let ids: HashSet<_> = bindings.iter().map(|binding| binding.id).collect();
        assert_eq!(ids.len(), bindings.len());
        let chords: HashSet<_> = bindings.iter().map(Binding::chord).collect();
        assert_eq!(chords.len(), bindings.len());
        assert_eq!(bindings.iter().filter(|binding| binding.reserved).count(), match profile {
            BindingProfile::Nested => 1,
            BindingProfile::KmsLive => 12,
        });
    }
    let snapshot = nested().snapshot();
    assert_eq!(snapshot.profile, "nested");
    assert_eq!(snapshot.table[0].chord, "Super+q");
    assert_eq!(snapshot.table[0].action, "RequestCloseFocused");
    assert!(snapshot.table.iter().any(|row| row.chord == "Ctrl+Shift+Super+F12" && row.action == "ToggleInterception"));
    assert!(snapshot.table.iter().any(|row| row.chord == "Shift+Super+2" && row.action == "WorkspaceMove"));
    let kms = BindingState::for_profile(BindingProfile::KmsLive, true).snapshot();
    assert_eq!(kms.table[0].chord, "Ctrl+Alt+F1");
    assert_eq!(kms.table[0].action, "SwitchVt");
}

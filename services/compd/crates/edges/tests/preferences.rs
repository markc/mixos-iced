// SPDX-License-Identifier: MIT OR Apache-2.0
use std::time::Duration;
use edges::{Edge, LogicalSize, OutputKey, PanelConfigError, PanelInput, PanelMode, PanelPreference, ShellModel};

fn model() -> ShellModel {
    ShellModel::new(OutputKey::new("fixture").unwrap(), LogicalSize::new(800.0, 600.0).unwrap(),
        Duration::ZERO, Duration::from_millis(800), Duration::from_millis(200)).unwrap()
}
fn policy(edge: Edge, mode: PanelMode, thickness: f32) -> [Option<PanelPreference>; 4] {
    let mut values = [None; 4];
    values[edge.index()] = Some(PanelPreference::new(mode, thickness).unwrap());
    values
}

#[test]
fn acquisition_cancels_resize_and_restores_settled_local_baseline() {
    let mut model = model();
    model.restore_mode(Edge::Left, Duration::ZERO, PanelMode::Pinned).unwrap();
    model.restore_thickness(Edge::Left, 180.0).unwrap();
    model.carousel_mut(Edge::Left).register("one").unwrap();
    model.carousel_mut(Edge::Left).register("two").unwrap();
    model.panel_input(Edge::Left, Duration::ZERO, PanelInput::ResizeStarted).unwrap();
    model.resize_thickness(Edge::Left, 240.0).unwrap();
    assert!(model.panel(Edge::Left).resize_active);
    model.set_preferences(policy(Edge::Left, PanelMode::Docked, 256.0), Duration::ZERO);
    let baseline = model.persistent_panel(Edge::Left);
    assert_eq!(baseline.mode, PanelMode::Pinned);
    assert_eq!(baseline.thickness, 180.0);
    assert!(baseline.remembered);
    assert!(!model.panel(Edge::Left).resize_active);
    model.carousel_mut(Edge::Left).select_id("two");
    model.set_preferences(policy(Edge::Left, PanelMode::Docked, 200.0), Duration::ZERO);
    model.panel_input(Edge::Left, Duration::ZERO, PanelInput::ResizeCancelled).unwrap();
    assert_eq!(model.panel(Edge::Left).thickness_px, 200.0);
    assert_eq!(model.persistent_panel(Edge::Left), baseline);
    model.set_preferences([None; 4], Duration::ZERO);
    assert_eq!(model.panel(Edge::Left).mode, PanelMode::Pinned);
    assert_eq!(model.panel(Edge::Left).thickness_px, 180.0);
    assert_eq!(model.carousel(Edge::Left).active_id(), Some("two"));
}

#[test]
fn thickness_only_hidden_updates_keep_reveal_holders_and_deadline() {
    let mut model = model();
    model.set_preferences(policy(Edge::Bottom, PanelMode::Hidden, 40.0), Duration::ZERO);
    model.panel_input(Edge::Bottom, Duration::ZERO, PanelInput::Reveal).unwrap();
    model.panel_input(Edge::Bottom, Duration::ZERO, PanelInput::PointerEntered).unwrap();
    model.panel_input(Edge::Bottom, Duration::ZERO, PanelInput::PointerLeft).unwrap();
    let before = model.panel(Edge::Bottom);
    let concealed = model.set_preferences(policy(Edge::Bottom, PanelMode::Hidden, 80.0), Duration::ZERO);
    let after = model.panel(Edge::Bottom);
    assert_eq!(concealed, [false; 4]);
    assert_eq!(after.transient_revealed, before.transient_revealed);
    assert_eq!(after.hide_at, before.hide_at);
    assert_eq!(after.target_fraction, before.target_fraction);
}

#[test]
fn opposing_budget_and_page_floor_fit_then_recover_requested_size() {
    let mut model = model();
    let mut values = policy(Edge::Left, PanelMode::Docked, 256.0);
    values[Edge::Right.index()] = values[Edge::Left.index()];
    model.set_preferences(values, Duration::ZERO);
    model.set_geometry(LogicalSize::new(300.0, 600.0).unwrap());
    assert!(model.panel(Edge::Left).exclusive_zone_px + model.panel(Edge::Right).exclusive_zone_px <= 299.01);
    assert!(model.panel(Edge::Left).thickness_px < 256.0);
    model.set_geometry(LogicalSize::new(800.0, 600.0).unwrap());
    assert_eq!(model.panel(Edge::Left).thickness_px, 256.0);
    assert_eq!(model.panel(Edge::Right).thickness_px, 256.0);
    model.carousel_mut(Edge::Left).register("wide").unwrap();
    model.set_page_minimum_thickness(Edge::Left, "wide", Some(440.0));
    assert_eq!(model.panel(Edge::Left).thickness_px, 440.0);
    assert_eq!(model.panel(Edge::Left).settled_thickness_px, 256.0);
    model.carousel_mut(Edge::Left).remove("wide").unwrap();
    assert_eq!(model.panel(Edge::Left).thickness_px, 256.0);
}

#[test]
fn managed_interactive_mutations_cannot_change_mode_or_size() {
    let mut model = model();
    model.set_preferences(policy(Edge::Bottom, PanelMode::Docked, 40.0), Duration::ZERO);
    let before = model.panel(Edge::Bottom);
    for input in [PanelInput::ToggleShown, PanelInput::PinToggle, PanelInput::DockToggle,
        PanelInput::Release, PanelInput::SetMode(PanelMode::Hidden), PanelInput::ResizeStarted,
        PanelInput::ResizeCancelled, PanelInput::ResizeCompleted] {
        assert!(!model.panel_input(Edge::Bottom, Duration::ZERO, input).unwrap().changed);
        assert_eq!(model.panel(Edge::Bottom), before);
    }
    assert_eq!(model.resize_thickness(Edge::Bottom, 100.0), Err(PanelConfigError::SettingsManaged(Edge::Bottom)));
    assert_eq!(model.panel(Edge::Bottom), before);
}

#[test]
fn empty_edges_and_migration_keep_policy_and_local_baseline() {
    let mut model = model();
    model.suppress_empty_edges(true);
    model.set_preferences(policy(Edge::Top, PanelMode::Docked, 16.0), Duration::ZERO);
    assert_eq!(model.panel(Edge::Top).exclusive_zone_px, 0.0);
    model.carousel_mut(Edge::Top).register("page").unwrap();
    model.reconcile_preferences();
    assert_eq!(model.panel(Edge::Top).exclusive_zone_px, 24.0);
    let baseline = model.persistent_panel(Edge::Top);
    let mut next = ShellModel::new(OutputKey::new("next").unwrap(), LogicalSize::new(1200.0, 800.0).unwrap(),
        Duration::ZERO, Duration::from_millis(800), Duration::from_millis(200)).unwrap();
    next.carry_live_state(&model);
    assert_eq!(next.preference(Edge::Top), model.preference(Edge::Top));
    assert_eq!(next.persistent_panel(Edge::Top), baseline);
    next.set_preferences([None; 4], Duration::ZERO);
    assert!(!next.has_remembered_thickness(Edge::Top));
    assert_eq!(next.panel(Edge::Top).mode, PanelMode::Hidden);
}

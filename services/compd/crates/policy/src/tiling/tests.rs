// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use surfaces::SurfaceRole;

fn area(width: i32) -> Rect { Rect { x: -20, y: 32, width, height: 600 } }
fn group() -> Group { Group { output: "fixture-output".into(), workspace: 1 } }
fn window(registry: &mut Registry<u32>, handle: u32) -> Target {
    let (id, generation) = registry.take_role(handle, SurfaceRole::Toplevel, None).unwrap();
    registry.set_mapped(id, true).unwrap();
    registry.set_workspace(id, 1).unwrap();
    Target { id, generation }
}
fn member(target: Target) -> Member { Member { target, group: group(), normal: Rect { x: 40, y: 60, width: 320, height: 240 } } }
fn plain(_: Target) -> Facts { Facts::default() }
fn ready(plan: Plan) -> Vec<Allocation> { match plan { Plan::Ready(cells) => cells, other => panic!("{other:?}") } }

#[test]
fn columns_conserve_every_pixel_and_keep_prepared_ssd_inside_its_cell() {
    for width in 1..=513 {
        for count in 1..=width.min(16) {
            let cells = columns(area(width), &vec![Constraints::default(); count as usize]).unwrap();
            assert_eq!(cells.first().unwrap().outer.x, -20);
            assert_eq!(cells.iter().map(|cell| cell.outer.width).sum::<i32>(), width);
            for pair in cells.windows(2) { assert_eq!(pair[0].outer.x + pair[0].outer.width, pair[1].outer.x); }
            assert_eq!(cells.last().unwrap().outer.x + cells.last().unwrap().outer.width, -20 + width);
        }
    }
    let hints = Constraints { insets: Insets { left: 2, right: 3, top: 24, bottom: 1 }, ..Constraints::default() };
    let cells = columns(area(1001), &[hints; 3]).unwrap();
    assert_eq!(cells.iter().map(|cell| cell.outer.width).collect::<Vec<_>>(), [333, 334, 334]);
    assert_eq!(cells[0].content, Rect { x: -18, y: 56, width: 328, height: 575 });
}

#[test]
fn infeasible_hints_or_geometry_never_return_partial_or_zero_cells() {
    assert_eq!(columns(area(1), &[Constraints::default(); 2]), Err(LayoutError::InsufficientArea { index: 0 }));
    let min = Constraints { min_size: (501, 0), ..Constraints::default() };
    assert_eq!(columns(area(1000), &[Constraints::default(), min]), Err(LayoutError::InsufficientArea { index: 1 }));
    let max = Constraints { max_size: (499, 0), ..Constraints::default() };
    assert_eq!(columns(area(1000), &[max; 2]), Err(LayoutError::InsufficientArea { index: 0 }));
    let tall = Constraints { min_size: (0, 601), ..Constraints::default() };
    assert_eq!(columns(area(1000), &[tall]), Err(LayoutError::InsufficientArea { index: 0 }));
    let bounded = Constraints { max_size: (0, 599), ..Constraints::default() };
    assert_eq!(columns(area(1000), &[bounded]), Err(LayoutError::InsufficientArea { index: 0 }));
    let bad = Constraints { min_size: (400, 0), max_size: (200, 0), ..Constraints::default() };
    assert_eq!(columns(area(1000), &[bad]), Err(LayoutError::InvalidConstraints { index: 0 }));
    let negative = Constraints { insets: Insets { top: -1, ..Insets::default() }, ..Constraints::default() };
    assert_eq!(columns(area(1000), &[negative]), Err(LayoutError::InvalidConstraints { index: 0 }));
    let huge = Constraints { insets: Insets { left: i32::MAX, right: i32::MAX, ..Insets::default() }, ..Constraints::default() };
    assert_eq!(columns(area(1000), &[huge]), Err(LayoutError::InsufficientArea { index: 0 }));
    assert_eq!(columns(Rect { x: i32::MAX, ..area(10) }, &[Constraints::default()]), Err(LayoutError::InvalidArea));
    let wide = Rect { x: 0, y: 0, width: i32::MAX, height: i32::MAX };
    assert_eq!(columns(wide, &[Constraints::default(); MAX_MEMBERS]).unwrap().iter().map(|cell| i64::from(cell.outer.width)).sum::<i64>(), i64::from(i32::MAX));
    assert_eq!(columns(area(1000), &[Constraints::default(); MAX_MEMBERS + 1]), Err(LayoutError::Capacity));
}

#[test]
fn invalid_admission_never_captures_a_restore_or_mutates_membership() {
    let mut registry = Registry::new();
    let target = window(&mut registry, 1);
    let mut tiles = Tiles::default();
    let mut invalid = member(target);
    invalid.normal.height = 0;
    assert_eq!(tiles.admit(&registry, invalid, Some(area(1000)), plain), Err(AdmissionError::InvalidRestore));
    let mut invalid = member(target);
    invalid.group.workspace = 2;
    assert_eq!(tiles.admit(&registry, invalid, Some(area(1000)), plain), Err(AdmissionError::InvalidGroup));
    let mut invalid = member(target);
    invalid.group.output = "x".repeat(MAX_OUTPUT_NAME_BYTES + 1);
    assert_eq!(tiles.admit(&registry, invalid, Some(area(1000)), plain), Err(AdmissionError::InvalidGroup));
    assert_eq!(tiles.admit(&registry, member(target), None, plain), Err(AdmissionError::Layout(LayoutError::NoOutput)));
    registry.set_mapped(target.id, false).unwrap();
    assert_eq!(tiles.admit(&registry, member(target), Some(area(1000)), plain), Err(AdmissionError::Target(WindowTargetError::NotMapped)));
    let (id, generation) = registry.take_role(2, SurfaceRole::X11 { override_redirect: true }, None).unwrap();
    registry.set_mapped(id, true).unwrap();
    assert_eq!(tiles.admit(&registry, member(Target { id, generation }), Some(area(1000)), plain), Err(AdmissionError::Target(WindowTargetError::NotManaged)));
    assert!(tiles.members().is_empty());
}

#[test]
fn admission_is_atomic_idempotent_and_preserves_the_first_restore_on_transfer() {
    let mut registry = Registry::new();
    let a = window(&mut registry, 1);
    let b = window(&mut registry, 2);
    let mut tiles = Tiles::default();
    assert!(tiles.admit(&registry, member(a), Some(area(1000)), plain).unwrap());
    let mut changed_restore = member(a);
    changed_restore.normal.width = 900;
    assert!(!tiles.admit(&registry, changed_restore.clone(), Some(area(1000)), plain).unwrap());
    assert_eq!(tiles.member(a).unwrap().normal, member(a).normal);
    let before = tiles.clone();
    let hints = |_: Target| Facts { constraints: Constraints { min_size: (600, 0), ..Constraints::default() }, overlay: false };
    assert_eq!(tiles.admit(&registry, member(b), Some(area(1000)), hints), Err(AdmissionError::Layout(LayoutError::InsufficientArea { index: 0 })));
    assert_eq!(tiles, before, "failed sibling allocation must not partially admit");
    changed_restore.group.output = "other-output".into();
    assert!(tiles.admit(&registry, changed_restore, Some(area(1000)), plain).unwrap());
    assert_eq!(tiles.member(a).unwrap().normal, member(a).normal);
    assert!(matches!(tiles.remove(&registry, Target { generation: a.generation + 1, ..a }), Err(AdmissionError::Target(WindowTargetError::StaleTarget { .. }))));
    assert_eq!(tiles.remove(&registry, a).unwrap().unwrap().normal, member(a).normal);
}

#[test]
fn suspension_overlays_pending_work_area_and_workspace_keep_order_and_restores() {
    let mut registry = Registry::new();
    let targets: Vec<_> = (1..=3).map(|handle| window(&mut registry, handle)).collect();
    let mut tiles = Tiles::default();
    for target in &targets { tiles.admit(&registry, member(*target), Some(area(1001)), plain).unwrap(); }
    let original = tiles.members().to_vec();
    registry.set_minimized(targets[1].id, true).unwrap();
    assert_eq!(ready(tiles.plan(&registry, &group(), Some(area(1001)), plain)).iter().map(|cell| cell.target).collect::<Vec<_>>(), [targets[0], targets[2]]);
    registry.set_minimized(targets[1].id, false).unwrap();
    registry.set_mapped(targets[1].id, false).unwrap();
    tiles.reconcile(&registry);
    assert_eq!(tiles.members(), original);
    registry.set_mapped(targets[1].id, true).unwrap();
    let overlay = |target| Facts { overlay: target == targets[1], ..Facts::default() };
    assert_eq!(ready(tiles.plan(&registry, &group(), Some(area(1001)), overlay)).len(), 2);
    assert_eq!(tiles.plan(&registry, &group(), None, plain), Plan::Pending(LayoutError::NoOutput));
    assert_eq!(tiles.plan(&registry, &group(), Some(area(2)), plain), Plan::Pending(LayoutError::InsufficientArea { index: 0 }));
    assert_eq!(tiles.members(), original);
    let resumed = ready(tiles.plan(&registry, &group(), Some(area(901)), plain));
    assert_eq!(resumed.iter().map(|cell| cell.target).collect::<Vec<_>>(), targets);
    assert_eq!(resumed.iter().map(|cell| cell.cell.outer.width).sum::<i32>(), 901);
    registry.set_workspace(targets[1].id, 2).unwrap();
    tiles.reconcile(&registry);
    assert_eq!(tiles.member(targets[1]).unwrap().group.workspace, 2);
    assert_eq!(ready(tiles.plan(&registry, &group(), Some(area(901)), plain)).len(), 2);
    registry.set_workspace(targets[1].id, 1).unwrap();
    tiles.reconcile(&registry);
    assert_eq!(tiles.members(), original);
}

#[test]
fn capacity_and_real_role_generations_bound_membership_and_retire_stale_requests() {
    let mut registry = Registry::new();
    let mut tiles = Tiles::default();
    for handle in 1..=MAX_MEMBERS as u32 {
        let target = window(&mut registry, handle);
        tiles.admit(&registry, member(target), Some(area(1024)), plain).unwrap();
    }
    let extra = window(&mut registry, 1000);
    assert_eq!(tiles.admit(&registry, member(extra), Some(area(1024)), plain), Err(AdmissionError::Capacity));
    let old = tiles.members()[0].target;
    registry.go_dormant(old.id).unwrap();
    let replacement = window(&mut registry, 1);
    assert_ne!(old.generation, replacement.generation);
    assert!(matches!(tiles.admit(&registry, member(old), Some(area(1024)), plain), Err(AdmissionError::Target(WindowTargetError::StaleTarget { .. }))));
    tiles.reconcile(&registry);
    assert!(tiles.member(old).is_none());
    assert!(tiles.member(replacement).is_none(), "new role is never auto-admitted");
    tiles.admit(&registry, member(extra), Some(area(1024)), plain).unwrap();
    registry.destroy(extra.id);
    tiles.reconcile(&registry);
    assert!(tiles.member(extra).is_none());
}

//! Workspaces on compd's real engine: windows are
//! stamped on the current workspace when they map, hidden off it, and the
//! commit counter the Bus-truth comparator uses as its frame-callback oracle
//! counts what a client commits. The policy itself is covered by policy's own
//! workspaces tests.

use policy::workspaces::{DefaultOutput, WorkspaceTarget};
use testkit::Harness;

fn harness() -> Harness {
    let mut h = Harness::new();
    h.wire.inner.comp.default_output = Some(DefaultOutput {
        key: "o_test".into(),
        name: "test".into(),
    });
    h
}

fn switch(h: &mut Harness, to: u32) -> (u32, u32) {
    let comp = &mut h.wire.inner.comp;
    let default = comp.default_output.clone();
    let (switched, _effects) = comp
        .workspaces
        .switch(&comp.registry, default.as_ref(), None, WorkspaceTarget::Index(to), false, None)
        .expect("a switch within the count");
    (switched.from, switched.to)
}

/// A window maps on the current workspace and is shown; a switch away hides
/// it; a window mapped meanwhile lands on the new current one; switching
/// back swaps which of the two is hidden.
#[test]
fn windows_join_the_current_workspace_and_hide_off_it() {
    let mut h = harness();
    let (first, _, _) = h.mapped_toplevel(64, 48);
    let first = h.handle_of(&first);
    assert_eq!(h.record(&first).unwrap().workspace(), Some(1));
    assert!(!h.comp().hidden(&first));

    assert_eq!(switch(&mut h, 2), (1, 2));
    assert_eq!(h.comp().current_workspace(), 2);
    assert!(h.comp().hidden(&first), "off the current workspace");
    assert!(h.record(&first).unwrap().mapped(), "hidden is not unmapped");

    let (second, _, _) = h.mapped_toplevel(32, 32);
    let second = h.handle_of(&second);
    assert_eq!(h.record(&second).unwrap().workspace(), Some(2));
    assert!(!h.comp().hidden(&second));

    assert_eq!(switch(&mut h, 1), (2, 1));
    assert!(!h.comp().hidden(&first));
    assert!(h.comp().hidden(&second));
}

/// Moving a window to another workspace hides it without switching.
#[test]
fn moving_a_window_hides_it_without_switching() {
    let mut h = harness();
    let (surface, _, _) = h.mapped_toplevel(64, 48);
    let handle = h.handle_of(&surface);
    let id = h.comp().registry.id_for_handle(&handle).unwrap();
    let comp = &mut h.wire.inner.comp;
    let default = comp.default_output.clone();
    let ((from, to), _effects) = comp
        .workspaces
        .move_window(&mut comp.registry, default.as_ref(), id, WorkspaceTarget::Index(3), None)
        .expect("a mapped toplevel moves");
    assert_eq!((from, to), (1, 3));
    assert_eq!(h.comp().current_workspace(), 1, "a move never switches");
    assert_eq!(h.record(&handle).unwrap().workspace(), Some(3));
    assert!(h.comp().hidden(&handle));
}

/// The commit counter counts buffer commits, one per drain that saw one.
#[test]
fn buffer_commits_are_counted_per_surface() {
    let mut h = harness();
    let (surface, _, _) = h.mapped_toplevel(64, 48);
    let handle = h.handle_of(&surface);
    let id = h.comp().registry.id_for_handle(&handle).unwrap();
    let before = h.comp().commits(id);
    assert!(before >= 1, "the mapping buffer was counted");
    h.client.attach(&surface, 64, 48);
    h.roundtrip();
    assert_eq!(h.comp().commits(id), before + 1);
    h.client.attach(&surface, 64, 48);
    h.roundtrip();
    assert_eq!(h.comp().commits(id), before + 2);
}

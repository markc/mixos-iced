// The snapshot tree and the read verbs over it. The read verbs are
// synchronous, so the single-flight test races two threads.

use super::*;
use crate::observation::{SetValidationError as SetError, validate_set_request};
use surfaces::{AGENT_SEAT_NAME, HUMAN_SEAT_NAME};

fn fixture() -> CompSnapshot {
    let output = "o_dp_1".to_string();
    let mut outputs = BTreeMap::new();
    outputs.insert(
        output.clone(),
        OutputSnapshot {
            name: "DP-1".into(),
            default: true,
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            scale: 1.0,
            refresh_mhz: 60_000,
            usable: RectSnapshot {
                x: 0.0,
                y: 30.0,
                width: 1920.0,
                height: 1050.0,
            },
            presentation: Some(OutputPresentationSnapshot {
                clock_id: 1,
                flags: Some(vec!["vsync"]),
                flags_mask: Some(1),
                refresh_us: None,
                frames: 3,
                interval_p50_us: Some(16_000),
                interval_p99_us: Some(17_000),
                since_us: 5,
            }),
        },
    );
    let layer = SurfaceSnapshot {
        occlusion: Default::default(),
        id: 1,
        role: "layer",
        mapped: true,
        visible: true,
        x: 0.0,
        y: 0.0,
        width: 1920.0,
        height: 30.0,
        band: "top",
        sequence: 2,
        tree_index: 0,
        parent: None,
        output: Some(output.clone()),
        title: None,
        app_id: None,
        focused: false,
        activated: false,
        maximized: false,
        fullscreen: false,
        minimized: false,
        workspace: None,
        decoration: None,
        layer: Some(LayerSnapshot {
            stratum: "top",
            interactivity: "exclusive",
            exclusive_zone: 30,
            binding: "explicit",
        }),
        foreign_id: None,
        generation: 3,
        window: WindowExtras::default(),
    };
    let toplevel = SurfaceSnapshot {
        occlusion: Default::default(),
        id: 2,
        role: "toplevel",
        mapped: true,
        visible: true,
        x: 40.0,
        y: 60.0,
        width: 800.0,
        height: 600.0,
        band: "normal",
        sequence: 1,
        tree_index: 0,
        parent: None,
        output: Some(output.clone()),
        title: Some(Arc::from("Terminal")),
        app_id: Some(Arc::from("org.example.Terminal")),
        focused: true,
        activated: true,
        maximized: false,
        fullscreen: false,
        minimized: false,
        workspace: Some(1),
        decoration: Some("server"),
        layer: None,
        foreign_id: Some("foreign-2".into()),
        generation: 4,
        window: WindowExtras {
            window_x: 52.0,
            window_y: 72.0,
            window_width: 776.0,
            window_height: 576.0,
            pid: Some(4242),
            workspace: 1,
        },
    };
    let mut surfaces = BTreeMap::new();
    surfaces.insert("s1".into(), layer);
    surfaces.insert("s2".into(), toplevel.clone());
    let mut popup = toplevel.clone();
    popup.id = 3;
    popup.role = "popup";
    popup.parent = Some(2);
    popup.title = None;
    popup.app_id = None;
    popup.focused = false;
    popup.activated = false;
    popup.decoration = None;
    popup.foreign_id = None;
    popup.workspace = None;
    surfaces.insert("s3".into(), popup);
    let mut subsurface = toplevel.clone();
    subsurface.id = 4;
    subsurface.role = "subsurface";
    subsurface.mapped = false;
    subsurface.visible = false;
    subsurface.parent = Some(2);
    subsurface.title = None;
    subsurface.app_id = None;
    subsurface.focused = false;
    subsurface.activated = false;
    subsurface.decoration = None;
    subsurface.foreign_id = None;
    subsurface.workspace = None;
    surfaces.insert("s4".into(), subsurface);
    let mut lock = toplevel.clone();
    lock.id = 5;
    lock.role = "lock";
    lock.band = "lock";
    lock.title = None;
    lock.app_id = None;
    lock.focused = false;
    lock.activated = false;
    lock.decoration = None;
    lock.foreign_id = None;
    lock.workspace = None;
    surfaces.insert("s5".into(), lock);
    let mut windows = BTreeMap::new();
    windows.insert(
        "s2".into(),
        WindowSnapshot {
            occlusion: Default::default(),
            id: toplevel.id,
            foreign_id: toplevel.foreign_id.clone(),
            title: toplevel.title.clone(),
            app_id: toplevel.app_id.clone(),
            x: toplevel.x,
            y: toplevel.y,
            width: toplevel.width,
            height: toplevel.height,
            focused: toplevel.focused,
            maximized: toplevel.maximized,
            fullscreen: toplevel.fullscreen,
            minimized: toplevel.minimized,
            output: toplevel.output.clone(),
            band: toplevel.band,
            generation: toplevel.generation,
            window_x: toplevel.window.window_x,
            window_y: toplevel.window.window_y,
            window_width: toplevel.window.window_width,
            window_height: toplevel.window.window_height,
            visible: toplevel.visible,
            pid: toplevel.window.pid,
            workspace: toplevel.window.workspace,
            presentation: Some(PresentationLeaves {
                presented: 3,
                interval_p50_us: Some(16_000),
                since_us: 5,
                ..PresentationLeaves::default()
            }),
        },
    );
    let mut workspace_outputs = BTreeMap::new();
    workspace_outputs.insert(output.clone(), OutputWorkspaceSnapshot { current: 1 });
    let workspaces = WorkspacesSnapshot {
        count: 2,
        current: 1,
        outputs: workspace_outputs,
        list: vec![
            WorkspaceRowSnapshot {
                index: 1,
                windows: 1,
            },
            WorkspaceRowSnapshot {
                index: 2,
                windows: 0,
            },
        ],
    };
    let mut sources = BTreeMap::new();
    sources.insert(
        "scene".to_string(),
        SourceSnapshot {
            output: None,
            registered_at_us: 7,
            revision: 4,
            registration: 1,
            presentation: SourcePresentationLeaves {
                upload_bytes_total: 640,
                ..SourcePresentationLeaves::default()
            },
        },
    );
    CompSnapshot {
        occlusion: Default::default(),
        info: InfoSnapshot {
            service: Arc::from("comp-nested"),
            version: Arc::from("0.37.0"),
            backend: "nested",
            engine: "bevy-0.19/wgpu",
            instance: Arc::from("fixture"),
            explicit_sync_advertised: false,
            explicit_sync_healthy: true,
        },
        outputs,
        surfaces,
        windows,
        workspaces,
        sources,
        stack: vec![1, 2],
        focus: FocusSnapshot {
            keyboard: Some(2),
            exclusive_latch: Some(1),
            pointer: Some(2),
            pointer_grab: "none",
            session_lock: "none",
            window: FocusWindowSnapshot {
                id: Some(2),
                generation: Some(4),
            },
        },
        decoration: DecorationSnapshot {
            enabled: true,
            style: "mac",
        },
        bindings: BindingsSnapshot {
            enabled: true,
            profile: "nested",
            table: vec![BindingRowSnapshot {
                chord: "Super+Q".into(),
                action: "close-focused",
            }],
        },
        input: InputSnapshot {
            seats: Some(BTreeMap::from([
                (
                    "human",
                    SeatSnapshot {
                        name: HUMAN_SEAT_NAME,
                        keyboard_focus: Some(SeatFocusSnapshot {
                            id: 2,
                            generation: 1,
                        }),
                        pointer_focus: Some(SeatFocusSnapshot {
                            id: 2,
                            generation: 1,
                        }),
                        pointer: Some(SeatPointerSnapshot {
                            output: "DP-1".into(),
                            x: 10.0,
                            y: 20.0,
                        }),
                        last_input_us: Some(42),
                    },
                ),
                (
                    "agent",
                    SeatSnapshot {
                        name: AGENT_SEAT_NAME,
                        keyboard_focus: None,
                        pointer_focus: None,
                        pointer: None,
                        last_input_us: None,
                    },
                ),
            ])),
            last_origin: Some("human"),
            // A read snapshot, with the volatile holder-plane counts.
            corners: CornersSnapshot {
                enforced: Some(EdgeCounts {
                    left: 1,
                    ..EdgeCounts::default()
                }),
                held: Some(EdgeCounts::default()),
                ..CornersSnapshot::from(CornerConfig::default())
            },
            host: Some(HostInputSnapshot { passthrough: true }),
        },
        #[cfg(feature = "xwayland")]
        xwayland: XwaylandSnapshot {
            enabled: true,
            persist_path: Arc::from("/tmp/fixture/etc/comp/xwayland-enabled.comp-nested"),
            display: Some(Arc::from(":3")),
            state: "ready",
            failures: 1,
        },
        // A read snapshot, with the volatile import ledger.
        dmabuf: DmabufLedgerSnapshot {
            accepted: 5,
            failed: 1,
            failures: vec![DmabufFailureRecord {
                format: "AR24".into(),
                modifier: "0x0000000000000000".into(),
                reason: "vulkan_rejected",
                detail: "fixture".into(),
                at_us: 9,
            }],
        },
        port: PortSnapshot {
            level: "L2",
            event_seq: 0,
            lost_count: 0,
            queue_depth: 1,
            reply_timeouts: 0,
            publish_timeouts: 0,
            slug_collisions: 0,
            broker: "connected",
        },
        full_tree: FullTreeCache::default(),
    }
}

#[test]
fn corner_hold_property_is_absent_from_snapshot_and_schema() {
    let snapshot = fixture();
    assert_eq!(snapshot.select(&["input", "corners", "hold_ms"]), None);
    assert_eq!(snapshot.node_kind(&["input", "corners", "hold_ms"]), None);
    let corners = snapshot.select(&["input", "corners"]).unwrap();
    assert!(corners.get("hold_ms").is_none());
    assert!(describe(&snapshot, &PropPath::new("input.corners.hold_ms").unwrap()).is_none());
}

#[test]
fn mutable_descriptors_match_the_writable_leaves() {
    let snapshot = fixture();
    let mutable = DESCRIPTORS
        .iter()
        .filter(|descriptor| descriptor.mutable)
        .collect::<Vec<_>>();
    // The corner leaves and the window band are process-lifetime
    // (persistence "none"); `xwayland.enabled` is deliberately the
    // surface's ONE file-persisted mutable leaf (startup-read — a
    // non-persisted startup switch would be unreachable from its own
    // surface).
    // 0.59.0 adds the four workspace leaves: the window's workspace,
    // the count, and the current workspace by default output and by
    // output key.
    // Chunk 19 adds the two affordance leaves, `input.corners.affordance`
    // and `input.corners.discovery`.
    #[cfg(feature = "xwayland")]
    assert_eq!(mutable.len(), 16);
    #[cfg(not(feature = "xwayland"))]
    assert_eq!(mutable.len(), 15);
    for path in [
        "input.corners.enabled",
        "input.corners.deadzone_px",
        "input.corners.dwell_ms",
        "input.corners.velocity_max_px_s",
        "input.corners.affordance",
        "input.corners.discovery",
        "input.host.passthrough",
        "windows.s2.band",
        "windows.s2.minimized",
        "windows.s2.maximized",
        "windows.s2.fullscreen",
        "windows.s2.workspace",
        "workspaces.count",
        "workspaces.current",
        "workspaces.o_dp_1.current",
    ] {
        let path = PropPath::new(path).unwrap();
        let body = describe(&snapshot, &path).expect("mutable descriptor");
        let body = serde_json::from_str::<Value>(&body).unwrap();
        assert_eq!(body["mutable"], true);
        assert_eq!(body["persistence"], "none");
    }
    #[cfg(feature = "xwayland")]
    {
        let body = describe(&snapshot, &PropPath::new("xwayland.enabled").unwrap())
            .expect("xwayland.enabled descriptor");
        let body = serde_json::from_str::<Value>(&body).unwrap();
        assert_eq!(body["mutable"], true);
        assert_eq!(body["persistence"], "file");
        assert_eq!(body["type"], "bool");
    }
    let dwell = describe(&snapshot, &PropPath::new("input.corners.dwell_ms").unwrap()).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&dwell).unwrap()["range"],
        "0..=5000"
    );
    // The workspace rows and per-output object are read-only; the
    // per-output current is a leaf under an object keyed like outputs.
    let list = describe(&snapshot, &PropPath::new("workspaces.list").unwrap()).unwrap();
    let list = serde_json::from_str::<Value>(&list).unwrap();
    assert_eq!(list["mutable"], false);
    assert_eq!(list["type"], "list");
    assert_eq!(
        snapshot.select(&["workspaces", "list"]),
        Some(json!([{"index": 1, "windows": 1}, {"index": 2, "windows": 0}]))
    );
    assert_eq!(
        snapshot.select(&["workspaces", "o_dp_1", "current"]),
        Some(json!(1))
    );
    assert_eq!(
        snapshot.node_kind(&["workspaces", "o_dp_1"]),
        Some(SnapshotNodeKind::Object)
    );
    assert_eq!(snapshot.node_kind(&["workspaces", "o_nope"]), None);
    assert_eq!(
        snapshot.select(&["surfaces", "s3", "workspace"]),
        Some(Value::Null)
    );
    assert_eq!(
        snapshot.select(&["windows", "s2", "workspace"]),
        Some(json!(1))
    );
}

#[test]
fn capability_leaf_reflects_holder_plane() {
    let snapshot = fixture();
    assert_eq!(
        snapshot.select(&["input", "corners", "holders"]),
        Some(json!(HOLDER_PLANE_AVAILABLE))
    );
    // Quoin goes command-driven on this leaf: it is true only because
    // holder tracking, the conceal timer, enforcement on a stalled shell,
    // disconnect cleanup and resynchronisation all exist (chunk 15).
    assert_eq!(
        snapshot.select(&["input", "corners", "holders"]),
        Some(json!(true))
    );
    // The enforcement and hold counts are read-only, volatile leaves.
    assert_eq!(
        snapshot.select(&["input", "corners", "enforced", "left"]),
        Some(json!(1))
    );
    for path in ["input.corners.enforced.left", "input.corners.held.top"] {
        let descriptor: Value =
            serde_json::from_str(&describe(&snapshot, &PropPath::new(path).unwrap()).unwrap())
                .unwrap();
        assert_eq!(descriptor["mutable"], false, "{path}");
        assert_eq!(descriptor["volatile"], true, "{path}");
        assert!(
            matches!(
                validate_set_request(path, &json!(0)),
                Err(SetError::ReadOnly)
            ),
            "{path}"
        );
    }
    let path = PropPath::new("input.corners.holders").unwrap();
    let descriptor: Value = serde_json::from_str(&describe(&snapshot, &path).unwrap()).unwrap();
    assert_eq!(descriptor["mutable"], false);
    assert_eq!(descriptor["type"], "bool");
    assert!(matches!(
        validate_set_request("input.corners.holders", &json!(false)),
        Err(SetError::ReadOnly)
    ));
}

#[test]
fn descriptor_table_and_serialised_fixture_have_exact_parity() {
    let tree = serde_json::to_value(fixture()).expect("fixture serialises");
    let leaves = flattened_paths(&tree);
    for leaf in &leaves {
        let matches = DESCRIPTORS
            .iter()
            .filter(|entry| entry.matches(leaf))
            .count();
        assert_eq!(matches, 1, "descriptor count for {}", leaf.as_str());
    }
    for descriptor in DESCRIPTORS {
        assert!(
            leaves.iter().any(|leaf| descriptor.matches(leaf)),
            "descriptor has no fixture leaf: {:?}",
            descriptor.pattern
        );
    }
}

/// S14: the descriptor table's `volatile` flag and the path rule
/// `props.changed` filters on never disagree.
#[test]
fn descriptor_volatility_matches_the_path_rule() {
    let snapshot = fixture();
    assert_eq!(
        snapshot.select(&["occlusion", "counters", "resumes"]),
        Some(serde_json::json!(0))
    );
    assert!(
        snapshot
            .select(&["surfaces", "s1", "occlusion_counters"])
            .is_none()
    );
    let mut volatile = 0;
    for descriptor in DESCRIPTORS {
        let path = descriptor
            .pattern
            .iter()
            .map(|segment| match segment {
                PatternSegment::Literal(literal) => *literal,
                PatternSegment::OutputKey => "o_dp_1",
                PatternSegment::SurfaceKey => "s2",
                PatternSegment::SourceKey => "scene",
                PatternSegment::SeatKey => "human",
            })
            .collect::<Vec<_>>()
            .join(".");
        assert_eq!(descriptor.volatile, volatile_path(&path), "{path}");
        volatile += usize::from(descriptor.volatile);
    }
    // + 8: the four `input.corners.enforced.*` and four `held.*` counts.
    // + 3: the `dmabuf.*` import ledger.
    // + 13: per-seat observations and last input origin.
    assert_eq!(volatile, 13 + 8 + 4 + 19 + 4 + 8 + 3 + 13);
}

#[test]
fn scopes_reach_ancestors_and_descendants_only() {
    let scopes = ReadScopes::Paths(vec!["windows.s2".into(), "outputs".into()]);
    assert!(scopes.wants("windows.s2.presentation"));
    assert!(scopes.wants("outputs.o_dp_1.presentation"));
    assert!(!scopes.wants("windows.s20.presentation"));
    assert!(!scopes.wants("sources"));
    assert!(ReadScopes::Paths(vec!["sources.scene.revision".into()]).wants("sources"));
    let mut merged = ReadScopes::Paths(Vec::new());
    assert!(!merged.wants("sources"));
    merged.add(Some("info"));
    assert!(!merged.wants("sources"));
    merged.add(None);
    assert_eq!(merged, ReadScopes::All);
}

#[test]
fn flag_names_follow_the_kind_bits() {
    assert_eq!(presentation_flag_names(0), Vec::<&str>::new());
    assert_eq!(
        presentation_flag_names(0x7),
        ["vsync", "hw_clock", "hw_completion"]
    );
    assert_eq!(presentation_flag_names(0x8), ["zero_copy"]);
}

#[test]
fn dmabuf_import_ledger_is_served_read_only_and_volatile() {
    let snapshot = fixture();
    let describe_json = |path: &str| {
        let body = describe(&snapshot, &PropPath::new(path).unwrap())
            .unwrap_or_else(|| panic!("describe {path}"));
        serde_json::from_str::<Value>(&body).unwrap()
    };
    for path in ["dmabuf.accepted", "dmabuf.failed", "dmabuf.failures"] {
        let body = describe_json(path);
        assert_eq!(body["volatile"], true, "{path}");
        assert_eq!(body["mutable"], false, "{path}");
        assert!(volatile_path(path), "{path}");
    }
    assert_eq!(describe_json("dmabuf.failures")["type"], "list");
    assert_eq!(snapshot.select(&["dmabuf", "accepted"]), Some(json!(5)));
    assert_eq!(snapshot.select(&["dmabuf", "failed"]), Some(json!(1)));
    assert_eq!(
        snapshot.select(&["dmabuf", "failures"]),
        Some(json!([{
            "format": "AR24",
            "modifier": "0x0000000000000000",
            "reason": "vulkan_rejected",
            "detail": "fixture",
            "at_us": 9
        }]))
    );
    let leaves = snapshot
        .leaf_paths()
        .into_iter()
        .map(|path| path.as_str().to_owned())
        .collect::<Vec<_>>();
    for path in ["dmabuf.accepted", "dmabuf.failed", "dmabuf.failures"] {
        assert!(leaves.iter().any(|leaf| leaf == path), "{path} listed");
    }
}

#[test]
fn presentation_and_source_leaves_are_described_as_volatile() {
    let snapshot = fixture();
    let describe_json = |path: &str| {
        let body = describe(&snapshot, &PropPath::new(path).unwrap())
            .unwrap_or_else(|| panic!("describe {path}"));
        serde_json::from_str::<Value>(&body).unwrap()
    };
    for path in [
        "windows.s2.presentation.presented",
        "windows.s2.presentation.missed",
        "outputs.o_dp_1.presentation.frames",
        "sources.scene.revision",
        "sources.scene.output",
        "sources.scene.presentation.upload_bytes_p99",
    ] {
        let body = describe_json(path);
        assert_eq!(body["volatile"], true, "{path}");
        assert_eq!(body["mutable"], false, "{path}");
        assert!(volatile_path(path), "{path}");
    }
    for path in ["windows.s2.presentation", "sources.scene", "sources"] {
        let body = describe_json(path);
        assert_eq!(body["type"], "object");
        assert_eq!(body["volatile"], true, "{path}");
    }
    assert!(
        describe_json("windows.s2.presentation")["children"]
            .as_array()
            .unwrap()
            .contains(&json!("windows.s2.presentation.since_us"))
    );
    for path in ["windows.s2.title", "outputs.o_dp_1", "windows"] {
        assert!(describe_json(path).get("volatile").is_none(), "{path}");
        assert!(!volatile_path(path), "{path}");
    }
    assert_eq!(
        snapshot.select(&["sources", "scene", "presentation", "upload_bytes_total"]),
        Some(json!(640))
    );
    assert_eq!(
        snapshot.select(&["windows", "s2", "presentation", "missed"]),
        Some(Value::Null),
        "unmeasured missed is null"
    );
    assert_eq!(
        snapshot.select(&["outputs", "o_dp_1", "presentation", "clock_id"]),
        Some(json!(1))
    );
    assert_eq!(snapshot.select(&["sources", "scene", "nope"]), None);
    assert_eq!(
        snapshot.select(&["windows", "s2", "presentation", "presented", "x"]),
        None
    );
}

#[test]
fn list_uses_segment_ancestry_and_every_leaf_round_trips() {
    let snapshot = fixture();
    let tree = serde_json::to_value(&snapshot).expect("fixture serialises");
    let leaves = flattened_paths(&tree);
    let prefix = PropPath::new("surfaces.s1").expect("valid prefix");
    let expected = leaves
        .iter()
        .filter(|leaf| leaf.starts_with(&prefix))
        .map(|leaf| leaf.as_str().to_string())
        .collect::<Vec<_>>();
    let (rc, body) = dispatch_read(
        &snapshot,
        "comp.props.list",
        &json!({"prefix": "surfaces.s1"}),
    );
    assert_eq!(rc, 0);
    assert_eq!(
        serde_json::from_str::<Vec<String>>(&body).expect("path list"),
        expected
    );
    let (rc, body) = dispatch_read(
        &snapshot,
        "comp.props.list",
        &json!({"prefix": "surfaces.s"}),
    );
    assert_eq!(rc, 0);
    assert_eq!(body.as_ref(), "[]");

    for leaf in leaves {
        let args = json!({"path": leaf.as_str()});
        assert_eq!(dispatch_read(&snapshot, "comp.props.get", &args).0, 0);
        assert_eq!(
            dispatch_read(&snapshot, "comp.props.describe", &args).0,
            0,
            "describe {}",
            leaf.as_str()
        );
    }
}

#[test]
fn typed_leaf_selection_size_is_independent_of_surface_count() {
    let mut snapshot = fixture();
    let selected = snapshot
        .select(&["surfaces", "s2", "title"])
        .expect("fixture leaf exists");
    let selected_size = selected.to_string().len();
    assert!(selected.is_string());
    assert!(selected_size < 64);

    let template = snapshot
        .surfaces
        .get("s2")
        .cloned()
        .expect("fixture surface exists");
    for id in 100_u64..1_100 {
        let mut surface = template.clone();
        surface.id = id;
        snapshot.surfaces.insert(format!("s{id}"), surface);
    }

    let selected_with_many_surfaces = snapshot
        .select(&["surfaces", "s2", "title"])
        .expect("fixture leaf still exists");
    assert!(selected_with_many_surfaces.is_string());
    assert_eq!(selected_with_many_surfaces.to_string().len(), selected_size);
    assert!(!snapshot.full_tree.is_filled());
}

#[test]
fn output_keys_obey_the_public_slug_encoding() {
    assert_eq!(output_key("compd-nested-0"), "o_compd_nested_0");
    assert_eq!(output_key("DP-1"), "o_dp_1");
}

#[test]
fn output_slug_collision_keeps_first_and_counts_dropped_output() {
    let snapshot = fixture();
    let mut outputs = snapshot.outputs;
    let mut collisions = 0;

    assert!(output_slug_collides(
        &outputs,
        "o_dp_1",
        "DP_1",
        &mut collisions,
    ));
    assert_eq!(collisions, 1);
    assert_eq!(
        outputs
            .remove("o_dp_1")
            .expect("first output retained")
            .name,
        "DP-1"
    );
}

#[test]
fn describe_accepts_empty_collection_subtrees() {
    let mut snapshot = fixture();
    snapshot.surfaces.clear();
    snapshot.windows.clear();
    for path in ["surfaces", "windows"] {
        let (rc, body) = dispatch_read(&snapshot, "comp.props.describe", &json!({"path": path}));
        assert_eq!(rc, 0, "{path}: {body}");
        assert_eq!(
            serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|value| value.get("children").cloned()),
            Some(json!([])),
            "{path}"
        );
    }
}

#[test]
fn full_tree_serialisation_is_single_flight_and_shares_the_cached_bytes() {
    let snapshot = Arc::new(fixture());
    let reader = |args: Value| {
        let snapshot = Arc::clone(&snapshot);
        std::thread::spawn(move || dispatch_read(&snapshot, "comp.props.get", &args))
    };
    let left = reader(Value::Null);
    let right = reader(json!({}));
    let (left_rc, left_body) = left.join().expect("left reader");
    let (right_rc, right_body) = right.join().expect("right reader");
    assert_eq!((left_rc, right_rc), (0, 0));
    assert!(Arc::ptr_eq(&left_body, &right_body));
    assert!(snapshot.full_tree.is_filled());
    // A clone is a new snapshot: it may be edited, so it shares no cache.
    assert!(!snapshot.as_ref().clone().full_tree.is_filled());

    let (rc, selected) = dispatch_read(
        &snapshot,
        "comp.props.get",
        &json!({"path": "info.service"}),
    );
    assert_eq!(rc, 0);
    assert_eq!(selected.as_ref(), "\"comp-nested\"");
}

#[test]
fn oversized_full_tree_returns_too_large_while_leaf_read_succeeds() {
    let mut snapshot = fixture();
    let template = snapshot
        .surfaces
        .get("s2")
        .cloned()
        .expect("fixture toplevel");
    for id in 100_u64..140 {
        let mut surface = template.clone();
        surface.id = id;
        snapshot.surfaces.insert(format!("s{id}"), surface);
    }
    let injected_limit = 1_024;

    let (rc, body) =
        dispatch_read_with_limit(&snapshot, "comp.props.get", &Value::Null, injected_limit);
    assert_eq!(rc, 10);
    assert_eq!(
        serde_json::from_str::<Value>(&body).expect("too_large JSON"),
        json!({
            "error": "too_large",
            "limit_bytes": injected_limit,
            "hint": "read a subtree",
        })
    );
    assert!(
        snapshot
            .full_tree
            .0
            .get()
            .is_some_and(|reply| reply.bytes > injected_limit),
        "cached full-tree bytes are measured once"
    );

    let (rc, leaf) = dispatch_read_with_limit(
        &snapshot,
        "comp.props.get",
        &json!({"path": "info.service"}),
        injected_limit,
    );
    assert_eq!(rc, 0);
    assert_eq!(leaf.as_ref(), "\"comp-nested\"");
}

// The two read verbs, on the model alone.
#[test]
fn info_and_windows_list_bodies_are_comps() {
    let snapshot = fixture();
    let (rc, body) = dispatch_read(&snapshot, "comp.info", &Value::Null);
    assert_eq!(rc, 0);
    assert_eq!(
        body.as_ref(),
        r#"{"backend":"nested","engine":"bevy-0.19/wgpu","event_seq":0,"instance":"fixture","lost_count":0,"output_count":1,"service":"comp-nested","surface_count":5,"version":"0.37.0"}"#
    );
    let info: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(info["instance"], snapshot.info.instance.as_ref());
    let mut replacement = fixture();
    replacement.info.instance = Arc::from("replacement-instance");
    let (rc, replacement_body) = dispatch_read(&replacement, "comp.info", &Value::Null);
    assert_eq!(rc, 0);
    let replacement_info: Value = serde_json::from_str(&replacement_body).unwrap();
    assert_eq!(replacement_info["instance"], "replacement-instance");
    assert_ne!(info["instance"], replacement_info["instance"]);
    let list = |args: Value| {
        let (rc, body) = dispatch_read(&snapshot, "comp.windows.list", &args);
        (rc, serde_json::from_str::<Value>(&body).unwrap())
    };
    let (rc, body) = list(json!({"app_id": "org.example.Terminal", "workspace": "current"}));
    assert_eq!(rc, 0);
    assert_eq!(body["windows"].as_array().unwrap().len(), 1);
    assert_eq!(body["windows"][0]["id"], 2);
    assert_eq!(body["windows"][0]["generation"], 4);
    assert_eq!(
        list(json!({"title_contains": "Term", "visible": true})).1["windows"][0]["id"],
        2
    );
    assert_eq!(list(json!({"visible": false})).1, json!({"windows": []}));
    assert_eq!(list(json!({"workspace": 2})).1, json!({"windows": []}));
    assert_eq!(
        list(json!({"workspace": "all"})).1["windows"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let (rc, body) = list(json!({"workspace": 3}));
    assert_eq!(rc, 10);
    assert_eq!(body["error"], "invalid_value");
    assert_eq!(body["path"], "workspace");
    assert_eq!(body["range"], "1..=count|current|all");
    let (rc, body) = list(json!({"appid": "x"}));
    assert_eq!(rc, 10);
    assert_eq!(
        body,
        json!({"error": "invalid_args", "field": "appid", "allowed": ["app_id", "title", "title_contains", "visible", "workspace"]})
    );
    assert_eq!(
        dispatch_read(&snapshot, "comp.props.set", &Value::Null),
        crate::reply::error("unknown_verb")
    );
    assert_eq!(
        dispatch_read(&snapshot, "comp.props.get", &json!({"path": "Bad Path"})),
        crate::reply::error("unknown_path")
    );
}

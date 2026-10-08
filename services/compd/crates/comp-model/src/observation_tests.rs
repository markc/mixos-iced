// The set-request gate, panel request refusals, publisher loss, the property
// reducer, the topic wire and corner validation, rfc3339, the gap message and
// the topic bodies. `wire()` is a TopicMessage (`get` on headers); the
// event-sequence exhaustion test drives `EventSeq` directly.

use super::*;
use crate::snapshot::RectSnapshot;

#[test]
fn window_band_path_parses_only_the_exact_writable_shape() {
    assert_eq!(parse_window_band_path("windows.s12.band"), Some(12));
    assert_eq!(parse_window_band_path("windows.s0.band"), Some(0));
    assert_eq!(parse_window_band_path("windows.s.band"), None);
    assert_eq!(
        parse_window_band_path("windows.s0007.band"),
        None,
        "leading zeros would alias the canonical s7 key"
    );
    assert_eq!(parse_window_band_path("windows.s01.band"), None);
    assert_eq!(parse_window_band_path("windows.s12.title"), None);
    assert_eq!(parse_window_band_path("windows.s1x2.band"), None);
    assert_eq!(parse_window_band_path("windows.12.band"), None);
    assert_eq!(parse_window_band_path("surfaces.s12.band"), None);
    assert_eq!(parse_window_band_path("windows.s12.band.extra"), None);
}

#[test]
fn window_band_value_accepts_exactly_the_operator_bands() {
    assert_eq!(
        validate_window_band_value("windows.s1.band", &json!("bottom")),
        Ok(StackBand::Bottom)
    );
    assert_eq!(
        validate_window_band_value("windows.s1.band", &json!("normal")),
        Ok(StackBand::Normal)
    );
    for refused in [json!("top"), json!("overlay"), json!("lock"), json!(1)] {
        assert!(matches!(
            validate_window_band_value("windows.s1.band", &refused),
            Err(SetValidationError::InvalidValue { .. })
        ));
    }
}

#[test]
fn ingress_gate_admits_every_writable_leaf_family() {
    for leaf in ["minimized", "maximized", "fullscreen"] {
        let path = format!("windows.s7.{leaf}");
        for value in [json!(true), json!(false)] {
            assert!(validate_set_request(&path, &value).is_ok());
        }
        for value in [json!(0), json!("true"), Value::Null] {
            assert!(matches!(
                validate_set_request(&path, &value),
                Err(SetValidationError::InvalidValue { .. })
            ));
        }
    }
    assert!(validate_set_request("input.corners.enabled", &json!(true)).is_ok());
    assert!(validate_set_request("windows.s7.band", &json!("bottom")).is_ok());
    // Other window leaves stay read-only, unknown stays unknown.
    assert!(matches!(
        validate_set_request("windows.s7.title", &json!("x")),
        Err(SetValidationError::ReadOnly)
    ));
    assert!(matches!(
        validate_set_request("nonsense.path", &json!(true)),
        Err(SetValidationError::UnknownPath)
    ));
    // 0.59.0: the four workspace leaves admit an integer >= 1 (the
    // count/live bound is the service's), refuse 0 and non-integers
    // with invalid_value naming the range, and the row list and the
    // subtree object are read-only.
    for path in [
        "windows.s7.workspace",
        "workspaces.count",
        "workspaces.current",
        "workspaces.o_dp_1.current",
    ] {
        assert!(validate_set_request(path, &json!(2)).is_ok(), "{path}");
        for refused in [json!(0), json!("2"), json!(2.5), json!(-1), json!(true)] {
            assert!(
                matches!(
                    validate_set_request(path, &refused),
                    Err(SetValidationError::InvalidValue {
                        expected: "integer",
                        ..
                    })
                ),
                "{path} refuses {refused}"
            );
        }
    }
    assert!(matches!(
        validate_set_request("workspaces.count", &json!(0)),
        Err(SetValidationError::InvalidValue {
            range: "1..=16",
            ..
        })
    ));
    assert!(matches!(
        validate_set_request("workspaces.current", &json!(0)),
        Err(SetValidationError::InvalidValue {
            range: "1..=count",
            ..
        })
    ));
    for path in ["workspaces.list", "workspaces"] {
        assert!(
            matches!(
                validate_set_request(path, &json!([])),
                Err(SetValidationError::ReadOnly)
            ),
            "{path}"
        );
    }
    assert!(matches!(
        validate_set_request("workspaces.o_dp_1", &json!(1)),
        Err(SetValidationError::UnknownPath)
    ));
    assert_eq!(
        parse_workspaces_set_path("workspaces.o_dp_1.current"),
        Some(WorkspacesSetTarget::Current(Some("o_dp_1".into())))
    );
    assert_eq!(parse_workspaces_set_path("workspaces.o_.current"), None);
    assert_eq!(parse_workspaces_set_path("workspaces.o_a.b.current"), None);
    assert_eq!(parse_workspaces_set_path("workspaces.dp_1.current"), None);
}

/// The regression this gate refactor fixed: `xwayland.enabled` had a
/// complete service arm that the wire-level gate could never reach,
/// because the old ingress validation only knew corner paths.
#[cfg(feature = "xwayland")]
#[test]
fn ingress_gate_admits_the_xwayland_leaf_it_previously_rejected() {
    assert!(validate_set_request("xwayland.enabled", &json!(false)).is_ok());
    assert!(matches!(
        validate_set_request("xwayland.enabled", &json!("nope")),
        Err(SetValidationError::InvalidValue { .. })
    ));
    // The four served, never-written leaves exist, so a write is
    // `read_only` (like `workspaces.list` and every `surfaces.*`
    // leaf), not `unknown_path`.
    for path in [
        "xwayland",
        "xwayland.persist_path",
        "xwayland.display",
        "xwayland.state",
        "xwayland.failures",
    ] {
        assert!(
            matches!(
                validate_set_request(path, &json!(":9")),
                Err(SetValidationError::ReadOnly)
            ),
            "{path}"
        );
    }
    assert!(matches!(
        validate_set_request("xwayland.nope", &json!(1)),
        Err(SetValidationError::UnknownPath)
    ));
}

#[test]
fn panel_request_refusals_name_the_offending_argument() {
    let base = json!({"output":"DP-1","edge":"left","surface":"quoin-panel-1"});
    let with = |extra: Value| {
        let mut args = base.clone();
        args.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        args
    };
    for (verb, args, field) in [
        (
            "comp.panel.hold",
            with(json!({"holder":"typo","acquire":true})),
            "holder",
        ),
        (
            "comp.panel.hold",
            with(json!({"holder":"popup"})),
            "acquire",
        ),
        (
            "comp.panel.hold",
            with(json!({"holder":"popup","acquire":true,"mode":"hidden"})),
            "mode",
        ),
        (
            "comp.panel.hold",
            with(json!({"holder":"popup","acquire":true,"sticky":true})),
            "sticky",
        ),
        ("comp.panel.mode", with(json!({"mode":"revealed"})), "mode"),
        (
            "comp.panel.mode",
            with(json!({"mode":"hidden","edge":"middle"})),
            "edge",
        ),
        (
            "comp.panel.mode",
            with(json!({"mode":"hidden","surface":""})),
            "surface",
        ),
        (
            "comp.panel.mode",
            with(json!({"mode":"hidden","acquire":false})),
            "acquire",
        ),
        ("comp.panel.mode", json!([1]), "args"),
        (
            "comp.panel.hold",
            with(json!({"output":7,"holder":"popup","acquire":true})),
            "output",
        ),
        (
            "comp.panel.hold",
            with(json!({"holder":"popup","acquire":"yes"})),
            "acquire",
        ),
        (
            "comp.panel.hold",
            with(json!({"holder":["popup"],"acquire":true})),
            "holder",
        ),
        (
            "comp.panel.mode",
            json!({"output":"DP-1","edge":"left","mode":"hidden"}),
            "surface",
        ),
        (
            "comp.panel.mode",
            json!({"output":"DP-1","surface":"s","mode":"hidden"}),
            "edge",
        ),
    ] {
        let Err(reply) = PanelRequest::parse(verb, &args) else {
            panic!("{verb} {args} must be refused");
        };
        let (rc, body) = reply.into_wire();
        let body: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(rc, 10);
        assert_eq!(body["error"], "invalid_args");
        assert_eq!(body["field"], field, "{verb} {args}");
        assert_eq!(body["allowed"], json!(PANEL_ARGS));
    }
}

/// A hidden panel as Quoin reports it, with the pointer engaged on its

#[test]
fn publisher_loss_dominates_interval_merges_in_both_orders() {
    let record = ObservationRecord::FocusChanged {
        keyboard: Some(1),
        previous: None,
        exclusive_latch: None,
        event_seq: 1,
    };
    let overflow = LossInterval::from_record(&record, LossCause::OutboxOverflow);
    let publisher = LossInterval::from_record(&record, LossCause::PublisherLoss);

    let mut overflow_then_publisher = overflow;
    overflow_then_publisher.merge(publisher);
    let mut publisher_then_overflow = publisher;
    publisher_then_overflow.merge(overflow);
    assert_eq!(overflow_then_publisher.cause, LossCause::PublisherLoss);
    assert_eq!(publisher_then_overflow.cause, LossCause::PublisherLoss);
}

#[test]
fn property_reducer_coalesces_each_path_and_excludes_operational_leaves() {
    let mut pending = PendingPropChanges::new();
    queue_prop_change(
        &mut pending,
        "surfaces.s2.title".into(),
        prop_str("old"),
        prop_str("middle"),
        "wayland.map",
    );
    queue_prop_change(
        &mut pending,
        "surfaces.s2.title".into(),
        prop_str("middle"),
        prop_str("new"),
        "wayland.focus",
    );
    queue_prop_change(
        &mut pending,
        "port.event_seq".into(),
        PropValue::U64(1),
        PropValue::U64(2),
        "wayland.map",
    );
    for volatile in [
        "windows.s2.presentation.presented",
        "outputs.o_dp_1.presentation.frames",
        "sources.scene.revision",
        "sources.scene.presentation.presented",
    ] {
        queue_prop_change(
            &mut pending,
            volatile.into(),
            PropValue::U64(1),
            PropValue::U64(2),
            "frame",
        );
    }
    assert_eq!(
        pending.remove("surfaces.s2.title"),
        Some((prop_str("old"), prop_str("new"), "wayland.map"))
    );
    assert!(pending.is_empty());

    queue_prop_change(
        &mut pending,
        "focus.keyboard".into(),
        PropValue::null(),
        PropValue::U64(2),
        "wayland.focus",
    );
    queue_prop_change(
        &mut pending,
        "focus.keyboard".into(),
        PropValue::U64(2),
        PropValue::null(),
        "wayland.focus",
    );
    assert!(pending.is_empty());
}

#[test]
fn exact_topic_commands_and_flat_bodies() {
    let record = ObservationRecord::SurfaceMapped {
        id: 7,
        role: "toplevel".into(),
        foreign_id: Some("f_7".into()),
        window: SurfaceEdgeWindow {
            generation: 3,
            app_id: Some("dev.mixos.Probe".into()),
            title: None,
        },
        event_seq: 9,
    };
    let wire = record.wire();
    assert_eq!(record.topic_suffix(), SURFACE_MAPPED_TOPIC_SUFFIX);
    assert_eq!(wire.get("command"), Some(SURFACE_MAPPED_TOPIC_SUFFIX));
    assert_eq!(
        topic_name("comp-nested", record.topic_suffix()),
        "comp-nested.surface.mapped"
    );
    assert_eq!(
        serde_json::from_str::<Value>(&wire.body).unwrap(),
        json!({
            "id": 7,
            "role": "toplevel",
            "generation": 3,
            "app_id": "dev.mixos.Probe",
            "title": null,
            "foreign_id": "f_7",
            "event_seq": 9,
        })
    );
}

#[test]
fn corner_clicked_v2_wire_has_press_modifiers_even_when_empty() {
    for (button, modifiers) in [
        ("left", vec![]),
        ("left", vec!["shift", "ctrl", "alt", "super"]),
        ("right", vec![]),
    ] {
        let record = ObservationRecord::CornerClickedV2 {
            output: "o_nested".into(),
            corner: Corner::BottomRight,
            dwell_ms: 217,
            button,
            kind: "brief",
            modifiers: modifiers.clone(),
            event_seq: 42,
        };
        let wire = record.wire();
        assert_eq!(record.topic_suffix(), "corner.clicked.v2");
        assert_eq!(wire.get("command"), Some("corner.clicked.v2"));
        assert_eq!(wire.get("event_seq"), Some("42"));
        assert_eq!(
            serde_json::from_str::<Value>(&wire.body).unwrap(),
            json!({
                "output": "o_nested", "corner": "br", "dwell_ms": 217,
                "button": button, "kind": "brief", "modifiers": modifiers,
                "event_seq": 42,
            })
        );
    }
}

#[test]
fn corner_clicked_wire_matches_entered_and_left() {
    let clicked = ObservationRecord::CornerClicked {
        output: "o_nested".into(),
        corner: Corner::BottomRight,
        dwell_ms: 217,
        event_seq: 42,
    };
    let wire = clicked.wire();
    assert_eq!(wire.get("command"), Some("corner.clicked"));
    assert_eq!(wire.get("event_seq"), Some("42"));
    let body = serde_json::from_str::<Value>(&wire.body).unwrap();
    assert_eq!(
        body,
        json!({
            "output": "o_nested", "corner": "br", "dwell_ms": 217, "event_seq": 42,
        })
    );
    for record in [
        ObservationRecord::CornerEntered {
            output: "o_nested".into(),
            corner: Corner::BottomRight,
            dwell_ms: 217,
            event_seq: 42,
        },
        ObservationRecord::CornerLeft {
            output: "o_nested".into(),
            corner: Corner::BottomRight,
            dwell_ms: 217,
            event_seq: 42,
        },
    ] {
        assert_eq!(
            serde_json::from_str::<Value>(&record.wire().body).unwrap(),
            body
        );
    }
}

#[test]
fn affected_topics_supports_every_topic_including_pointer() {
    let mut topics = AffectedTopics::default();
    for suffix in TOPIC_SUFFIXES {
        topics.insert(suffix);
    }
    assert_eq!(topics.0, (1u16 << TOPIC_SUFFIXES.len()) - 1);
    assert_eq!(topics.iter().collect::<Vec<_>>(), TOPIC_SUFFIXES);
    topics.remove(CORNER_CLICKED_TOPIC_SUFFIX);
    assert!(!topics.contains(CORNER_CLICKED_TOPIC_SUFFIX));
    let mut clicked = AffectedTopics::default();
    clicked.insert(CORNER_CLICKED_TOPIC_SUFFIX);
    topics.merge(clicked);
    assert_eq!(topics.0, (1u16 << TOPIC_SUFFIXES.len()) - 1);
    topics.remove(POINTER_TOPIC_SUFFIX);
    assert!(!topics.contains(POINTER_TOPIC_SUFFIX));
}

#[test]
fn every_topic_uses_the_registered_service_and_an_unprefixed_command() {
    let records = [
        ObservationRecord::PropsChanged {
            path: "input.corners.dwell_ms".into(),
            old: PropValue::U64(200),
            new: PropValue::U64(250),
            unix_ms: 0,
            cause: "props.set",
            event_seq: 1,
        },
        ObservationRecord::SurfaceMapped {
            id: 1,
            role: "toplevel".into(),
            foreign_id: None,
            window: SurfaceEdgeWindow::default(),
            event_seq: 2,
        },
        ObservationRecord::SurfaceUnmapped {
            id: 1,
            role: "toplevel".into(),
            foreign_id: None,
            window: SurfaceEdgeWindow::default(),
            event_seq: 3,
        },
        ObservationRecord::FocusChanged {
            keyboard: Some(1),
            previous: None,
            exclusive_latch: None,
            event_seq: 4,
        },
        ObservationRecord::OutputChanged {
            output: "o_nested".into(),
            row: OutputSnapshot {
                instance: uuid::Uuid::nil().to_string(),
                generation: 1,
                name: "nested".into(),
                default: true,
                x: 0,
                y: 0,
                width: 1,
                height: 1,
                scale: 1.0,
                refresh_mhz: 60_000,
                usable: RectSnapshot {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0,
                },
                presentation: None,
            },
            event_seq: 5,
        },
        ObservationRecord::CornerEntered {
            output: "o_nested".into(),
            corner: Corner::TopLeft,
            dwell_ms: 200,
            event_seq: 6,
        },
        ObservationRecord::CornerLeft {
            output: "o_nested".into(),
            corner: Corner::TopLeft,
            dwell_ms: 200,
            event_seq: 7,
        },
        ObservationRecord::CornerClicked {
            output: "o_nested".into(),
            corner: Corner::TopLeft,
            dwell_ms: 200,
            event_seq: 8,
        },
    ];
    let suffixes = [
        "props.changed",
        "surface.mapped",
        "surface.unmapped",
        "focus.changed",
        "output.changed",
        "corner.entered",
        "corner.left",
        "corner.clicked",
    ];
    for (record, suffix) in records.iter().zip(suffixes) {
        assert_eq!(record.topic_suffix(), suffix);
        assert_eq!(record.wire().get("command"), Some(suffix));
        assert_eq!(
            topic_name("observer-test", suffix),
            format!("observer-test.{suffix}")
        );
    }
}

#[test]
fn corner_property_validation_accepts_endpoints_and_rejects_wrong_json_types() {
    for (path, value) in [
        ("input.corners.enabled", json!(false)),
        ("input.corners.deadzone_px", json!(1.0)),
        ("input.corners.deadzone_px", json!(256.0)),
        ("input.corners.dwell_ms", json!(0)),
        ("input.corners.dwell_ms", json!(5_000)),
        ("input.corners.velocity_max_px_s", json!(1.0)),
        ("input.corners.velocity_max_px_s", json!(20_000.0)),
        ("input.corners.affordance", json!(false)),
        ("input.corners.discovery", json!(true)),
    ] {
        assert!(
            validate_corner_value(path, &value).is_ok(),
            "{path}={value}"
        );
    }
    for (path, value) in [
        ("input.corners.enabled", json!(1)),
        ("input.corners.deadzone_px", json!("12")),
        ("input.corners.dwell_ms", json!(1.5)),
        ("input.corners.dwell_ms", json!(-1)),
        ("input.corners.velocity_max_px_s", json!(null)),
        ("input.corners.affordance", json!(0)),
        ("input.corners.discovery", json!("true")),
    ] {
        assert!(matches!(
            validate_corner_value(path, &value),
            Err(SetValidationError::InvalidValue { .. })
        ));
    }
}

#[test]
fn corner_hold_property_is_unknown() {
    for value in [json!(0), json!(500), json!(5_001), json!(1.5)] {
        assert_eq!(
            validate_corner_value("input.corners.hold_ms", &value),
            Err(SetValidationError::UnknownPath)
        );
        assert_eq!(
            validate_set_request("input.corners.hold_ms", &value),
            Err(SetValidationError::UnknownPath)
        );
    }
}

/// Event-sequence exhaustion, on the counter alone.
#[test]
fn event_sequence_exhaustion_offers_max_once_then_stops() {
    let mut seq = EventSeq::default();
    assert_eq!(
        (seq.next_seq(), seq.next_seq()),
        (Some(1), Some(2)),
        "starts at 1"
    );
    let mut seq = EventSeq::starting_after(u64::MAX - 1);
    assert_eq!(seq.next_seq(), Some(u64::MAX));
    assert_eq!(seq.next_seq(), None);
    assert_eq!(seq.current(), u64::MAX, "the watermark stays at MAX");
    assert!(seq.exhausted());
    assert_eq!(EventSeq::starting_after(u64::MAX).next_seq(), None);
}

// ---- rfc3339, the gap message and the remaining topic bodies ----

#[test]
fn rfc3339_millis_matches_chronos_utc_millisecond_form() {
    for (unix_ms, expected) in [
        (0, "1970-01-01T00:00:00.000Z"),
        (1_759_449_600_123, "2025-10-03T00:00:00.123Z"),
        (951_782_400_000, "2000-02-29T00:00:00.000Z"),
        (-1, "1969-12-31T23:59:59.999Z"),
        (4_107_542_399_999, "2100-02-28T23:59:59.999Z"),
    ] {
        assert_eq!(rfc3339_millis(unix_ms), expected, "{unix_ms}");
    }
}

#[test]
fn props_changed_carries_path_and_cause_headers_and_a_timestamp() {
    let record = ObservationRecord::PropsChanged {
        path: "input.corners.dwell_ms".into(),
        old: PropValue::U64(200),
        new: PropValue::U64(250),
        unix_ms: 1_759_449_600_123,
        cause: "props.set",
        event_seq: 12,
    };
    let wire = record.wire();
    assert_eq!(
        wire.headers.keys().map(String::as_str).collect::<Vec<_>>(),
        ["cause", "command", "event_seq", "path"]
    );
    assert_eq!(wire.get("path"), Some("input.corners.dwell_ms"));
    assert_eq!(wire.get("cause"), Some("props.set"));
    assert_eq!(wire.get("event_seq"), Some("12"));
    assert_eq!(
        wire.body,
        r#"{"cause":"props.set","event_seq":12,"new":250,"old":200,"path":"input.corners.dwell_ms","ts":"2025-10-03T00:00:00.123Z"}"#
    );
}

#[test]
fn every_topic_body_carries_its_event_seq() {
    let row = OutputSnapshot {
        instance: uuid::Uuid::nil().to_string(),
        generation: 1,
        name: "DP-1".into(),
        default: true,
        x: 0,
        y: 0,
        width: 1920,
        height: 1080,
        scale: 1.25,
        refresh_mhz: 60_000,
        usable: RectSnapshot {
            x: 0.0,
            y: 30.0,
            width: 1920.0,
            height: 1050.0,
        },
        presentation: None,
    };
    let records = [
        ObservationRecord::PanelCommand {
            output: "DP-1".into(),
            edge: "left".into(),
            surface: "quoin-panel-1".into(),
            reveal: false,
            event_seq: 1,
        },
        ObservationRecord::PointerChanged {
            sample: PointerSample {
                version: 1,
                instance: Arc::from("abc"),
                output: Some("o_dp_1".into()),
                position: Some(PointerPosition { x: 1.5, y: 2.0 }),
                valid: true,
                timestamp_ms: 9,
            },
            event_seq: 2,
        },
        ObservationRecord::FocusChanged {
            keyboard: Some(7),
            previous: None,
            exclusive_latch: None,
            event_seq: 3,
        },
        ObservationRecord::OutputChanged {
            output: "o_dp_1".into(),
            row,
            event_seq: 4,
        },
    ];
    let bodies = [
        r#"{"action":"conceal","edge":"left","event_seq":1,"output":"DP-1","surface":"quoin-panel-1","version":1}"#,
        r#"{"event_seq":2,"instance":"abc","output":"o_dp_1","position":{"x":1.5,"y":2.0},"timestamp_ms":9,"valid":true,"version":1}"#,
        r#"{"event_seq":3,"exclusive_latch":null,"keyboard":7,"previous":null}"#,
        r#"{"event_seq":4,"geometry":{"height":1080,"width":1920,"x":0,"y":0},"output":"o_dp_1","usable":{"height":1050.0,"width":1920.0,"x":0.0,"y":30.0}}"#,
    ];
    for (record, body) in records.iter().zip(bodies) {
        let wire = record.wire();
        assert_eq!(wire.body, body, "{}", record.topic_suffix());
        assert_eq!(
            wire.get("event_seq"),
            Some(record.event_seq().to_string().as_str())
        );
        assert_eq!(wire.get("command"), Some(record.topic_suffix()));
    }
}

#[test]
fn a_gap_names_its_last_lost_sequence_count_and_cause() {
    let record = ObservationRecord::FocusChanged {
        keyboard: None,
        previous: None,
        exclusive_latch: None,
        event_seq: 40,
    };
    let mut gap = LossInterval::from_record(&record, LossCause::OutboxOverflow);
    gap.merge(LossInterval {
        first_lost_seq: 38,
        last_lost_seq: 42,
        topics: AffectedTopics::default(),
        cause: LossCause::OutboxOverflow,
    });
    assert_eq!((gap.first_lost_seq, gap.last_lost_seq), (38, 42));
    let wire = gap_message(FOCUS_TOPIC_SUFFIX, gap, 5);
    assert_eq!(wire.get("command"), Some("focus.changed"));
    assert_eq!(wire.get("event_seq"), Some("42"));
    assert_eq!(
        wire.body,
        r#"{"cause":"outbox.overflow","gap":true,"lost_count":5}"#
    );
}

#[test]
fn corner_values_apply_and_report_old_and_new() {
    let mut config = CornerConfig::default();
    assert!(config.valid());
    let value = validate_corner_value("input.corners.dwell_ms", &json!(350)).unwrap();
    assert_eq!(
        apply_corner_value(&mut config, value),
        (PropValue::U64(200), PropValue::U64(350))
    );
    assert_eq!(config.dwell_ms, 350);
    assert!(publishes_prop_change(
        "input.corners.dwell_ms",
        &PropValue::U64(200),
        &PropValue::U64(350)
    ));
    assert!(!publishes_prop_change(
        "port.event_seq",
        &PropValue::U64(1),
        &PropValue::U64(2)
    ));
    assert!(!publishes_prop_change(
        "dmabuf.accepted",
        &PropValue::U64(1),
        &PropValue::U64(2)
    ));
    assert_eq!(
        read_only_or_unknown("surfaces.s1.title"),
        SetValidationError::ReadOnly
    );
    assert_eq!(
        read_only_or_unknown("nonsense"),
        SetValidationError::UnknownPath
    );
    assert_eq!(
        Corner::ALL.map(Corner::summoned_edge),
        ["left", "top", "bottom", "right"]
    );
}

#[test]
fn with_event_seq_numbers_a_record_and_changes_nothing_else() {
    let record = ObservationRecord::FocusChanged {
        keyboard: Some(4),
        previous: None,
        exclusive_latch: None,
        event_seq: 0,
    };
    let numbered = record.clone().with_event_seq(17);
    assert_eq!(numbered.event_seq(), 17);
    assert_eq!(numbered.topic_suffix(), record.topic_suffix());
    assert_eq!(
        numbered,
        ObservationRecord::FocusChanged {
            keyboard: Some(4),
            previous: None,
            exclusive_latch: None,
            event_seq: 17,
        }
    );
}

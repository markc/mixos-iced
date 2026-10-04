// The store's tests: its own page registry and dialog seat, plus contract
// tests against the scene crate's canonical fixtures.

use super::*;

const STATIC: &str = scene::fixtures::STATIC;
const FIXTURE: &str = scene::fixtures::CONFORMANCE;
const CLIPPANEL: &str = scene::fixtures::CLIPPANEL;
const SHELL_VERBS: &str = include_str!("../tests/fixtures/shell-verbs.json");

const MODEL_SCENE: &str = r#"---
scene: 1
name: model-test
citizen: scene-behaviour
model: {"caption":"first","rows":[{"id":"one","cells":["one"]}]}
---
```mix
root: {widget: "column", children: ["caption", "rows"]}
caption: {widget: "text", text: "= $model.caption"}
rows: {widget: "list", rows: "= $model.rows", row: "item", row_height: 24}
item: {widget: "text", text: "{cells[0]}"}
```
"#;

fn mount<'a>(owner: &'a str, accepted_at: u64) -> SceneMount<'a> {
    SceneMount { output: "test-output", owner, accepted_at }
}

fn changed(publish: &[String]) -> Vec<Value> {
    publish
        .iter()
        .map(|wire| {
            let (header, body) = wire.split_once("\n---\n").unwrap();
            assert_eq!(header, format!("---\ncommand: {CHANGED_COMMAND}"));
            serde_json::from_str(body).unwrap()
        })
        .collect()
}

// ── contract: the scene crate's fixtures ────────────────────────────────

#[test]
fn every_fixture_validates_loads_and_reads_back_as_scene_core_resolves_it() {
    for source in [STATIC, FIXTURE, CLIPPANEL] {
        let expected = scene::resolve(&scene::parse(source).unwrap()).unwrap();
        let mut store = SceneStore::default();
        let (valid, summary) = store.request(SceneVerb::Validate, source, &Value::Null).unwrap();
        assert_eq!((valid["scene"].as_str(), valid["valid"].as_bool()), (Some(expected.name.as_str()), Some(true)));
        assert!(summary.is_none() && store.scenes.is_empty(), "validate has no side effects");
        let mut caller = mount("loader", 1);
        let done = store.dispatch(SceneVerb::Load, source, &Value::Null, &mut caller);
        assert_eq!(done.rc, 0, "{}", done.body);
        let reply: Value = serde_json::from_str(&done.body).unwrap();
        assert_eq!(reply["revision"], 1);
        assert_eq!(reply["digest"], digest(&expected));
        let summaries = changed(&done.publish);
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0]["scene"], expected.name.as_str());
        assert_eq!(summaries[0]["ops"], expected.nodes.len());
        let get = json!({"scene": expected.name});
        assert_eq!(store.request(SceneVerb::Get, "", &get).unwrap().0, json!(expected));
        // The source export round-trips to the same scene (line numbers may
        // move; identity, envelope, families and ports may not).
        let (export, _) =
            store.request(SceneVerb::Get, "", &json!({"scene": expected.name, "format": "source"})).unwrap();
        let reparsed = scene::parse(export["source"].as_str().unwrap()).unwrap();
        let actual = scene::resolve(&reparsed).unwrap();
        assert_eq!((&expected.name, &expected.citizen, &expected.window), (&actual.name, &actual.citizen, &actual.window));
        assert_eq!(expected.nodes.len(), actual.nodes.len());
        for (id, node) in &expected.nodes {
            assert_eq!(node.family, actual.nodes[id].family, "{id}");
            assert_eq!(node.ports, actual.nodes[id].ports, "{id}");
        }
        let watch = store.request(SceneVerb::Watch, "", &get).unwrap().0;
        assert_eq!(watch, json!({"scene": expected.name, "revision": 1, "digest": digest(&expected),
            "applied_revision": 0, "diagnostics": []}));
    }
}

#[test]
fn the_clippanel_fixture_prepares_its_rows_for_the_renderer() {
    let mut store = SceneStore::default();
    store.request(SceneVerb::Load, CLIPPANEL, &Value::Null).unwrap();
    let entry = store.scenes.values().next().unwrap();
    let lists: Vec<_> = entry.tree.nodes.iter().filter(|(_, node)| node.family == "list").collect();
    assert_eq!(entry.prepared.len(), lists.len());
    assert_eq!(lists.len(), 2, "table and remote");
    for (id, node) in lists {
        let rows = crate::templates::rows(node);
        assert_eq!(entry.prepared[id].instances.len(), rows.len(), "{id}");
    }
    let e1 = &entry.prepared["table"].instances["e1"];
    assert_eq!(e1["r_prev"].ports["text"], "example preview one");
    assert_eq!(e1["r_id"].ports["text"], "1");
    assert!(e1.contains_key("entry_row"), "the row node itself is instantiated");
    assert!(entry.prepared["remote"].instances.is_empty());
}

#[test]
fn describe_answers_every_family_and_refuses_an_unknown_one() {
    let mut store = SceneStore::default();
    let (all, _) = store.request(SceneVerb::Describe, "", &Value::Null).unwrap();
    let names: Vec<_> = all.as_object().unwrap().keys().cloned().collect();
    let mut expected: Vec<_> = FAMILIES.iter().map(|f| f.to_string()).collect();
    expected.sort();
    assert_eq!(names, expected);
    for family in FAMILIES {
        assert!(!all[family].as_array().unwrap().is_empty(), "{family}");
        let (one, _) = store.request(SceneVerb::Describe, "", &json!({"family": family})).unwrap();
        assert_eq!(one, all[family]);
    }
    let mut caller = mount("x", 1);
    let done = store.dispatch(SceneVerb::Describe, "", &json!({"family":"nope"}), &mut caller);
    assert_eq!(done.rc, 10);
    let body: Value = serde_json::from_str(&done.body).unwrap();
    assert_eq!((body["error_code"].as_str(), body["message"].as_str()), (Some("SCENE_REFUSED"), Some("unknown family")));
    assert!(done.publish.is_empty(), "a read refusal publishes nothing");
}

#[test]
fn a_refused_load_publishes_a_zero_op_summary_with_its_diagnostics() {
    let mut store = SceneStore::default();
    let mut caller = mount("loader", 1);
    let done = store.dispatch(SceneVerb::Load, "not a scene", &Value::Null, &mut caller);
    assert_eq!(done.rc, 10);
    let body: Value = serde_json::from_str(&done.body).unwrap();
    assert_eq!(body["error_code"], "SCENE_REFUSED");
    assert!(!body["diagnostics"].as_array().unwrap().is_empty());
    let summaries = changed(&done.publish);
    assert_eq!(summaries.len(), 1);
    assert_eq!((summaries[0]["revision"].as_u64(), summaries[0]["ops"].as_u64()), (Some(0), Some(0)));
    assert_eq!(summaries[0]["diagnostics"], body["diagnostics"]);
}


// ── ported SceneStore tests ──────────────────────────────────────────────

#[test]
fn managed_model_writes_require_loader_and_current_generation() {
    let mut store = SceneStore::default();
    let mut caller = mount("scenes", 1);
    let load = json!({"source": MODEL_SCENE, "model_generation": 7});
    store.request_mounted(SceneVerb::Load, "", &load, Some(&mut caller)).unwrap();
    let patch = json!({"scene":"model-test", "path":"model.caption", "value":"current", "generation":7});
    store.request_mounted(SceneVerb::Patch, "", &patch, Some(&mut caller)).unwrap();
    assert_eq!(store.scenes["model-test"].tree.nodes["caption"].ports["text"], "current");

    // Equal tokens confer no authority to a direct behaviour or mesh caller.
    for other in ["scene-model-test", "peer/editor"] {
        let mut caller = mount(other, 1);
        let before = store.scenes["model-test"].tree.clone();
        let revision = store.scenes["model-test"].revision;
        for path in ["model", "model.caption"] {
            let mut request = patch.clone();
            request["path"] = json!(path);
            request["value"] = if path == "model" { json!({"caption":"bad"}) } else { json!("bad") };
            let error = store.request_mounted(SceneVerb::Patch, "", &request, Some(&mut caller)).unwrap_err();
            assert_eq!(error["error_code"], "SCENE_MODEL_AUTHORITY");
            assert_eq!(store.scenes["model-test"].tree, before);
            assert_eq!(store.scenes["model-test"].revision, revision);
        }
        assert!(store.request_mounted(SceneVerb::Load, MODEL_SCENE, &Value::Null, Some(&mut caller)).is_err());
        assert!(store.request_mounted(SceneVerb::Load, "", &load, Some(&mut caller)).is_err());
    }
    // A raw load cannot accidentally strip an existing fence, even by its owner.
    assert!(store.request_mounted(SceneVerb::Load, MODEL_SCENE, &Value::Null, Some(&mut caller)).is_err());
    store
        .request_mounted(SceneVerb::Load, "", &json!({"source":MODEL_SCENE,"model_generation":8}), Some(&mut caller))
        .unwrap();
    let revision = store.scenes["model-test"].revision;
    for generation in [Value::Null, json!(7), json!("8")] {
        let mut stale = patch.clone();
        stale["generation"] = generation;
        assert!(store.request_mounted(SceneVerb::Patch, "", &stale, Some(&mut caller)).is_err());
        assert_eq!(store.scenes["model-test"].revision, revision);
    }
    let mut current = patch;
    current["generation"] = json!(8);
    store.request_mounted(SceneVerb::Patch, "", &current, Some(&mut caller)).unwrap();
    store
        .request_mounted(
            SceneVerb::Patch,
            "",
            &json!({"scene":"model-test","path":"caption.text","value":"layout"}),
            Some(&mut caller),
        )
        .unwrap();
    assert_eq!(store.scenes["model-test"].model_generation, Some(8));
    store
        .request_mounted(SceneVerb::Load, "", &json!({"source":MODEL_SCENE,"model_generation":0}), Some(&mut caller))
        .unwrap();
    current["generation"] = json!(0);
    assert!(store.request_mounted(SceneVerb::Patch, "", &current, Some(&mut caller)).is_err());
    // An out-of-range or non-integer generation is refused before parsing.
    for generation in [json!(9_007_199_254_740_991u64), json!(-1), json!(1.5)] {
        let error = store
            .request_mounted(SceneVerb::Load, "", &json!({"source":MODEL_SCENE,"model_generation":generation}), Some(&mut caller))
            .unwrap_err();
        assert_eq!(error["error_code"], "SCENE_MODEL_GENERATION");
    }
}

#[test]
fn model_patch_is_transactional_and_retains_bindings_and_last_good_values() {
    let mut store = SceneStore::default();
    store.request(SceneVerb::Load, MODEL_SCENE, &Value::Null).unwrap();
    let bindings = store.scenes["model-test"].bindings.clone();
    store
        .request(SceneVerb::Patch, "", &json!({"scene":"model-test","path":"model.caption","value":"second"}))
        .unwrap();
    assert_eq!(store.scenes["model-test"].tree.nodes["caption"].ports["text"], "second");
    assert_eq!(store.scenes["model-test"].document.model.as_ref().unwrap()["caption"], "second");
    assert_eq!(store.scenes["model-test"].bindings, bindings);
    // A failed expression keeps its last-good port, while accepting model
    // data and returning an evaluation diagnostic, per core binding policy.
    let (_, summary) = store
        .request(SceneVerb::Patch, "", &json!({"scene":"model-test","path":"model","value":{"caption":7,"rows":[]}}))
        .unwrap();
    assert_eq!(store.scenes["model-test"].tree.nodes["caption"].ports["text"], "second");
    assert!(!summary.unwrap()["diagnostics"].as_array().unwrap().is_empty());
    let before = store.scenes["model-test"].tree.clone();
    let revision = store.scenes["model-test"].revision;
    for (path, value) in [
        ("model..caption", json!("bad")),
        ("model.rows.child", json!("bad")),
        ("model", json!(7)),
        ("model", json!({"large":"x".repeat(scene::MAX_DOCUMENT_BYTES)})),
    ] {
        assert!(store.request(SceneVerb::Patch, "", &json!({"scene":"model-test","path":path,"value":value})).is_err());
        assert_eq!(store.scenes["model-test"].tree, before);
        assert_eq!(store.scenes["model-test"].revision, revision);
    }
}

#[test]
fn cumulative_model_bounds_source_export_and_validation_have_no_side_effects() {
    let mut store = SceneStore::default();
    store.request(SceneVerb::Validate, MODEL_SCENE, &Value::Null).unwrap();
    assert!(store.scenes.is_empty());
    assert!(store.revisions.is_empty());
    store.request(SceneVerb::Load, MODEL_SCENE, &Value::Null).unwrap();
    store
        .request(SceneVerb::Patch, "", &json!({"scene":"model-test","path":"model.a","value":"x".repeat(150_000)}))
        .unwrap();
    let revision = store.scenes["model-test"].revision;
    assert!(
        store
            .request(SceneVerb::Patch, "", &json!({"scene":"model-test","path":"model.b","value":"x".repeat(150_000)}))
            .is_err()
    );
    assert_eq!(store.scenes["model-test"].revision, revision);
    let (export, _) = store.request(SceneVerb::Get, "", &json!({"scene":"model-test","format":"source"})).unwrap();
    let document = scene::parse(export["source"].as_str().unwrap()).unwrap();
    assert_eq!(document.nodes["caption"].ports["text"], "= $model.caption");
    assert_eq!(document.model, store.scenes["model-test"].document.model);
    assert_eq!(document.nodes["item"].ports["text"], "{cells[0]}");
    // Authored behaviour is event routing only; it cannot revoke loader seats.
    store.scenes.get_mut("model-test").unwrap().owner = Some(SceneOwner { citizen: "scenes".into(), accepted_at: 1 });
    assert!(store.unload_owned_by("scene-behaviour").is_empty());
    assert!(store.scenes.contains_key("model-test"));
}

#[test]
fn model_patch_cannot_move_a_live_mount_address() {
    let mut store = SceneStore::default();
    let source = "---\nscene: 1\nname: edge-binding\ncitizen: behaviour\nmodel: {\"edge\":\"right\"}\n---\n```mix\nroot: {widget: \"window\", kind: \"edge\", edge: \"= $model.edge\"}\n```\n";
    store.request(SceneVerb::Load, source, &Value::Null).unwrap();
    let before = store.scenes["edge-binding"].tree.clone();
    let revision = store.scenes["edge-binding"].revision;
    let error = store
        .request(SceneVerb::Patch, "", &json!({"scene":"edge-binding", "path":"model.edge", "value":"left"}))
        .unwrap_err();
    assert_eq!(error["error_code"], "SUBPANEL_COLLISION");
    assert_eq!(store.scenes["edge-binding"].tree, before);
    assert_eq!(store.scenes["edge-binding"].revision, revision);
}

#[test]
fn inventory_requires_the_render_reservation_and_reports_unowned_watch() {
    let mut store = SceneStore::default();
    store.request(SceneVerb::Load, FIXTURE, &Value::Null).unwrap();
    let output = "test-output";
    let watched = store.request(SceneVerb::Watch, "", &json!({"scene":"conformance"})).unwrap().0;
    let row = &store.list(output)[0];
    assert_eq!(row["owner"], Value::Null);
    assert_eq!(row["registered"], false);
    assert_eq!(row["revision"], watched["revision"]);
    assert_eq!(row["digest"], watched["digest"]);

    let entry = store.scenes.get_mut("conformance").unwrap();
    entry.owner = Some(SceneOwner { citizen: "loader".into(), accepted_at: 7 });
    let page = page_id(&entry.tree);
    let edge = scene_edge(&entry.tree);
    let other_edge = if edge == Edge::Left { Edge::Right } else { Edge::Left };
    for (owner, receipt, seat_edge, seat_output, registered) in [
        ("loader", 7, edge, output, true),
        ("other", 7, edge, output, false),
        ("loader", 8, edge, output, false),
        ("loader", 7, other_edge, output, false),
        ("loader", 7, edge, "other-output", false),
    ] {
        store.pages.forget(&page);
        store.pages.mount(&page, seat_output, seat_edge, owner, receipt).unwrap();
        let rows = store.list(output);
        assert_eq!(rows[0]["registered"], registered);
        assert_eq!(rows[0]["edge"], if registered { json!(edge.as_str()) } else { Value::Null });
        assert_eq!(rows[0]["owner"], "loader");
    }
}

#[test]
fn cumulative_patch_size_is_transactional() {
    let mut store = SceneStore::default();
    store.request(SceneVerb::Load, FIXTURE, &Value::Null).unwrap();
    // Each patch fits the ingress bound; their combined document does not.
    for path in ["text.text", "field.value"] {
        store
            .request(SceneVerb::Patch, "", &json!({"scene":"conformance","path":path,"value":"x".repeat(100_000)}))
            .unwrap();
    }
    let before = store.scenes["conformance"].tree.clone();
    let revision = store.scenes["conformance"].revision;
    let error = store
        .request(SceneVerb::Patch, "", &json!({"scene":"conformance","path":"button.label","value":"x".repeat(100_000)}))
        .unwrap_err();
    assert_eq!(error["diagnostics"][0]["code"], "document-too-large");
    assert_eq!(error["error_code"], "DOCUMENT_TOO_LARGE");
    assert_eq!(store.scenes["conformance"].tree, before);
    assert_eq!(store.scenes["conformance"].revision, revision);
}

#[test]
fn conformance_and_last_good() {
    let expected = scene::resolve(&scene::parse(FIXTURE).unwrap()).unwrap();
    let mut store = SceneStore::default();
    store.request(SceneVerb::Load, FIXTURE, &Value::Null).unwrap();
    let get = json!({"scene":"conformance"});
    assert_eq!(store.request(SceneVerb::Get, "", &get).unwrap().0, json!(expected));
    assert!(store.request(SceneVerb::Load, "bad", &Value::Null).is_err());
    assert!(
        store
            .request(SceneVerb::Patch, "", &json!({"scene":"conformance","path":"field.value","value":false}))
            .is_err()
    );
    assert_eq!(store.request(SceneVerb::Get, "", &get).unwrap().0, json!(expected));
    assert_eq!(store.scenes["conformance"].revision, 1);
    // A port read, an unknown path and an unknown port.
    assert_eq!(store.request(SceneVerb::Get, "", &json!({"scene":"conformance","path":"button.label"})).unwrap().0, "Go");
    assert!(store.request(SceneVerb::Get, "", &json!({"scene":"conformance","path":"button"})).is_err());
    assert!(store.request(SceneVerb::Patch, "", &json!({"scene":"conformance","path":"button.nope","value":1})).is_err());
}

fn dialog_source(name: &str, w: u32) -> String {
    format!(
        "---\nscene: 1\nname: {name}\ncitizen: scene-{name}\nwindow: {{\"chrome\":true,\"h\":620,\"kind\":\"dialog\",\"title\":\"Scene Editor\",\"w\":{w}}}\n---\n```mix\nroot: {{widget: \"column\", children: [\"caption\"]}}\ncaption: {{widget: \"text\", text: \"hi\"}}\n```\n"
    )
}

fn load(store: &mut SceneStore, owner: &str, receipt: u64, source: &str, preempt: bool) -> Result<(Value, Option<Value>), Value> {
    let mut caller = SceneMount { output: "DP-1", owner, accepted_at: receipt };
    let mut args = json!({"source": source, "model_generation": 3});
    if preempt {
        args["preempt_dialog"] = json!(true);
    }
    store.request_mounted(SceneVerb::Load, "", &args, Some(&mut caller))
}

#[test]
fn a_dialog_load_takes_the_dialog_seat_never_an_edge_page() {
    let mut store = SceneStore::default();
    load(&mut store, "scenes", 1, &dialog_source("editor", 880), false).unwrap();
    let seat = store.dialog_seat().unwrap();
    assert_eq!((seat.scene.as_str(), seat.owner.as_str(), seat.w, seat.h), ("editor", "scenes", 880.0, 620.0));
    assert_eq!((seat.title.as_deref(), seat.chrome), (Some("Scene Editor"), true));
    assert!(store.pages.seat("scene-editor").is_none(), "no page seat");
    assert_eq!(store.is_dialog("editor"), Some(true));
    assert_eq!(store.edge_page("editor"), None);
    let row = &store.list("DP-1")[0];
    assert_eq!((row["kind"].as_str(), row["edge"].is_null(), row["registered"].as_bool()), (Some("dialog"), true, Some(true)));
    // The same holder reloads (a new size) in place.
    load(&mut store, "scenes", 1, &dialog_source("editor", 900), false).unwrap();
    assert_eq!(store.dialog_seat().unwrap().w, 900.0);
    // A loaded dialog cannot become an edge page in place.
    let edge = dialog_source("editor", 880).replace(
        "{\"chrome\":true,\"h\":620,\"kind\":\"dialog\",\"title\":\"Scene Editor\",\"w\":880}",
        "{\"kind\":\"edge\",\"edge\":\"right\"}",
    );
    let error = load(&mut store, "scenes", 1, &edge, false).unwrap_err();
    assert_eq!(error["error_code"], "SUBPANEL_COLLISION");
    assert!(store.pages.seat("scene-editor").is_none());
}

#[test]
fn a_second_dialog_is_busy_unless_it_preempts_and_the_incumbent_is_told() {
    let mut store = SceneStore::default();
    load(&mut store, "some-citizen", 1, &dialog_source("other-dialog", 400), false).unwrap();
    let before = store.scenes["other-dialog"].revision;
    let error = load(&mut store, "scenes", 2, &dialog_source("editor", 880), false).unwrap_err();
    assert_eq!(error["error_code"], "DIALOG_BUSY");
    assert_eq!(error["holder"], json!({"scene":"other-dialog","owner":"some-citizen"}));
    // The refusal body is the frozen one, plus the scene it refused.
    let frozen: Value = serde_json::from_str(SHELL_VERBS).unwrap();
    let mut refusal = frozen["shell.scene.load"]["refusals"]["DIALOG_BUSY"].clone();
    refusal["scene"] = json!("editor");
    assert_eq!(error, refusal);
    assert!(store.scenes.contains_key("other-dialog") && !store.scenes.contains_key("editor"));
    assert!(store.notices.is_empty());

    load(&mut store, "scenes", 3, &dialog_source("editor", 880), true).unwrap();
    assert_eq!(store.dialog_seat().unwrap().scene, "editor");
    assert!(!store.scenes.contains_key("other-dialog"), "the incumbent is unloaded");
    let mut expected = frozen["shell.scene.load"]["preemption_notice"]["body"].clone();
    expected["revision"] = json!(before);
    assert_eq!(std::mem::take(&mut store.notices), vec![expected]);
    // Pre-empting again by the holder itself displaces nobody.
    load(&mut store, "scenes", 3, &dialog_source("editor", 880), true).unwrap();
    assert!(store.notices.is_empty());
}

fn edge_source(name: &str, panel: Option<&str>) -> String {
    let panel = panel.map_or_else(String::new, |panel| format!(",\"panel\":\"{panel}\""));
    format!(
        "---\nscene: 1\nname: {name}\ncitizen: squatter\nwindow: {{\"kind\":\"edge\",\"edge\":\"right\"{panel}}}\n---\n```mix\nroot: {{widget: \"column\", children: []}}\n```\n"
    )
}

/// An edge scene on page `scene-editor` cannot squat the editor, because a
/// dialog is outside the page-id namespace.
#[test]
fn an_edge_page_squatter_cannot_block_the_editor() {
    let mut store = SceneStore::default();
    load(&mut store, "someone", 1, &edge_source("squat", Some("scene-editor")), false).unwrap();
    assert!(store.pages.seat("scene-editor").is_some());
    for preempt in [false, true] {
        load(&mut store, "scenes", 2, &dialog_source("editor", 880), preempt).unwrap();
    }
    assert_eq!(store.dialog_seat().unwrap().scene, "editor");
    assert!(store.scenes.contains_key("squat"), "no conflict, nothing displaced");
    assert!(store.notices.is_empty());
    // Nor can a dialog take an edge page's address from it.
    assert!(store.pages.seat("scene-editor").is_some_and(|seat| seat.owner == "someone"));
}

/// An edge scene named `editor`, even another owner's fenced one, is
/// displaced by the pre-empting editor load; without the flag the switch is
/// refused.
#[test]
fn a_name_squatter_is_displaced_only_by_a_preempting_dialog_load() {
    let mut store = SceneStore::default();
    load(&mut store, "someone", 1, &edge_source("editor", None), false).unwrap();
    assert!(store.pages.seat("scene-editor").is_some());
    let error = load(&mut store, "scenes", 2, &dialog_source("editor", 880), false).unwrap_err();
    assert_eq!(error["error_code"], "SCENE_MODEL_AUTHORITY", "{error}");
    load(&mut store, "scenes", 3, &dialog_source("editor", 880), true).unwrap();
    assert_eq!(store.is_dialog("editor"), Some(true));
    assert_eq!(store.dialog_seat().unwrap().owner, "scenes");
    assert!(store.pages.seat("scene-editor").is_none(), "the squatter's page is released");
    let notices = std::mem::take(&mut store.notices);
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0]["reason"], "preempted");
    assert_eq!(notices[0]["by"], json!({"scene":"editor","owner":"scenes"}));
    assert_eq!(store.removed.len(), 0, "the squatter was never rendered in this fixture");
}

#[test]
fn unload_and_owner_disconnect_release_the_dialog_seat() {
    let mut store = SceneStore::default();
    load(&mut store, "scenes", 1, &dialog_source("editor", 880), false).unwrap();
    let mut caller = SceneMount { output: "DP-1", owner: "scenes", accepted_at: 2 };
    store.request_mounted(SceneVerb::Unload, "", &json!({"scene":"editor"}), Some(&mut caller)).unwrap();
    assert!(store.dialog_seat().is_none());

    load(&mut store, "scenes", 3, &dialog_source("editor", 880), false).unwrap();
    assert!(store.unload_owned_before("scenes", 3).is_empty(), "accepted at the cutoff stays");
    assert_eq!(store.unload_owned_before("scenes", 4), ["editor"]);
    assert!(store.dialog_seat().is_none());
    // The departure is announced, so a returning owner remounts.
    let notices = std::mem::take(&mut store.notices);
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0]["scene"], "editor");
    assert_eq!(notices[0]["reason"], "owner_departed");
    assert_eq!(notices[0]["ops"], json!(["unloaded"]));
    assert_eq!(notices[0]["owner"], "scenes");
    // The freed seat is anyone's.
    load(&mut store, "someone", 5, &dialog_source("other-dialog", 400), false).unwrap();
}

#[test]
fn an_owner_departure_frees_its_edge_page_and_hands_the_renderer_its_content() {
    let mut store = SceneStore::default();
    load(&mut store, "behaviour", 1, &edge_source("notes", None), false).unwrap();
    store.set_mounted("notes", Some(Mounted { handle: 42, revision: 1 }));
    assert_eq!(store.live_owners().into_iter().collect::<Vec<_>>(), ["behaviour"]);
    assert_eq!(store.unload_owned_before("behaviour", 2), ["notes"]);
    assert!(store.pages.seat("scene-notes").is_none());
    assert_eq!(store.take_removed(), [Mounted { handle: 42, revision: 1 }]);
    assert_eq!(changed(&store.take_notices())[0]["reason"], "owner_departed");
    // Anonymous and mesh-qualified owners have no local lifetime to sweep.
    load(&mut store, "anonymous@7", 3, &edge_source("anon", None), false).unwrap();
    load(&mut store, "editor@peer", 4, &edge_source("mesh", None), false).unwrap();
    assert!(store.live_owners().is_empty());
}

/// A seated edge page that is not on screen takes each revision as it lands
/// (Quoin mounts it off screen); a drawn one reports its surface's; another
/// output's pass touches neither; a hide never drops it to 0.
#[test]
fn an_offscreen_seated_page_applies_each_revision() {
    let mut store = SceneStore::default();
    let watch = |store: &mut SceneStore| store.request(SceneVerb::Watch, "", &json!({"scene": "notes"})).unwrap().0["applied_revision"].clone();
    load(&mut store, "behaviour", 1, &edge_source("notes", None), false).unwrap();
    assert_eq!(watch(&mut store), 0);
    store.apply_offscreen("HDMI-A-1", |_| false);
    assert_eq!(watch(&mut store), 0, "seated on DP-1, not HDMI-A-1");
    store.apply_offscreen("DP-1", |name| name == "notes");
    assert_eq!(watch(&mut store), 0, "drawn: its surface reports, not the store");
    store.apply_offscreen("DP-1", |_| false);
    assert_eq!(watch(&mut store), 1);
    store.set_mounted("notes", Some(Mounted { handle: 7, revision: 1 }));
    store.set_mounted("notes", None);
    assert_eq!(watch(&mut store), 1, "a hide keeps what was applied");
}

/// The loader treats exactly this refusal body as "already unloaded".
/// Rewording it must break this test.
#[test]
fn unloading_an_unknown_scene_refuses_with_the_pinned_body() {
    let mut store = SceneStore::default();
    let mut caller = SceneMount { output: "DP-1", owner: "scenes", accepted_at: 1 };
    let done = store.dispatch(SceneVerb::Unload, "", &json!({"scene":"gone"}), &mut caller);
    assert_eq!(done.rc, 10, "{}", done.body);
    let body: Value = serde_json::from_str(&done.body).unwrap();
    assert_eq!(body["error"], "unknown scene", "{body}");
    assert_eq!(body["error_code"], "SCENE_REFUSED", "{body}");
    assert!(done.publish.is_empty());
}

#[test]
fn a_preempting_dispatch_publishes_the_incumbents_notice_first() {
    let mut store = SceneStore::default();
    let mut publish = Vec::new();
    for (owner, receipt, name, preempt) in [("some-citizen", 1, "other-dialog", false), ("scenes", 2, "editor", true)] {
        let mut args = json!({"source": dialog_source(name, 880), "model_generation": 3});
        if preempt {
            args["preempt_dialog"] = json!(true);
        }
        let mut caller = SceneMount { output: "DP-1", owner, accepted_at: receipt };
        let done = store.dispatch(SceneVerb::Load, "", &args, &mut caller);
        assert_eq!(done.rc, 0, "{}", done.body);
        publish.extend(done.publish);
    }
    let changes = changed(&publish);
    let scenes: Vec<_> = changes.iter().map(|c| (c["scene"].as_str().unwrap(), c["reason"].as_str())).collect();
    assert_eq!(
        scenes,
        [("other-dialog", None), ("other-dialog", Some("preempted")), ("editor", None)],
        "the displaced owner hears before its successor's summary"
    );
    assert_eq!(changes[1]["by"], json!({"scene":"editor","owner":"scenes"}));
    assert!(store.notices.is_empty());
}

#[test]
fn show_targets_the_selected_output() {
    let mut store = SceneStore::default();
    load(&mut store, "scenes", 1, &dialog_source("editor", 880), false).unwrap();
    assert!(store.retarget_dialog("editor", "HDMI-A-1"));
    assert_eq!(store.dialog_seat().unwrap().output, "HDMI-A-1");
    assert!(!store.retarget_dialog("other", "HDMI-A-1"));
}

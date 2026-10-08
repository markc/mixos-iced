//! The document core's behaviour: parse, lint, resolve, diff, describe.

#![allow(clippy::useless_conversion)]

mod evaluation;
mod layout;

use super::*;
use crate::fixtures::{CLIPPANEL as C, CONFORMANCE as F, STATIC as S};
use crate::schema::schema;

fn doc(b: &str) -> SceneDocument {
    parse(&format!("---\nscene: 1\nname: test\ncitizen: c\n---\n```mix\n{b}\n```\n")).unwrap()
}

#[test]
fn image_src_cell_substitution_allowed_in_template() {
    let source = "root: {widget: \"list\", rows: [{id: \"a\", cells: [\"icon.png\"]}], row: \"t\", row_height: 24}\nt: {widget: \"row\", children: [\"icon\"]}\nicon: {widget: \"image\", src: \"{cells[0]}\"}";
    assert!(lint(&doc(source)).is_empty());
    for invalid in [
        source.replace("{cells[0]}", "{cells[1]}"),
        source.replace("children: [\"icon\"]", "children: [\"icon\"], background: \"{cells[0]}\""),
        "root: {widget: \"image\", src: \"{cells[0]}\"}".into(),
        "root: {widget: \"text\", text: \"{cells[0]}\"}".into(),
    ] {
        assert!(lint(&doc(&invalid)).iter().any(|d| d.code == "cell-substitution"), "{invalid}");
    }
}

#[test]
fn non_template_cell_markers_in_literal_ports_load() {
    for source in ["root: {widget: \"field\", value: \"{cells[0]}\"}", "root: {widget: \"button\", label: \"{cells[\"}"] {
        assert!(resolve(&doc(source)).is_ok(), "{source}");
    }
}

#[test]
fn envelope_window_chrome_is_type_checked() {
    let mut document = doc("root: {widget: \"column\", children: []}");
    for value in [json!("false"), json!(0), JsonValue::Null] {
        document.window = Some(json!({"chrome":value}));
        assert!(lint(&document).iter().any(|d| d.code == "port-type"));
        assert!(resolve(&document).is_err());
    }
    for value in [json!(true), json!(false)] {
        document.window = Some(json!({"chrome":value}));
        assert!(resolve(&document).is_ok());
    }
}

#[test]
fn window_chrome_port_defaults_true_and_accepts_false() {
    let source = "root: {widget: \"window\", kind: \"edge\"}";
    assert_eq!(resolve(&doc(source)).unwrap().nodes["root"].ports["chrome"], true);
    let mut document = doc(source);
    document.nodes.get_mut("root").unwrap().ports.insert("chrome".into(), json!(false));
    assert_eq!(resolve(&document).unwrap().nodes["root"].ports["chrome"], false);
    document.window = Some(json!({"kind":"edge","chrome":false}));
    assert!(lint(&document).is_empty());
    document.window.as_mut().unwrap()["chrome"] = json!(true);
    assert!(lint(&document).iter().any(|d| d.code == "window-disagreement"));
    document.window = None;
    document.nodes.get_mut("root").unwrap().ports.insert("chrome".into(), json!("false"));
    assert!(lint(&document).iter().any(|d| d.code == "port-type"));
}

fn codes(document: &SceneDocument) -> Vec<String> {
    lint(document).into_iter().filter(|d| d.severity == Severity::Error).map(|d| d.code).collect()
}

#[test]
fn dialog_window_kind_is_described_and_accepted() {
    let window = describe("window").unwrap();
    let kind = window.iter().find(|p| p.path == "kind").unwrap();
    assert_eq!(kind.enum_values, Some(vec!["edge".to_string(), "dialog".to_string()]));
    let mut document = doc("root: {widget: \"column\", children: []}");
    document.window = Some(json!({"kind":"dialog","w":880,"h":620,"title":"Scene Editor","chrome":true}));
    assert!(codes(&document).is_empty(), "{:?}", lint(&document));
    assert!(resolve(&document).is_ok());
    let node = doc("root: {widget: \"window\", kind: \"dialog\", w: 880, h: 620}");
    assert!(codes(&node).is_empty(), "{:?}", lint(&node));
}

#[test]
fn dialog_refuses_edge_panel_and_needs_bounded_size() {
    let mut document = doc("root: {widget: \"column\", children: []}");
    for (header, expected) in [
        (json!({"kind":"dialog","edge":"right","w":880,"h":620}), "window-dialog-edge"),
        (json!({"kind":"dialog","panel":"scene-x","w":880,"h":620}), "window-dialog-edge"),
        (json!({"kind":"dialog","h":620}), "missing-port"),
        (json!({"kind":"dialog","w":880}), "missing-port"),
        (json!({"kind":"dialog","w":239,"h":620}), "port-type"),
        (json!({"kind":"dialog","w":880,"h":2049}), "port-type"),
        (json!({"kind":"dialog","w":"880","h":620}), "port-type"),
    ] {
        document.window = Some(header.clone());
        assert!(codes(&document).iter().any(|c| c == expected), "{header}: {:?}", lint(&document));
        assert!(resolve(&document).is_err(), "{header} resolved");
    }
    for (w, h) in [(240, 240), (2048, 2048)] {
        document.window = Some(json!({"kind":"dialog","w":w,"h":h}));
        assert!(codes(&document).is_empty(), "{w}x{h}: {:?}", lint(&document));
    }
}

#[test]
fn header_only_window_declaration_is_validated() {
    let mut document = doc("root: {widget: \"column\", children: []}");
    for (header, expected) in [
        (json!({"kind":"floating"}), "window-kind"),
        (json!({"kind":7}), "window-kind"),
        (json!({"kind":"edge","edge":"middle"}), "enum-value"),
        (json!({"kind":"edge","w":-1}), "port-type"),
        (json!({"kind":"edge","h":"52"}), "port-type"),
        (json!({"kind":"edge","title":3}), "port-type"),
    ] {
        document.window = Some(header.clone());
        assert!(codes(&document).iter().any(|c| c == expected), "{header}: {:?}", lint(&document));
    }
    // Edge headers the shipped templates author stay valid, kind or not.
    for header in [
        json!({"kind":"edge","edge":"bottom","h":52,"chrome":false}),
        json!({"edge":"left","w":440,"title":"Applications","chrome":false}),
        json!({"kind":"edge","edge":"right","panel":"settings.appearance","title":"Settings"}),
        json!({"chrome":true}),
    ] {
        document.window = Some(header.clone());
        assert!(codes(&document).is_empty(), "{header}: {:?}", lint(&document));
    }
}

#[test]
fn column_align_and_text_align_validate_enums() {
    for (family, ports, default, values) in [
        ("column", "children: []", "stretch", vec!["start", "center", "end", "stretch"]),
        ("text", "text: \"x\"", "left", vec!["left", "center", "right"]),
    ] {
        let source = format!("root: {{widget: \"{family}\", {ports}}}");
        assert_eq!(resolve(&doc(&source)).unwrap().nodes["root"].ports["align"], default);
        for value in values {
            let source = format!("root: {{widget: \"{family}\", {ports}, align: \"{value}\"}}");
            assert!(lint(&doc(&source)).is_empty());
        }
        let source = format!("root: {{widget: \"{family}\", {ports}, align: \"invalid\"}}");
        assert!(lint(&doc(&source)).iter().any(|d| d.code == "enum-value"));
    }
}

#[test]
fn fixture_round_trips() {
    for s in [C, F, S] {
        let d = parse(s).unwrap();
        assert!(lint(&d).is_empty(), "{:?}", lint(&d));
        let r = resolve(&d).unwrap();
        assert!(diff(&r, &r).is_empty());
    }
}

#[test]
fn exported_fixtures_are_the_files() {
    for (stem, content) in fixtures::ALL {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/{stem}.scene.mix"));
        assert_eq!(std::fs::read_to_string(path).unwrap(), content, "{stem}");
    }
}

#[test]
fn resolved_defaults() {
    let n = &resolve(&doc("root: {widget: \"text\", text: \"x\"}")).unwrap().nodes["root"];
    for (k, v) in [
        ("size", json!(13.0)),
        ("bold", json!(false)),
        ("mono", json!(false)),
        ("elide", json!(false)),
        ("fill", json!(false)),
        ("hidden", json!(false)),
    ] {
        assert_eq!(n.ports[k], v);
    }
}

#[test]
fn resolved_defaults_cover_all_families() {
    let d = doc(
        "root: {widget: \"column\", children: [\"win\",\"row\",\"field\",\"button\",\"toggle\",\"list\",\"image\",\"spacer\"]}\nwin: {widget: \"window\", kind: \"edge\"}\nrow: {widget: \"row\", children: [\"text\"]}\ntext: {widget: \"text\", text: \"x\"}\nfield: {widget: \"field\", value: \"x\"}\nbutton: {widget: \"button\", label: \"x\"}\ntoggle: {widget: \"toggle\", value: false, label: \"x\"}\nlist: {widget: \"list\", rows: [], row: \"template\", row_height: 1}\ntemplate: {widget: \"row\", children: []}\nimage: {widget: \"image\", src: \"x\"}\nspacer: {widget: \"spacer\"}",
    );
    let r = resolve(&d).unwrap();
    for family in ["window", "column", "row", "text", "field", "button", "toggle", "list", "image", "spacer"] {
        assert!(r.nodes.values().any(|n| n.family == family), "{family}");
    }
    for (id, expected) in [
        ("win", [("edge", json!("right"))].as_slice()),
        ("root", [("gap", json!(0.0)), ("padding", json!(0.0)), ("fill", json!(false))].as_slice()),
        ("row", [("gap", json!(0.0)), ("padding", json!(0.0)), ("fill", json!(false)), ("align", json!("start"))].as_slice()),
        (
            "text",
            [
                ("size", json!(13.0)),
                ("bold", json!(false)),
                ("mono", json!(false)),
                ("elide", json!(false)),
                ("fill", json!(false)),
                ("hidden", json!(false)),
            ]
            .as_slice(),
        ),
        ("field", [("password", json!(false))].as_slice()),
        ("button", [("tone", json!("normal"))].as_slice()),
        ("list", [("gap", json!(0.0)), ("fill", json!(false)), ("hidden_if_empty", json!(false))].as_slice()),
    ] {
        for (port, value) in expected {
            assert_eq!(r.nodes[id].ports.get(*port), Some(value), "{id}.{port}");
        }
    }
    assert!(r.nodes["spacer"].ports.is_empty());
}

fn numeric_probe(family: &str, port: &str, value: f64) -> Vec<Diagnostic> {
    let mut nodes = IndexMap::new();
    let mut ports = IndexMap::new();
    for (name, value) in [
        ("kind", json!("edge")),
        ("children", json!([])),
        ("text", json!("x")),
        ("value", json!("x")),
        ("label", json!("x")),
        ("rows", json!([])),
        ("row", json!("template")),
        ("row_height", json!(1.0)),
        ("src", json!("x")),
    ] {
        ports.insert(name.into(), value);
    }
    ports.insert(port.into(), json!(value));
    nodes.insert("root".into(), RawNode { widget: family.into(), ports, line: 1 });
    if family == "list" {
        nodes.insert(
            "template".into(),
            RawNode { widget: "row".into(), ports: [("children".into(), json!([]))].into_iter().collect(), line: 1 },
        );
    }
    lint(&SceneDocument {
        name: "test".into(),
        citizen: "c".into(),
        window: None,
        subscribe: None,
        targets: None,
        model: None,
        nodes,
        source: String::new(),
        preparation: PreparationCache::default(),
    })
}

#[test]
fn numeric_port_boundaries_are_table_driven() {
    for family in ["window", "column", "row", "text", "field", "button", "list", "image", "spacer"] {
        for port in schema(family).unwrap().iter().filter(|p| p.ty == "number") {
            let zero = numeric_probe(family, port.name, 0.0);
            let negative = numeric_probe(family, port.name, -1.0);
            if port.name == "row_height" {
                assert!(zero.iter().any(|d| d.code == "port-min"));
                assert!(!numeric_probe(family, port.name, 0.5).iter().any(|d| d.code == "port-min"));
            } else if port.name == "max_rows" {
                assert!(zero.iter().any(|d| d.code == "port-min"));
                assert!(!numeric_probe(family, port.name, 1.0).iter().any(|d| d.code == "port-min"));
            } else {
                assert!(!zero.iter().any(|d| d.code == "port-min"));
            }
            assert!(negative.iter().any(|d| d.code == "port-min"), "{family}.{port_name}", port_name = port.name);
        }
    }
}

#[test]
fn window_disagreement_checks_all_windows_and_fields() {
    for (field, value) in [("kind", "floating"), ("edge", "left"), ("title", "wrong"), ("w", "10"), ("h", "20")] {
        let source = r#"---
scene: 1
name: test
citizen: c
window: {"kind":"edge","edge":"right","title":"ok","w":1,"h":2}
---
```mix
root: {widget: "column", children: ["a","b"]}
a: {widget: "window", kind: "edge", edge: "right", title: "ok", w: 1, h: 2}
b: {widget: "window", kind: "edge", edge: "right", title: "ok", w: 1, h: 2}
```
"#
        .to_string();
        let mut d = parse(&source).unwrap();
        d.nodes["b"].ports.insert(
            field.into(),
            match field {
                "kind" | "edge" | "title" => json!(value),
                _ => json!(value.parse::<f64>().unwrap()),
            },
        );
        assert!(lint(&d).iter().any(|x| x.code == "window-disagreement"), "{field}");
    }
}

#[test]
fn non_list_row_does_not_seed_template_reachability() {
    let d = doc("root: {widget: \"text\", text: \"x\", row: \"template\"}\ntemplate: {widget: \"text\", text: \"x\"}");
    assert!(lint(&d).iter().any(|x| x.code == "orphan-node" && x.message.contains("template")));
}

#[test]
fn list_template_child_check_scans_all_nodes() {
    let d = doc(
        "root: {widget: \"column\", children: [\"list\",\"holder\"]}\nlist: {widget: \"list\", rows: [], row: \"template\", row_height: 1}\nholder: {widget: \"column\", children: [\"template\"]}\ntemplate: {widget: \"row\", children: []}",
    );
    assert!(lint(&d).iter().any(|x| x.code == "invalid-template"));
}

#[test]
fn numeric_default_diff_is_empty() {
    let a = resolve(&doc("root: {widget: \"text\", text: \"x\"}")).unwrap();
    let b = resolve(&doc("root: {widget: \"text\", text: \"x\", size: 13}")).unwrap();
    assert!(diff(&a, &b).is_empty());
}

#[test]
fn graph_and_lint_regressions() {
    let d = doc(
        "root: {widget: \"column\", children: [\"a\"]}\na: {widget: \"text\", text: \"x\"}\nb: {widget: \"column\", children: [\"c\"]}\nc: {widget: \"column\", children: [\"b\"]}",
    );
    let codes: HashSet<_> = lint(&d).into_iter().map(|x| x.code).collect();
    assert!(codes.contains("cycle") && codes.contains("orphan-node"));
}

#[test]
fn diff_covers_exact_ops_from_resolved_documents() {
    let a = resolve(&doc("root: {widget: \"column\", children: [\"x\",\"y\"]}\nx: {widget: \"text\", text: \"x\", color: \"red\"}\ny: {widget: \"text\", text: \"y\"}")).unwrap();
    let b = resolve(&parse("---\nscene: 1\nname: newer\ncitizen: d\nwindow: {\"kind\":\"edge\"}\nsubscribe: [\"x\"]\n---\n```mix\nroot: {widget: \"column\", children: [\"z\",\"x\"]}\nx: {widget: \"text\", text: \"z\"}\nz: {widget: \"text\", text: \"new\"}\n```").unwrap()).unwrap();
    assert_eq!(
        diff(&a, &b),
        vec![
            Op::Remove { id: "y".into() },
            Op::SetPort { id: "root".into(), port: "children".into(), value: json!(["z", "x"]) },
            Op::SetPort { id: "x".into(), port: "color".into(), value: JsonValue::Null },
            Op::SetPort { id: "x".into(), port: "text".into(), value: json!("z") },
            Op::Insert { id: "z".into(), parent: Some("root".into()), index: 0, node: b.nodes["z"].clone() },
            Op::Reparent { id: "x".into(), parent: Some("root".into()), index: 1 },
            Op::SetScene { field: "name".into(), value: json!("newer") },
            Op::SetScene { field: "citizen".into(), value: json!("d") },
            Op::SetScene { field: "window".into(), value: json!({"kind":"edge"}) },
            Op::SetScene { field: "subscribe".into(), value: json!(["x"]) },
        ]
    );
}

#[test]
fn every_lint_class_has_a_diagnostic() {
    let valid = |body: &str| format!("---\nscene: 1\nname: test\ncitizen: c\n---\n```mix\n{body}\n```");
    let cases = vec![
        ("document-too-large", "x".repeat(MAX_DOCUMENT_BYTES + 1)),
        ("envelope", "not an envelope".into()),
        ("missing-header", "---\nscene: 1\n---\n```mix\nroot: {widget: \"text\", text: \"x\"}\n```\n".into()),
        ("scene-version", valid("root: {widget: \"text\", text: \"x\"}").replacen("scene: 1", "scene: 2", 1)),
        ("invalid-name", valid("root: {widget: \"text\", text: \"x\"}").replacen("name: test", "name: Bad", 1)),
        ("fence-count", "---\nscene: 1\nname: test\ncitizen: c\n---\n```mix\nroot: {widget: \"text\", text: \"x\"}\n```\n```mix\na: {widget: \"text\", text: \"x\"}\n```\n".into()),
        ("fence-count", "---\nscene: 1\nname: test\ncitizen: c\n---\nno fence\n".into()),
        ("mix-parse", valid("root: {widget: \"text\", text: \"x\"}\n\"unterminated").into()),
        ("strict-data", valid("root: {widget: \"text\", text: \"x\"}\nvalue: \"a\" ..\n  \"b\"").into()),
        ("duplicate-id", valid("root: {widget: \"text\", text: \"x\"}\nroot: {widget: \"text\", text: \"y\"}").into()),
        ("duplicate-id", valid("root: {widget: \"column\", children: [\"child\"]}\nchild: {widget: \"text\", text: \"x\"}\nchild: {widget: \"text\", text: \"y\"}").into()),
        ("root-type", valid("[\"x\"]").into()),
        ("node-type", valid("root: \"x\"").into()),
        ("missing-widget", valid("root: {}").into()),
        ("invalid-id", valid("root: {widget: \"column\", children: [\"bad@id\"]}\n\"bad@id\": {widget: \"text\", text: \"x\"}").into()),
        ("header-json", valid("root: {widget: \"text\", text: \"x\"}").replacen("citizen: c", "citizen: c\nwindow: nope", 1)),
        ("node-limit", valid(&(0..=MAX_NODES).map(|i| format!("n{i}: {{widget: \"text\", text: \"x\"}}\n")).collect::<String>())),
        ("unknown-family", valid("root: {widget: \"unknown\"}").into()),
        ("unknown-port", valid("root: {widget: \"text\", text: \"x\", nope: 1}").into()),
        ("layout-conflict", valid("root: {widget: \"text\", text: \"x\", fill: true, grow: 1}").into()),
        ("port-type", valid("root: {widget: \"text\", text: 1}").into()),
        ("enum-value", valid("root: {widget: \"button\", label: \"x\", tone: \"bad\"}").into()),
        ("port-min", valid("root: {widget: \"list\", rows: [], row: \"t\", row_height: 0}\nt: {widget: \"row\", children: []}").into()),
        ("missing-port", valid("root: {widget: \"text\"}").into()),
        ("dangling-child", valid("root: {widget: \"column\", children: [\"gone\"]}").into()),
        ("child-type", valid("root: {widget: \"column\", children: [1]}").into()),
        ("window-kind", valid("root: {widget: \"window\", kind: \"floating\"}").into()),
        ("row-limit", valid("root: {widget: \"list\", rows: [], row: \"t\", row_height: 1}\nt: {widget: \"row\", children: []}").into()),
        ("row-type", valid("root: {widget: \"list\", rows: [{}], row: \"t\", row_height: 1}\nt: {widget: \"row\", children: []}").into()),
        ("invalid-template", valid("root: {widget: \"list\", rows: [], row: \"t\", row_height: 1}\nt: {widget: \"button\", label: \"x\"}").into()),
        ("cell-substitution", valid("root: {widget: \"list\", rows: [{id: \"1\", cells: [\"x\"]}], row: \"t\", row_height: 1}\nt: {widget: \"row\", children: [\"x\"]}\nx: {widget: \"text\", text: \"{cells[1]}\"}").into()),
        ("missing-root", valid("x: {widget: \"text\", text: \"x\"}").into()),
        ("multiple-parents", valid("root: {widget: \"column\", children: [\"a\", \"b\"]}\na: {widget: \"column\", children: [\"x\"]}\nb: {widget: \"column\", children: [\"x\"]}\nx: {widget: \"text\", text: \"x\"}").into()),
        ("window-disagreement", valid("root: {widget: \"window\", kind: \"edge\", title: \"node\"}").replacen("citizen: c", "citizen: c\nwindow: {\"kind\":\"edge\",\"title\":\"header\"}", 1)),
        ("window-dialog-edge", valid("root: {widget: \"window\", kind: \"dialog\", edge: \"left\", w: 400, h: 300}").into()),
        ("orphan-node", valid("root: {widget: \"text\", text: \"x\"}\nother: {widget: \"text\", text: \"y\"}").into()),
        ("cycle", valid("root: {widget: \"column\", children: [\"a\"]}\na: {widget: \"column\", children: [\"root\"]}").into()),
        ("invalid-binding", valid("root: {widget: \"text\", text: \"= \"}").into()),
        ("binding-policy", valid("root: {widget: \"text\", text: \"= send \\\"x\\\" y\"}").into()),
        ("binding-not-allowed", valid("root: {widget: \"column\", children: \"= $model.children\"}").into()),
        ("binding-eval", valid("root: {widget: \"text\", text: \"= 1 / 0\"}").into()),
        ("binding-type", valid("root: {widget: \"text\", text: \"= 1\"}").into()),
        ("model-path", valid("root: {widget: \"text\", text: \"x\"}").into()),
    ];
    let table: HashSet<_> = cases.iter().map(|(code, _)| *code).collect();
    for code in ALL_CODES {
        assert!(table.contains(code), "missing table entry: {code}");
    }
    for (expected, source) in cases {
        let diagnostics = match parse(&source) {
            Ok(mut d) if expected == "row-limit" => {
                let rows = (0..=MAX_ROWS).map(|i| json!({"id": i.to_string(), "cells": ["x"]})).collect();
                d.nodes["root"].ports.insert("rows".into(), JsonValue::Array(rows));
                lint(&d)
            }
            Ok(d) if expected == "model-path" => {
                let tree = resolve(&d).unwrap();
                bindings::reevaluate(&tree, &bindings::compile(&d).unwrap(), "wrong.x", &json!(1)).unwrap_err()
            }
            Ok(d) => lint(&d),
            Err(d) => d,
        };
        assert!(diagnostics.iter().any(|d| d.code == expected), "{expected}: {diagnostics:?}");
    }
}

#[test]
fn hundred_rows_parse() {
    let mut body = String::from("root: {widget: \"list\", rows: [");
    for i in 0..100 {
        if i > 0 {
            body.push(',');
        }
        body.push_str(&format!("{{id: \"{i}\", cells: [\"x\"]}}"));
    }
    body.push_str("], row: \"t\", row_height: 1}\nt: {widget: \"row\", children: []}");
    let start = std::time::Instant::now();
    assert!(parse(&format!("---\nscene: 1\nname: test\ncitizen: c\n---\n```mix\n{body}\n```\n")).is_ok());
    assert!(start.elapsed().as_millis() < 5, "100-row parse took {:?}", start.elapsed());
}

#[test]
fn ragged_rows_reject_missing_cell() {
    let d = doc(
        "root: {widget: \"list\", rows: [{id: \"a\", cells: [\"x\"]}, {id: \"b\", cells: []}], row: \"t\", row_height: 1}\nt: {widget: \"row\", children: [\"x\"]}\nx: {widget: \"text\", text: \"{cells[0]}\"}",
    );
    let codes: HashSet<_> = lint(&d).into_iter().map(|x| x.code).collect();
    assert!(codes.contains("cell-substitution"));
}

#[test]
fn binding_compile_rejects_bad_syntax_and_policy() {
    let d = doc("root: {widget: \"text\", text: \"= send \\\"x\\\" y\"}");
    let diagnostics = lint(&d);
    assert!(diagnostics.iter().any(|x| x.code == "binding-policy" && x.line > 0));
    let d = doc("root: {widget: \"text\", text: \"= ($model.x\"}");
    assert!(lint(&d).iter().any(|x| x.code == "invalid-binding"));
    let d = doc("root: {widget: \"text\", text: \"= $model.x\\n$model.y\"}");
    assert!(lint(&d).iter().any(|x| x.code == "invalid-binding"));
}

#[test]
fn no_v0_fixture_port_starts_with_equals() {
    for source in [C, F, S] {
        let d = parse(source).unwrap();
        assert!(d.nodes.values().flat_map(|n| n.ports.values()).all(|v| !v.as_str().is_some_and(|s| s.starts_with("= "))));
    }
}

#[test]
fn literal_leading_equals_escape() {
    let r = resolve(&doc("root: {widget: \"text\", text: \"== x\"}\na: {widget: \"text\", text: \"=x\"}")).unwrap();
    assert_eq!(r.nodes["root"].ports["text"], json!("= x"));
    assert_eq!(r.nodes["a"].ports["text"], json!("=x"));
}

#[test]
fn binding_deps_are_syntactic() {
    let d = doc("root: {widget: \"text\", text: \"= $model.a.b .. $model.c\"}");
    let set = bindings::compile(&d).unwrap();
    assert_eq!(set.bindings["root.text"].deps, ["model.a.b", "model.c"].into_iter().map(String::from).collect());
    let d = doc("root: {widget: \"text\", text: \"= $model.m[$model.k].x\"}");
    let set = bindings::compile(&d).unwrap();
    assert_eq!(set.bindings["root.text"].deps, ["model.m", "model.k"].into_iter().map(String::from).collect());
    let d = doc("root: {widget: \"list\", rows: [], row: \"t\", row_height: 1}\nt: {widget: \"text\", text: \"= $item.cells[0]\"}");
    assert!(bindings::compile(&d).unwrap().bindings["t.text"].reads_item);
    let d = doc("root: {widget: \"text\", text: \"= $item.cells[0]\"}");
    assert!(bindings::compile(&d).unwrap_err().iter().any(|d| d.code == "binding-policy"));
}

#[test]
fn model_patch_reevaluates_exactly_the_dirty_set() {
    let d = doc("root: {widget: \"column\", children: [\"a\",\"b\",\"c\"]}\na: {widget: \"text\", text: \"= $model.a\"}\nb: {widget: \"text\", text: \"= $model.b\"}\nc: {widget: \"text\", text: \"static\"}");
    let set = bindings::compile(&d).unwrap();
    let tree = resolve(&d).unwrap();
    let result = bindings::reevaluate(&tree, &set, "model.a", &json!("new")).unwrap();
    assert_eq!(result.evaluated, vec!["a.text"]);
}

#[test]
fn ancestor_descendant_dirty_relation() {
    let d = doc("root: {widget: \"column\", children: [\"a\",\"b\"]}\na: {widget: \"text\", text: \"= $model.a.b\"}\nb: {widget: \"text\", text: \"= $model.a\"}");
    let set = bindings::compile(&d).unwrap();
    let tree = resolve(&d).unwrap();
    assert_eq!(bindings::reevaluate(&tree, &set, "model.a", &json!({"b":"x"})).unwrap().evaluated, vec!["a.text", "b.text"]);
    assert!(bindings::reevaluate(&tree, &set, "model.z", &json!(1)).unwrap().evaluated.is_empty());
}

#[test]
fn eval_error_keeps_last_good_and_reports() {
    let mut d = doc("root: {widget: \"text\", text: \"= $model.fail ? 1 / 0 : $model.title\"}");
    d.model = Some(json!({"fail":false, "title":"last good"}));
    let set = bindings::compile(&d).unwrap();
    let tree = resolve(&d).unwrap();
    let result = bindings::reevaluate(&tree, &set, "model.fail", &json!(true)).unwrap();
    assert_eq!(result.tree.nodes["root"].ports["text"], json!("last good"));
    assert!(result.diagnostics.iter().any(|x| x.code == "binding-eval"));
}

#[test]
fn load_evaluates_against_envelope_model() {
    let mut d = doc("root: {widget: \"text\", text: \"= $model.title\"}");
    d.model = Some(json!({"title":"hello"}));
    assert_eq!(resolve(&d).unwrap().nodes["root"].ports["text"], json!("hello"));
    let set = bindings::compile(&d).unwrap();
    let patched = bindings::reevaluate(&resolve(&d).unwrap(), &set, "model.title", &json!("patched")).unwrap();
    assert_eq!(patched.tree.nodes["root"].ports["text"], json!("patched"));
    assert_eq!(resolve(&d).unwrap().nodes["root"].ports["text"], json!("hello"));
}

#[test]
fn binding_type_mismatch_keeps_last_good() {
    let mut d = doc("root: {widget: \"text\", text: \"= $model.value\"}");
    d.model = Some(json!({"value":"last good"}));
    let set = bindings::compile(&d).unwrap();
    let tree = resolve(&d).unwrap();
    let result = bindings::reevaluate(&tree, &set, "model.value", &json!(3)).unwrap();
    assert!(result.diagnostics.iter().any(|x| x.code == "binding-type"));
    assert_eq!(result.tree.nodes["root"].ports["text"], json!("last good"));
    assert!(result.changed.is_empty());
}

#[test]
fn structural_ports_reject_bindings() {
    let d = doc("root: {widget: \"column\", children: \"= $model.children\"}");
    assert!(lint(&d).iter().any(|x| x.code == "binding-not-allowed"));
}

#[test]
fn reeval_result_revalidated() {
    for (body, port, good, bad) in [
        (r#"root: {widget: "button", label: "x", tone: "= $model.value"}"#, "tone", json!("normal"), json!("bad")),
        (r#"root: {widget: "text", text: "x", size: "= $model.value"}"#, "size", json!(13), json!(-1)),
        ("root: {widget: \"list\", rows: \"= $model.value\", row: \"t\", row_height: 1}\nt: {widget: \"row\", children: []}", "rows", json!([]), json!([{}])),
    ] {
        let mut d = doc(body);
        d.model = Some(json!({"value":good}));
        let set = bindings::compile(&d).unwrap();
        let tree = resolve(&d).unwrap();
        let result = bindings::reevaluate(&tree, &set, "model.value", &bad).unwrap();
        assert_eq!(result.tree.nodes["root"].ports[port], tree.nodes["root"].ports[port]);
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.diagnostics[0].code, "binding-type");
        assert!(result.changed.is_empty());
    }
}

#[test]
fn noop_reeval_diff_is_empty() {
    let mut d = doc("root: {widget: \"text\", text: \"= $model.title\"}");
    d.model = Some(json!({"title":"x"}));
    let set = bindings::compile(&d).unwrap();
    let tree = resolve(&d).unwrap();
    assert!(bindings::reevaluate(&tree, &set, "model.title", &json!("x")).unwrap().changed.is_empty());
}

#[test]
fn clock_one_hz() {
    let d = doc("root: {widget: \"text\", text: \"= $model.now\"}");
    let set = bindings::compile(&d).unwrap();
    let tree = resolve(&d).unwrap();
    let a = bindings::reevaluate(&tree, &set, "model.now", &json!("one")).unwrap();
    assert_eq!(a.tree.nodes["root"].ports["text"], json!("one"));
    assert_eq!(a.evaluated, vec!["root.text"]);
    let b = bindings::reevaluate(&a.tree, &set, "model.now", &json!("two")).unwrap();
    assert_eq!(b.tree.nodes["root"].ports["text"], json!("two"));
    assert_eq!(b.evaluated, vec!["root.text"]);
}

#[test]
fn five_hundred_rows_patch_costs_two_bindings() {
    let d = doc("root: {widget: \"column\", children: [\"list\",\"count\"]}\nlist: {widget: \"list\", rows: \"= $model.entries\", row: \"template\", row_height: 1}\ncount: {widget: \"text\", text: \"= $model.entries[0].id\"}\ntemplate: {widget: \"row\", children: []}");
    let set = bindings::compile(&d).unwrap();
    let tree = resolve(&d).unwrap();
    let entries: Vec<_> = (0..500).map(|i| json!({"id": i.to_string(), "cells": ["x"]})).collect();
    let result = bindings::reevaluate(&tree, &set, "model.entries", &JsonValue::Array(entries)).unwrap();
    assert_eq!(result.evaluated, vec!["list.rows", "count.text"]);
}

#[test]
fn template_instantiate_binds_item() {
    let d = doc("root: {widget: \"list\", rows: [], row: \"template\", row_height: 1}\ntemplate: {widget: \"row\", children: [\"text\",\"static\"]}\ntext: {widget: \"text\", text: \"= $item.cells[0]\"}\nstatic: {widget: \"text\", text: \"static\"}");
    let set = bindings::compile(&d).unwrap();
    let tree = resolve(&d).unwrap();
    let item = json!({"cells":["row"]});
    let result = bindings::template_instantiate("text", &tree.nodes["text"], &set, &tree.model, &item).unwrap();
    assert_eq!(result.ports["text"], json!("row"));
    let result = bindings::template_instantiate("static", &tree.nodes["static"], &set, &tree.model, &item).unwrap();
    assert_eq!(result.ports["text"], json!("static"));
}

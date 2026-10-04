//! Mix Scenes: the renderer-neutral document core.
//!
//! A scene is a `---` envelope of headers (`scene`, `name`, `citizen` and
//! the optional JSON headers `window`, `subscribe`, `targets`, `model`)
//! followed by one ```` ```mix ```` fence holding a strict-data map of
//! nodes. This crate parses that document, lints it against the widget
//! schema, resolves it to a tree with defaults filled and bindings
//! evaluated, diffs two resolved trees into incremental ops, and writes a
//! document back out as source.
//!
//! Numeric ports are normalised to JSON `f64` values, so the wire form of
//! `13` is `13.0`. Incremental application order is Remove, Insert,
//! SetPort, then Reparent. A removed port is `SetPort { value: null }`.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

pub mod bindings;
mod envelope;
pub mod evaluator;
pub mod fixtures;
mod schema;
mod source;

pub use schema::{PortDescribe, describe};
pub use source::to_source;
use schema::{check_port_value, normalize_number, port_for, schema};

#[cfg(test)]
mod tests;

/// The largest document, and the largest patch value, accepted.
pub const MAX_DOCUMENT_BYTES: usize = 256 * 1024;
/// The most nodes a document may declare.
pub const MAX_NODES: usize = 2_000;
/// The most rows a list may hold.
pub const MAX_ROWS: usize = 500;
/// Every diagnostic code this crate emits.
pub const ALL_CODES: &[&str] = &[
    "document-too-large",
    "envelope",
    "missing-header",
    "scene-version",
    "invalid-name",
    "fence-count",
    "mix-parse",
    "strict-data",
    "duplicate-id",
    "root-type",
    "node-type",
    "missing-widget",
    "invalid-id",
    "header-json",
    "node-limit",
    "unknown-family",
    "unknown-port",
    "port-type",
    "enum-value",
    "port-min",
    "layout-conflict",
    "missing-port",
    "dangling-child",
    "child-type",
    "window-kind",
    "row-limit",
    "row-type",
    "invalid-template",
    "cell-substitution",
    "missing-root",
    "multiple-parents",
    "window-disagreement",
    "window-dialog-edge",
    "orphan-node",
    "cycle",
    "invalid-binding",
    "binding-policy",
    "binding-not-allowed",
    "binding-eval",
    "binding-type",
    "model-path",
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: String,
    pub line: usize,
    pub message: String,
}

impl Diagnostic {
    fn error(c: impl Into<String>, l: usize, m: impl Into<String>) -> Self {
        Self { severity: Severity::Error, code: c.into(), line: l, message: m.into() }
    }
    fn warning(c: impl Into<String>, l: usize, m: impl Into<String>) -> Self {
        Self { severity: Severity::Warning, code: c.into(), line: l, message: m.into() }
    }
}

/// A parsed document: headers and authored nodes, before defaults and
/// bindings are applied.
#[derive(Clone, Debug, PartialEq)]
pub struct SceneDocument {
    pub name: String,
    pub citizen: String,
    pub window: Option<JsonValue>,
    pub subscribe: Option<JsonValue>,
    pub targets: Option<JsonValue>,
    pub model: Option<JsonValue>,
    pub nodes: IndexMap<String, RawNode>,
    source: String,
    preparation: PreparationCache,
}

// The cache is not document identity. Public fields stay editable; prepare
// compares every input it uses before reusing a compilation and evaluation.
#[derive(Debug, Default)]
struct PreparationCache(std::sync::Mutex<Option<CachedPreparation>>);

impl Clone for PreparationCache {
    fn clone(&self) -> Self {
        Self(std::sync::Mutex::new(self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()))
    }
}

impl PartialEq for PreparationCache {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

#[derive(Clone, Debug)]
struct CachedPreparation {
    nodes: IndexMap<String, RawNode>,
    model: Option<JsonValue>,
    window: Option<JsonValue>,
    prepared: PreparedBindings,
}

/// A node as authored: its widget family and literal or bound ports.
#[derive(Clone, Debug, PartialEq)]
pub struct RawNode {
    pub widget: String,
    pub ports: IndexMap<String, JsonValue>,
    pub line: usize,
}

/// A resolved node: defaults filled, bindings evaluated.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub family: String,
    pub ports: IndexMap<String, JsonValue>,
    pub line: usize,
    pub is_template: bool,
}

/// A resolved document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResolvedScene {
    pub name: String,
    pub citizen: String,
    pub window: Option<JsonValue>,
    pub subscribe: Option<JsonValue>,
    pub nodes: IndexMap<String, Node>,
    /// The list row templates (the roots only, not their descendants).
    pub templates: Vec<String>,
    #[serde(default = "empty_model", skip_serializing_if = "is_empty_model")]
    pub model: JsonValue,
    /// `node.port` to binding source for every bound port.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub bindings: BTreeMap<String, String>,
}

fn empty_model() -> JsonValue {
    json!({})
}

fn is_empty_model(value: &JsonValue) -> bool {
    value.as_object().is_some_and(|m| m.is_empty())
}

/// One incremental change between two resolved trees.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Op {
    Insert { id: String, parent: Option<String>, index: usize, node: Node },
    Remove { id: String },
    SetPort { id: String, port: String, value: JsonValue },
    Reparent { id: String, parent: Option<String>, index: usize },
    SetScene { field: String, value: JsonValue },
}

/// Smallest and largest logical size a dialog may author for `w` and `h`.
pub const DIALOG_MIN_PX: f64 = 240.0;
pub const DIALOG_MAX_PX: f64 = 2048.0;

/// Validate a window declaration: the envelope `window` header (`node:
/// false`) or a window-family node (`node: true`).
///
/// - `kind` is `edge` or `dialog`; it is required on a node (the schema says
///   so) and optional on the header, where absence means `edge`.
/// - `edge` must be one of the four edges; `w`/`h` non-negative numbers;
///   `title` a string.
/// - A `dialog` takes no `edge` or `panel` (`window-dialog-edge`): it is
///   centred, not on an edge, and has no carousel page. It needs numeric `w`
///   and `h` in `DIALOG_MIN_PX..=DIALOG_MAX_PX`.
fn check_window_declaration<'a>(get: impl Fn(&str) -> Option<&'a JsonValue>, line: usize, node: bool, out: &mut Vec<Diagnostic>) {
    let kind = match get("kind") {
        None if node => return, // missing-port is reported by the schema check
        None => "edge",
        Some(value) => match value.as_str() {
            Some(k @ ("edge" | "dialog")) => k,
            _ => {
                out.push(Diagnostic::error("window-kind", line, "window kind must be edge or dialog"));
                return;
            }
        },
    };
    if !node {
        if let Some(edge) = get("edge")
            && !matches!(edge.as_str(), Some("right" | "left" | "top" | "bottom"))
        {
            out.push(Diagnostic::error("enum-value", line, "window.edge must be right, left, top or bottom"));
        }
        for key in ["w", "h"] {
            if get(key).is_some_and(|v| v.as_f64().is_none_or(|n| n < 0.0)) {
                out.push(Diagnostic::error("port-type", line, format!("window.{key} must be a non-negative number")));
            }
        }
        if get("title").is_some_and(|v| !v.is_string()) {
            out.push(Diagnostic::error("port-type", line, "window.title must be a string"));
        }
    }
    if kind != "dialog" {
        return;
    }
    for key in ["edge", "panel"] {
        if get(key).is_some() {
            out.push(Diagnostic::error(
                "window-dialog-edge",
                line,
                format!("a dialog window takes no {key}: it is centred, not mounted on an edge"),
            ));
        }
    }
    for key in ["w", "h"] {
        match get(key).and_then(JsonValue::as_f64) {
            None if get(key).is_none() => out.push(Diagnostic::error("missing-port", line, format!("a dialog window needs {key}"))),
            Some(n) if (DIALOG_MIN_PX..=DIALOG_MAX_PX).contains(&n) => {}
            _ => out.push(Diagnostic::error("port-type", line, format!("dialog {key} must be a number in 240..=2048"))),
        }
    }
}

// Checked before defaults are inserted: even `fill: false` beside `grow` is
// ambiguous author intent, so the legacy shorthand must go first.
fn check_layout_declarations(id: &str, node: &RawNode, out: &mut Vec<Diagnostic>) {
    let ports = &node.ports;
    let explicit = ["grow", "shrink", "basis"].iter().any(|p| ports.contains_key(*p));
    let conflict = (ports.contains_key("fill") && explicit)
        || (node.widget == "spacer" && ports.contains_key("size") && explicit)
        || (node.widget == "row" && ports.contains_key("height") && explicit);
    if conflict {
        out.push(Diagnostic::error(
            "layout-conflict",
            node.line,
            format!("{id}: remove legacy fill / fixed row height / spacer size before conflicting explicit flex sizing"),
        ));
    }
}

pub(crate) fn check_layout_bounds(id: &str, node: &Node, out: &mut Vec<Diagnostic>) {
    for (min, max) in [("min_width", "max_width"), ("min_height", "max_height")] {
        if let (Some(low), Some(high)) = (node.ports.get(min).and_then(JsonValue::as_f64), node.ports.get(max).and_then(JsonValue::as_f64))
            && low > high
        {
            out.push(Diagnostic::error("layout-conflict", node.line, format!("{id}: {min} exceeds {max}")));
        }
    }
    if node.family == "list" && node.ports.contains_key("max_rows") && node.ports.contains_key("max_height") {
        out.push(Diagnostic::error(
            "layout-conflict",
            node.line,
            format!("{id}: max_rows and max_height both constrain the list viewport"),
        ));
    }
}

pub(crate) fn port_changes(old: &ResolvedScene, new: &ResolvedScene) -> Vec<(String, JsonValue)> {
    let mut out = Vec::new();
    for (id, node) in &new.nodes {
        if let Some(previous) = old.nodes.get(id) {
            for (port, value) in &node.ports {
                if previous.ports.get(port) != Some(value) {
                    out.push((format!("{id}.{port}"), value.clone()));
                }
            }
            for port in previous.ports.keys() {
                if !node.ports.contains_key(port) {
                    out.push((format!("{id}.{port}"), JsonValue::Null));
                }
            }
        }
    }
    out
}

/// Parse scene source into a document. Errors are the diagnostics that
/// stopped the parse, sorted by line.
pub fn parse(source: &str) -> Result<SceneDocument, Vec<Diagnostic>> {
    let mut ds = Vec::new();
    if source.len() > MAX_DOCUMENT_BYTES {
        return Err(vec![Diagnostic::error("document-too-large", 1, "scene document exceeds 256 KiB")]);
    }
    let msg = match envelope::parse(source) {
        Ok(m) => m,
        Err(message) => return Err(vec![Diagnostic::error("envelope", 1, message)]),
    };
    for k in ["scene", "name", "citizen"] {
        if msg.get(k).is_none() {
            ds.push(Diagnostic::error("missing-header", 1, format!("missing required header {k}")));
        }
    }
    if msg.get("scene") != Some("1") {
        ds.push(Diagnostic::error("scene-version", 1, "scene header must be 1"));
    }
    if let Some(n) = msg.get("name")
        && !valid_name(n)
    {
        ds.push(Diagnostic::error("invalid-name", 1, "invalid scene name"));
    }
    let bs = source.find("\n---\n").map(|i| i + 5).unwrap_or(source.len());
    let body = msg.body.clone();
    let base = source[..bs].lines().count();
    let fs = fence_ranges(&body);
    if fs.len() != 1 {
        let line = fs.get(1).map_or(base + 1, |(open, _, _)| base + open);
        ds.push(Diagnostic::error("fence-count", line, "body must contain exactly one ```mix fence"));
        return Err(ds);
    }
    let (open, _, interior) = fs[0].clone();
    let value = match strict::parse(&interior) {
        Ok(v) => v,
        Err(e) => {
            ds.push(data_diagnostic(&e, base + open));
            return Err(ds);
        }
    };
    let Some(map) = value.as_map() else {
        ds.push(Diagnostic::error("root-type", base + open + 1, "fence must contain a map of nodes"));
        return Err(ds);
    };
    let mut nodes = IndexMap::new();
    for (id, v) in map {
        let line = base + open + line_in(&interior, id).unwrap_or(1);
        if id.contains('@') {
            ds.push(Diagnostic::error("invalid-id", line, "@ is reserved for template instance ids"));
        }
        let Some(fields) = v.as_map() else {
            ds.push(Diagnostic::error("node-type", line, format!("node {id} must be a map")));
            continue;
        };
        let widget = match fields.get("widget").and_then(strict::Value::as_str) {
            Some(x) => x.to_string(),
            None => {
                ds.push(Diagnostic::error("missing-widget", line, format!("node {id} is missing widget")));
                continue;
            }
        };
        nodes.insert(
            id.clone(),
            RawNode {
                widget,
                ports: fields.iter().filter(|(k, _)| k.as_str() != "widget").map(|(k, v)| (k.clone(), strict::to_json(v))).collect(),
                line,
            },
        );
    }
    if !ds.is_empty() {
        return Err(sorted(ds));
    }
    let document = SceneDocument {
        name: msg.get("name").unwrap_or_default().into(),
        citizen: msg.get("citizen").unwrap_or_default().into(),
        window: header_json(msg.get("window"), &mut ds, 1),
        subscribe: header_json(msg.get("subscribe"), &mut ds, 1),
        targets: header_json(msg.get("targets"), &mut ds, 1),
        model: header_json(msg.get("model"), &mut ds, 1),
        nodes,
        source: source.into(),
        preparation: PreparationCache::default(),
    };
    if ds.is_empty() { Ok(document) } else { Err(sorted(ds)) }
}

/// Every diagnostic for `doc`: structure, schema, bindings and the
/// evaluation of each binding against the envelope model.
pub fn lint(doc: &SceneDocument) -> Vec<Diagnostic> {
    prepare(doc).diagnostics
}

// One compilation and one evaluation pass, shared by lint and resolve.
#[derive(Clone, Debug)]
struct PreparedBindings {
    set: bindings::BindingSet,
    values: BTreeMap<String, Option<JsonValue>>,
    diagnostics: Vec<Diagnostic>,
}

fn prepare(doc: &SceneDocument) -> PreparedBindings {
    let mut cache = doc.preparation.0.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(cached) = cache.as_ref()
        && cached.nodes == doc.nodes
        && cached.model == doc.model
        && cached.window == doc.window
    {
        return cached.prepared.clone();
    }
    let mut prepared = PreparedBindings { set: bindings::BindingSet::default(), values: BTreeMap::new(), diagnostics: Vec::new() };
    match bindings::compile(doc) {
        Ok(set) => {
            prepared.diagnostics.extend(set.diagnostics.clone());
            prepared.set = set;
        }
        Err(ds) => prepared.diagnostics.extend(ds),
    }
    let mut out = lint_structure(doc);
    out.append(&mut prepared.diagnostics);
    if !out.iter().any(|d| d.severity == Severity::Error) {
        let model = doc.model.clone().unwrap_or_else(empty_model);
        let started = std::time::Instant::now();
        for path in &prepared.set.order {
            let Some(binding) = prepared.set.bindings.get(path) else { continue };
            if binding.reads_item {
                continue;
            }
            let Some((id, port)) = path.rsplit_once('.') else { continue };
            let Some(node) = doc.nodes.get(id) else { continue };
            let Some(schema_port) = port_for(&node.widget, port) else { continue };
            match bindings::evaluate_for_resolve(binding, &model, schema_port, started) {
                Ok(value) => {
                    prepared.values.insert(path.clone(), value);
                }
                Err(message) => out.push(Diagnostic::warning(
                    if message == "binding-type" { "binding-type" } else { "binding-eval" },
                    node.line,
                    format!("binding evaluation failed for {path}: {message}"),
                )),
            }
        }
    }
    prepared.diagnostics = sorted(out);
    *cache = Some(CachedPreparation {
        nodes: doc.nodes.clone(),
        model: doc.model.clone(),
        window: doc.window.clone(),
        prepared: prepared.clone(),
    });
    prepared
}

fn lint_structure(doc: &SceneDocument) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    if doc.source.len() > MAX_DOCUMENT_BYTES {
        out.push(Diagnostic::error("document-too-large", 1, "scene document exceeds 256 KiB"));
    }
    if doc.nodes.len() > MAX_NODES {
        out.push(Diagnostic::error("node-limit", 1, "scene has more than 2,000 nodes"));
    }
    let mut parents: HashMap<String, usize> = HashMap::new();
    let mut template_nodes = HashSet::new();
    for (id, n) in &doc.nodes {
        if id.contains('@') {
            out.push(Diagnostic::error("invalid-id", n.line, "@ is reserved for template instance ids"));
        }
        let Some(ps) = schema(&n.widget) else {
            out.push(Diagnostic::error("unknown-family", n.line, format!("unknown widget family {}", n.widget)));
            continue;
        };
        check_layout_declarations(id, n, &mut out);
        check_layout_bounds(id, &Node { family: n.widget.clone(), ports: n.ports.clone(), line: n.line, is_template: false }, &mut out);
        for (k, v) in &n.ports {
            let Some(p) = ps.iter().find(|p| p.name == k) else {
                out.push(Diagnostic::error("unknown-port", n.line, format!("unknown port {k} on {id}")));
                continue;
            };
            if bindings::binding_source(v).is_none() {
                check_port_value(id, n.line, *p, v, &mut out);
            }
        }
        for p in ps {
            if p.required && !n.ports.contains_key(p.name) {
                out.push(Diagnostic::error("missing-port", n.line, format!("missing required port {} on {id}", p.name)));
            }
        }
        if let Some(cs) = n.ports.get("children").and_then(JsonValue::as_array) {
            for (i, c) in cs.iter().enumerate() {
                if let Some(c) = c.as_str() {
                    if !doc.nodes.contains_key(c) {
                        out.push(Diagnostic::error("dangling-child", n.line, format!("{id} refers to missing child {c}")));
                    } else {
                        *parents.entry(c.into()).or_default() += 1;
                    }
                } else {
                    out.push(Diagnostic::error("child-type", n.line, format!("child {i} of {id} is not a string")));
                }
            }
        }
        if n.widget == "window" {
            check_window_declaration(|k| n.ports.get(k), n.line, true, &mut out);
        }
        if n.widget == "list"
            && let Some(row) = n.ports.get("row").and_then(JsonValue::as_str)
        {
            if doc.nodes.values().any(|parent| {
                parent.ports.get("children").and_then(JsonValue::as_array).is_some_and(|cs| cs.iter().any(|c| c.as_str() == Some(row)))
            }) {
                out.push(Diagnostic::error("invalid-template", n.line, "list row template must not also be a child"));
            }
            match doc.nodes.get(row) {
                Some(t) => {
                    let mut seen = HashSet::new();
                    check_template(row, t, doc, minimum_cells(n), &mut out, &mut seen);
                    template_nodes.extend(seen);
                }
                None => out.push(Diagnostic::error("dangling-child", n.line, format!("missing row template {row}"))),
            }
        }
    }
    for (id, node) in &doc.nodes {
        if !template_nodes.contains(id)
            && node.ports.iter().any(|(port, value)| {
                matches!((node.widget.as_str(), port.as_str()), ("text", "text") | ("image", "src"))
                    && value.as_str().is_some_and(|s| s.contains("{cells["))
            })
        {
            out.push(Diagnostic::error(
                "cell-substitution",
                node.line,
                "cell substitution is allowed only in text.text and image.src inside list templates",
            ));
        }
    }
    if !doc.nodes.contains_key("root") {
        out.push(Diagnostic::error("missing-root", 1, "scene must contain a node named root"));
    }
    if parents.values().any(|n| *n > 1) {
        out.push(Diagnostic::error("multiple-parents", 1, "a node has more than one parent"));
    }
    if let Some(w) = &doc.window {
        if w.get("chrome").is_some_and(|value| !value.is_boolean()) {
            out.push(Diagnostic::error("port-type", 1, "window.chrome must be bool"));
        }
        check_window_declaration(|k| w.get(k), 1, false, &mut out);
        for n in doc.nodes.values().filter(|n| n.widget == "window") {
            if ["kind", "edge", "title", "w", "h", "chrome"].iter().any(|k| n.ports.get(*k) != w.get(*k)) {
                out.push(Diagnostic::error("window-disagreement", n.line, "window envelope and window node disagree"));
            }
        }
    }
    let reach = reachable(doc);
    for id in doc.nodes.keys() {
        if id != "root" && !reach.contains(id) {
            out.push(Diagnostic::warning("orphan-node", doc.nodes[id].line, format!("node {id} is unreachable from root")));
        }
    }
    if has_cycle(doc) {
        out.push(Diagnostic::error("cycle", 1, "scene child graph contains a cycle"));
    }
    sorted(out)
}

/// Resolve `doc`: fill defaults, evaluate every model binding and mark
/// the list row templates. Errors are the lint diagnostics.
pub fn resolve(doc: &SceneDocument) -> Result<ResolvedScene, Vec<Diagnostic>> {
    let prepared = prepare(doc);
    if prepared.diagnostics.iter().any(|d| d.severity == Severity::Error) {
        return Err(prepared.diagnostics);
    }
    let ts: BTreeSet<String> = doc
        .nodes
        .values()
        .filter(|node| node.widget == "list")
        .filter_map(|node| node.ports.get("row").and_then(JsonValue::as_str))
        .map(str::to_owned)
        .collect();
    let binding_set = prepared.set;
    let mut nodes = IndexMap::new();
    for (id, r) in &doc.nodes {
        let Some(ps) = schema(&r.widget) else {
            return Err(vec![Diagnostic::error("unknown-family", r.line, "unknown widget family")]);
        };
        let mut ports: IndexMap<String, JsonValue> = ps
            .iter()
            .filter_map(|p| {
                r.ports
                    .get(p.name)
                    .filter(|v| bindings::binding_source(v).is_none())
                    .and_then(|v| bindings::escaped_literal(v).or_else(|| Some(v.clone())))
                    .or_else(|| p.default.and_then(|v| serde_json::from_str(v).ok()))
                    .map(|v| (p.name.into(), normalize_number(v)))
            })
            .collect();
        for p in ps {
            if let Some(value) = prepared.values.get(&format!("{id}.{}", p.name)) {
                bindings::set_port(&mut ports, p.name, value.clone());
            }
        }
        nodes.insert(id.clone(), Node { family: r.widget.clone(), ports, line: r.line, is_template: ts.contains(id) });
    }
    let mut templates: Vec<_> = ts.into_iter().collect();
    let mut conflicts = Vec::new();
    for (id, node) in &nodes {
        check_layout_bounds(id, node, &mut conflicts);
    }
    if !conflicts.is_empty() {
        return Err(sorted(conflicts));
    }
    templates.sort();
    Ok(ResolvedScene {
        name: doc.name.clone(),
        citizen: doc.citizen.clone(),
        window: doc.window.clone(),
        subscribe: doc.subscribe.clone(),
        nodes,
        templates,
        model: doc.model.clone().unwrap_or_else(empty_model),
        bindings: binding_set.bindings.into_iter().map(|(k, v)| (k, v.source)).collect(),
    })
}

/// The ops that turn `old` into `new`: removes, then port changes and
/// inserts in `new`'s node order, then reparents, then scene fields.
pub fn diff(old: &ResolvedScene, new: &ResolvedScene) -> Vec<Op> {
    let mut ops = Vec::new();
    for id in old.nodes.keys() {
        if !new.nodes.contains_key(id) {
            ops.push(Op::Remove { id: id.clone() });
        }
    }
    for (id, n) in &new.nodes {
        if let Some(o) = old.nodes.get(id) {
            let keys: BTreeSet<_> = o.ports.keys().chain(n.ports.keys()).collect();
            for k in keys {
                let v = n.ports.get(k).cloned().unwrap_or(JsonValue::Null);
                if o.ports.get(k) != Some(&v) {
                    ops.push(Op::SetPort { id: id.clone(), port: k.clone(), value: v });
                }
            }
        } else {
            let (p, i) = parent_of(new, id).map_or((None, 0), |(p, i)| (Some(p), i));
            ops.push(Op::Insert { id: id.clone(), parent: p, index: i, node: n.clone() });
        }
    }
    for id in new.nodes.keys() {
        if old.nodes.contains_key(id) && parent_of(old, id) != parent_of(new, id) {
            let (p, i) = parent_of(new, id).map_or((None, 0), |(p, i)| (Some(p), i));
            ops.push(Op::Reparent { id: id.clone(), parent: p, index: i });
        }
    }
    for (field, a, b) in [
        ("name", json!(old.name), json!(new.name)),
        ("citizen", json!(old.citizen), json!(new.citizen)),
        ("window", old.window.clone().unwrap_or(JsonValue::Null), new.window.clone().unwrap_or(JsonValue::Null)),
        ("subscribe", old.subscribe.clone().unwrap_or(JsonValue::Null), new.subscribe.clone().unwrap_or(JsonValue::Null)),
    ] {
        if a != b {
            ops.push(Op::SetScene { field: field.into(), value: b });
        }
    }
    ops
}

/// The source `d` was parsed from.
pub fn source(d: &SceneDocument) -> &str {
    &d.source
}

fn valid_name(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 2 && b.len() <= 31 && b[0].is_ascii_lowercase() && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

fn header_json(v: Option<&str>, ds: &mut Vec<Diagnostic>, line: usize) -> Option<JsonValue> {
    v.map(|s| match serde_json::from_str(s) {
        Ok(v) => normalize_number(v),
        Err(_) => {
            ds.push(Diagnostic::error("header-json", line, "header JSON is invalid"));
            JsonValue::Null
        }
    })
}

fn line_in(t: &str, id: &str) -> Option<usize> {
    t.lines()
        .position(|l| l.trim_start().strip_prefix(id).is_some_and(|rest| rest.trim_start().starts_with(':')))
        .map(|x| x + 1)
}

fn minimum_cells(n: &RawNode) -> usize {
    n.ports
        .get("rows")
        .and_then(JsonValue::as_array)
        .map(|rs| rs.iter().filter_map(|r| r.get("cells").and_then(JsonValue::as_array).map(Vec::len)).min().unwrap_or(usize::MAX))
        .unwrap_or(usize::MAX)
}

fn check_template(id: &str, n: &RawNode, d: &SceneDocument, cells: usize, o: &mut Vec<Diagnostic>, seen: &mut HashSet<String>) {
    if !seen.insert(id.to_owned()) {
        return;
    }
    if !["row", "column", "text", "spacer", "image", "list"].contains(&n.widget.as_str()) {
        o.push(Diagnostic::error("invalid-template", n.line, "list row template has an invalid family"));
        return;
    }
    for (k, v) in &n.ports {
        if let Some(s) = v.as_str() {
            let allows_cells = (n.widget == "text" && k == "text") || (n.widget == "image" && k == "src");
            if s.contains("{cells[") && !allows_cells {
                o.push(Diagnostic::error(
                    "cell-substitution",
                    n.line,
                    "cell substitution is allowed only in text.text and image.src inside list templates",
                ));
            }
            if allows_cells {
                for (start, _) in s.match_indices("{cells[") {
                    if let Some(e) = s[start + 7..].find("]}")
                        && s[start + 7..start + 7 + e].parse::<usize>().ok().is_none_or(|i| i >= cells)
                    {
                        o.push(Diagnostic::error("cell-substitution", n.line, "cell index is not present"));
                    }
                }
            }
        }
    }
    if let Some(cs) = n.ports.get("children").and_then(JsonValue::as_array) {
        for id in cs.iter().filter_map(JsonValue::as_str) {
            if let Some(c) = d.nodes.get(id) {
                check_template(id, c, d, cells, o, seen);
            }
        }
    }
}

fn fence_ranges(b: &str) -> Vec<(usize, usize, String)> {
    let l: Vec<_> = b.lines().collect();
    let mut r = Vec::new();
    let mut i = 0;
    while i < l.len() {
        if l[i].trim() == "```mix" {
            let s = i + 1;
            i += 1;
            while i < l.len() && l[i].trim() != "```" {
                i += 1;
            }
            if i < l.len() {
                r.push((s, i, l[s..i].join("\n")));
            }
        }
        i += 1;
    }
    r
}

fn parent_of(s: &ResolvedScene, id: &str) -> Option<(String, usize)> {
    for (p, n) in &s.nodes {
        if let Some(cs) = n.ports.get("children").and_then(JsonValue::as_array)
            && let Some(i) = cs.iter().position(|v| v.as_str() == Some(id))
        {
            return Some((p.clone(), i));
        }
    }
    None
}

fn reachable(d: &SceneDocument) -> HashSet<String> {
    let mut s = HashSet::new();
    fn go(id: &str, d: &SceneDocument, s: &mut HashSet<String>) {
        if !s.insert(id.into()) {
            return;
        }
        if let Some(n) = d.nodes.get(id)
            && let Some(cs) = n.ports.get("children").and_then(JsonValue::as_array)
        {
            for c in cs.iter().filter_map(JsonValue::as_str) {
                go(c, d, s);
            }
        }
    }
    if d.nodes.contains_key("root") {
        go("root", d, &mut s);
    }
    s.extend(bindings::template_ids(d));
    s
}

fn has_cycle(d: &SceneDocument) -> bool {
    fn go(id: &str, d: &SceneDocument, a: &mut HashSet<String>, done: &mut HashSet<String>) -> bool {
        if a.contains(id) {
            return true;
        }
        if done.contains(id) {
            return false;
        }
        a.insert(id.into());
        if let Some(n) = d.nodes.get(id)
            && let Some(cs) = n.ports.get("children").and_then(JsonValue::as_array)
        {
            for c in cs.iter().filter_map(JsonValue::as_str) {
                if go(c, d, a, done) {
                    return true;
                }
            }
        }
        a.remove(id);
        done.insert(id.into());
        false
    }
    let mut a = HashSet::new();
    let mut done = HashSet::new();
    d.nodes.keys().any(|id| go(id, d, &mut a, &mut done))
}

/// The diagnostic for a strict-data error in the node fence, offset to
/// the fence's position in the document.
fn data_diagnostic(e: &strict::Error, offset: usize) -> Diagnostic {
    let line = offset + e.line().unwrap_or(1);
    match e.kind() {
        strict::ErrorKind::DuplicateKey => Diagnostic::error("duplicate-id", line, e.message()),
        strict::ErrorKind::Violation => Diagnostic::error(
            "strict-data",
            line,
            match e.hint() {
                Some(hint) => format!("{}: {hint}", e.message()),
                None => e.message().to_owned(),
            },
        ),
        _ => Diagnostic::error("mix-parse", line, e.message()),
    }
}

fn sorted(mut d: Vec<Diagnostic>) -> Vec<Diagnostic> {
    d.sort_by(|a, b| a.line.cmp(&b.line).then(a.code.cmp(&b.code)));
    d
}

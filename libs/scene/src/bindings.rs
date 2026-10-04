//! Bindings: ports whose value is `= expression`, compiled once per
//! document, evaluated against the model at load and on each model patch,
//! and against each row inside a list template.

use crate::evaluator::{BindingError, BindingErrorKind, Compiled, DEFAULT_EVALUATOR, Evaluator, Limits};
use crate::schema::{Port, check_port_value, normalize_number, port_for};
use crate::{Diagnostic, MAX_ROWS, Node, ResolvedScene, SceneDocument, Severity};
use serde_json::{Value as JsonValue, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The wall-clock budget one evaluation pass (a load, a model patch, a
/// revision's template instantiation) may spend across all its bindings.
pub(crate) const EVALUATION_BUDGET: Duration = Duration::from_millis(250);

/// The most one expression may take inside the shared budget.
const EXPRESSION_LIMIT: Duration = Duration::from_millis(50);

#[cfg(test)]
thread_local! {
    static TEST_EVALUATION_BUDGET: std::cell::Cell<Duration> = const { std::cell::Cell::new(EVALUATION_BUDGET) };
    pub(crate) static COMPILE_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(crate) static EVALUATE_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn with_evaluation_budget<T>(budget: Duration, f: impl FnOnce() -> T) -> T {
    struct Restore(Duration);
    impl Drop for Restore {
        fn drop(&mut self) {
            TEST_EVALUATION_BUDGET.set(self.0);
        }
    }
    let _restore = Restore(TEST_EVALUATION_BUDGET.replace(budget));
    f()
}

fn evaluation_budget() -> Duration {
    #[cfg(test)]
    {
        TEST_EVALUATION_BUDGET.get()
    }
    #[cfg(not(test))]
    {
        EVALUATION_BUDGET
    }
}

/// One binding: its source, the model paths it depends on, whether it
/// reads the list row, and its compiled form.
#[derive(Clone)]
pub struct CompiledBinding {
    pub source: String,
    /// The `model` paths the expression reads (`model`, `model.a.b`).
    pub deps: BTreeSet<String>,
    /// The expression reads `$item`, so it only evaluates inside a template.
    pub reads_item: bool,
    compiled: Arc<dyn Compiled>,
}

impl CompiledBinding {
    /// Compile `source` with the default evaluator.
    pub fn new(source: &str) -> Result<Self, BindingError> {
        Self::with(DEFAULT_EVALUATOR, source)
    }

    /// Compile `source` with `evaluator`.
    pub fn with(evaluator: &dyn Evaluator, source: &str) -> Result<Self, BindingError> {
        let compiled = evaluator.compile(source)?;
        let deps = compiled
            .reads()
            .iter()
            .filter(|path| *path == "model" || path.starts_with("model."))
            .cloned()
            .collect();
        let reads_item = compiled.roots().contains("item");
        Ok(Self { source: source.to_owned(), deps, reads_item, compiled })
    }

    /// The globals the expression reads.
    pub fn roots(&self) -> &BTreeSet<String> {
        self.compiled.roots()
    }
}

// The compiled form is a function of the source and the evaluator, so two
// bindings are equal when their source and analysis agree.
impl PartialEq for CompiledBinding {
    fn eq(&self, other: &Self) -> bool {
        self.source == other.source && self.deps == other.deps && self.reads_item == other.reads_item
    }
}

impl fmt::Debug for CompiledBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompiledBinding")
            .field("source", &self.source)
            .field("deps", &self.deps)
            .field("reads_item", &self.reads_item)
            .finish()
    }
}

/// Every binding of a document, in document order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BindingSet {
    pub bindings: BTreeMap<String, CompiledBinding>,
    pub order: Vec<String>,
    pub diagnostics: Vec<Diagnostic>,
}

impl BindingSet {
    /// The bindings a resolved tree carries (`node.port` to source),
    /// recompiled with the default evaluator. They were policy-checked
    /// when the document resolved, so a source that fails to compile is
    /// skipped.
    pub fn from_resolved(tree: &ResolvedScene) -> Self {
        let mut set = Self::default();
        for (path, source) in &tree.bindings {
            if let Ok(binding) = CompiledBinding::new(source) {
                set.order.push(path.clone());
                set.bindings.insert(path.clone(), binding);
            }
        }
        set
    }
}

/// The result of one model patch.
#[derive(Clone, Debug)]
pub struct ReEval {
    pub tree: ResolvedScene,
    pub diagnostics: Vec<Diagnostic>,
    pub changed: Vec<(String, JsonValue)>,
    pub evaluated: Vec<String>,
}

/// The expression of a `= expression` port value, or `None` for a literal.
/// `=` alone is an empty expression; `== x` is the escaped literal `= x`.
pub(crate) fn binding_source(value: &JsonValue) -> Option<String> {
    let s = value.as_str()?;
    if s == "=" {
        Some(String::new())
    } else if s.starts_with("== ") {
        None
    } else if s.strip_prefix('=').is_some_and(|rest| rest.chars().next().is_some_and(char::is_whitespace)) {
        Some(s[1..].trim().into())
    } else {
        None
    }
}

pub(crate) fn escaped_literal(value: &JsonValue) -> Option<JsonValue> {
    value.as_str().and_then(|s| s.strip_prefix("== ").map(|x| json!(format!("= {x}"))))
}

/// Compile every binding of `doc` with the default evaluator.
pub fn compile(doc: &SceneDocument) -> Result<BindingSet, Vec<Diagnostic>> {
    compile_with(doc, DEFAULT_EVALUATOR)
}

/// Compile every binding of `doc` with `evaluator`. Structural ports
/// (`children`, `row`, `widget`) take no binding; an expression may read
/// only `$model`, and `$item` only inside a list template.
pub fn compile_with(doc: &SceneDocument, evaluator: &dyn Evaluator) -> Result<BindingSet, Vec<Diagnostic>> {
    #[cfg(test)]
    COMPILE_COUNT.with(|n| n.set(n.get() + 1));
    let mut set = BindingSet::default();
    let mut errors = Vec::new();
    let templates = template_ids(doc);
    for (id, node) in &doc.nodes {
        for (port, value) in &node.ports {
            let Some(source) = binding_source(value) else {
                continue;
            };
            let path = format!("{id}.{port}");
            set.order.push(path.clone());
            if matches!(port.as_str(), "children" | "row" | "widget") {
                errors.push(Diagnostic::error("binding-not-allowed", node.line, format!("binding is not allowed on {path}")));
                continue;
            }
            let binding = match CompiledBinding::with(evaluator, &source) {
                Ok(binding) => binding,
                Err(error) => {
                    let code = match error.kind {
                        BindingErrorKind::NotAllowed => "binding-policy",
                        _ => "invalid-binding",
                    };
                    errors.push(Diagnostic::error(code, node.line, format!("invalid binding on {path}: {error}")));
                    continue;
                }
            };
            let bad_root = binding.roots().iter().any(|root| root != "model" && root != "item");
            if bad_root || (binding.reads_item && !templates.contains(id)) {
                errors.push(Diagnostic::error("binding-policy", node.line, format!("binding on {path} reads a disallowed root")));
            }
            set.bindings.insert(path, binding);
        }
    }
    if errors.iter().any(|d| d.severity == Severity::Error) {
        Err(errors)
    } else {
        set.diagnostics = errors;
        Ok(set)
    }
}

/// Apply a model patch at `path` (`model` or `model.a.b`) and re-evaluate
/// the bindings it dirties. A failed binding keeps its last good port and
/// reports a warning; a layout conflict refuses the whole patch.
pub fn reevaluate(tree: &ResolvedScene, set: &BindingSet, path: &str, value: &JsonValue) -> Result<ReEval, Vec<Diagnostic>> {
    if too_large(value) {
        return Err(vec![Diagnostic::error("model-path", 1, "patch value too large")]);
    }
    let parts: Vec<_> = path.split('.').collect();
    if parts.is_empty() || parts.iter().any(|p| p.is_empty()) || parts[0] != "model" {
        return Err(vec![Diagnostic::error("model-path", 1, "model patch path must start with model")]);
    }
    let mut next = tree.clone();
    if !apply_model_patch(&mut next.model, &parts[1..], value) {
        return Err(vec![Diagnostic::error("model-path", 1, "model patch path must contain map keys")]);
    }
    // A sequence of individually small patches must not grow an unbounded
    // model. The host also bounds authored ports and metadata together with
    // this model before committing a revision.
    if too_large(&next.model) {
        return Err(vec![Diagnostic::error("model-path", 1, "aggregate model too large")]);
    }
    let old = tree.clone();
    let mut diagnostics = Vec::new();
    let mut evaluated = Vec::new();
    let model = next.model.clone();
    let started = Instant::now();
    let patch = parts.join(".");
    for path in &set.order {
        let Some(binding) = set.bindings.get(path) else {
            continue;
        };
        if binding.reads_item || !binding.deps.iter().any(|dep| related(dep, &patch)) {
            continue;
        }
        let Some((id, port)) = path.rsplit_once('.') else {
            continue;
        };
        let Some(node) = next.nodes.get_mut(id) else {
            continue;
        };
        let result = evaluate_budgeted(binding, &model, None, started, || evaluated.push(path.clone()))
            .and_then(|v| coerce_port(&v, port_for(&node.family, port)));
        match result {
            Ok(v) => set_port(&mut node.ports, port, v),
            Err(code) => diagnostics.push(Diagnostic::warning(
                if code == "binding-type" { "binding-type" } else { "binding-eval" },
                node.line,
                format!("binding evaluation failed for {path}: {code}"),
            )),
        }
    }
    let mut conflicts = Vec::new();
    for (id, node) in &next.nodes {
        crate::check_layout_bounds(id, node, &mut conflicts);
    }
    if !conflicts.is_empty() {
        return Err(conflicts);
    }
    let changed = crate::port_changes(&old, &next);
    Ok(ReEval { tree: next, diagnostics, changed, evaluated })
}

/// Whether `value` serialises past the document limit (or not at all).
fn too_large(value: &JsonValue) -> bool {
    serde_json::to_vec(value).ok().is_none_or(|bytes| bytes.len() > crate::MAX_DOCUMENT_BYTES)
}

/// Instantiate one template node for one row, with a fresh budget.
pub fn template_instantiate(id: &str, node: &Node, set: &BindingSet, model: &JsonValue, item: &JsonValue) -> Result<Node, Diagnostic> {
    template_instantiate_with(id, node, set, item, &mut TemplateEvaluation::new(model))
}

/// One aggregate budget for a scene revision, shared by every list and row.
pub struct TemplateEvaluation {
    model: JsonValue,
    started: Instant,
    remaining_nodes: usize,
}

/// The most template nodes one revision may instantiate.
pub const MAX_TEMPLATE_NODES: usize = 16_384;

impl TemplateEvaluation {
    pub fn new(model: &JsonValue) -> Self {
        Self { model: model.clone(), started: Instant::now(), remaining_nodes: MAX_TEMPLATE_NODES }
    }
}

/// Instantiate one template node for one row under a shared time and work
/// budget.
pub fn template_instantiate_with(
    id: &str,
    node: &Node,
    set: &BindingSet,
    item: &JsonValue,
    evaluation: &mut TemplateEvaluation,
) -> Result<Node, Diagnostic> {
    if evaluation.remaining_nodes == 0 || evaluation.started.elapsed() >= EVALUATION_BUDGET {
        return Err(Diagnostic::warning("binding-eval", node.line, "template instantiation budget exhausted"));
    }
    evaluation.remaining_nodes -= 1;
    let mut out = node.clone();
    // Range by node instead of scanning every document binding for every row.
    let prefix = format!("{id}.");
    for (path, binding) in set.bindings.range(prefix.clone()..) {
        if !path.starts_with(&prefix) {
            break;
        }
        let Some((binding_id, port)) = path.rsplit_once('.') else { continue };
        if binding_id != id {
            continue;
        }
        let value = evaluate_budgeted(binding, &evaluation.model, Some(item), evaluation.started, || {})
            .and_then(|v| coerce_port(&v, port_for(&node.family, port)))
            .map_err(|code| {
                Diagnostic::warning(
                    if code == "binding-type" { "binding-type" } else { "binding-eval" },
                    node.line,
                    format!("template binding failed for {path}: {code}"),
                )
            })?;
        set_port(&mut out.ports, port, value);
    }
    let mut conflicts = Vec::new();
    crate::check_layout_bounds(id, &out, &mut conflicts);
    if let Some(conflict) = conflicts.into_iter().next() {
        return Err(conflict);
    }
    if evaluation.started.elapsed() >= EVALUATION_BUDGET {
        return Err(Diagnostic::warning("binding-eval", node.line, "template instantiation budget exhausted"));
    }
    Ok(out)
}

/// Evaluate one binding inside the pass budget that began at `started`.
/// The error is the message for a `binding-eval` diagnostic.
pub(crate) fn evaluate_budgeted(
    binding: &CompiledBinding,
    model: &JsonValue,
    item: Option<&JsonValue>,
    started: Instant,
    on_evaluate: impl FnOnce(),
) -> Result<JsonValue, String> {
    let budget = evaluation_budget();
    let remaining = budget
        .checked_sub(started.elapsed())
        .filter(|d| !d.is_zero())
        .ok_or_else(|| "evaluation budget exhausted".to_string())?;
    on_evaluate();
    #[cfg(test)]
    EVALUATE_COUNT.with(|n| n.set(n.get() + 1));
    let mut globals = vec![("model", model)];
    if let Some(item) = item {
        globals.push(("item", item));
    }
    let limits = Limits {
        max_string_len: 1 << 20,
        max_list_len: MAX_ROWS,
        max_map_len: 1024,
        time_limit: remaining.min(EXPRESSION_LIMIT),
    };
    let result = binding.compiled.eval(&globals, &limits).map_err(|e| e.message);
    // A result that finished after the shared deadline is refused too, so
    // the last binding of a pass cannot slip past the budget.
    if started.elapsed() >= budget {
        return Err("evaluation budget exhausted".into());
    }
    result
}

/// Check a bound value against its port: `nil` clears the port to its
/// default; a wrong type or an out-of-range value is `binding-type`.
pub(crate) fn coerce_port(value: &JsonValue, port: Option<Port>) -> Result<Option<JsonValue>, String> {
    let Some(port) = port else {
        return Err("binding-type".into());
    };
    if value.is_null() {
        return Ok(port.default.and_then(|x| serde_json::from_str(x).ok()).map(normalize_number));
    }
    let ok = matches!(
        (port.ty, value),
        ("string", JsonValue::String(_))
            | ("number", JsonValue::Number(_))
            | ("bool", JsonValue::Bool(_))
            | ("list", JsonValue::Array(_))
            | ("object", JsonValue::Object(_))
    );
    if !ok {
        return Err("binding-type".into());
    }
    let mut ds = Vec::new();
    check_port_value("bound", 1, port, value, &mut ds);
    if ds.iter().any(|d| d.severity == Severity::Error) {
        Err("binding-type".into())
    } else {
        Ok(Some(normalize_number(value.clone())))
    }
}

pub(crate) fn evaluate_for_resolve(
    binding: &CompiledBinding,
    model: &JsonValue,
    port: Port,
    started: Instant,
) -> Result<Option<JsonValue>, String> {
    let value = evaluate_budgeted(binding, model, None, started, || {})?;
    coerce_port(&value, Some(port))
}

pub(crate) fn set_port(ports: &mut indexmap::IndexMap<String, JsonValue>, port: &str, value: Option<JsonValue>) {
    if let Some(value) = value {
        ports.insert(port.into(), value);
    } else {
        ports.shift_remove(port);
    }
}

fn apply_model_patch(model: &mut JsonValue, parts: &[&str], value: &JsonValue) -> bool {
    if parts.is_empty() {
        if value.is_null() {
            *model = json!({});
            return true;
        }
        if !value.is_object() {
            return false;
        }
        *model = value.clone();
        return true;
    }
    if parts.iter().any(|p| p.is_empty()) {
        return false;
    }
    if !model.is_object() {
        *model = json!({});
    }
    let mut current = model;
    for part in &parts[..parts.len() - 1] {
        let Some(map) = current.as_object_mut() else {
            return false;
        };
        if value.is_null() && !map.contains_key(*part) {
            return true;
        }
        current = map.entry(*part).or_insert_with(|| json!({}));
        if !current.is_object() {
            return false;
        }
    }
    let Some(map) = current.as_object_mut() else {
        return false;
    };
    if value.is_null() {
        map.remove(parts[parts.len() - 1]);
    } else {
        map.insert(parts[parts.len() - 1].into(), value.clone());
    }
    true
}

fn related(dep: &str, patch: &str) -> bool {
    dep == patch || dep.starts_with(&(patch.to_owned() + ".")) || patch.starts_with(&(dep.to_owned() + "."))
}

/// Every node reachable from a list row template.
pub(crate) fn template_ids(doc: &SceneDocument) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    fn visit(id: &str, doc: &SceneDocument, ids: &mut BTreeSet<String>) {
        if !ids.insert(id.into()) {
            return;
        }
        if let Some(node) = doc.nodes.get(id) {
            if node.widget == "list"
                && let Some(row) = node.ports.get("row").and_then(JsonValue::as_str)
            {
                visit(row, doc, ids);
            }
            if let Some(children) = node.ports.get("children").and_then(JsonValue::as_array) {
                for child in children.iter().filter_map(JsonValue::as_str) {
                    visit(child, doc, ids);
                }
            }
        }
    }
    for row in doc.nodes.values().filter(|n| n.widget == "list").filter_map(|n| n.ports.get("row").and_then(JsonValue::as_str)) {
        visit(row, doc, &mut ids);
    }
    ids
}

#[cfg(test)]
mod template_budget_tests {
    use super::*;

    #[test]
    fn all_rows_share_work_and_time_limits() {
        let doc = crate::parse("---\nscene: 1\nname: budget\ncitizen: test\nmodel: {\"prefix\":\"live \"}\n---\n```mix\nroot: {widget: \"list\", rows: [], row: \"t\", row_height: 20}\nt: {widget: \"text\", text: \"= $model.prefix .. $item.cells[0]\"}\n```\n").unwrap();
        let tree = crate::resolve(&doc).unwrap();
        let set = compile(&doc).unwrap();
        let mut evaluation = TemplateEvaluation::new(&tree.model);
        evaluation.remaining_nodes = 2;
        for value in ["one", "two"] {
            let node =
                template_instantiate_with("t", &tree.nodes["t"], &set, &json!({"id":value,"cells":[value]}), &mut evaluation).unwrap();
            assert_eq!(node.ports["text"], json!(format!("live {value}")));
        }
        assert!(template_instantiate_with("t", &tree.nodes["t"], &set, &json!({"cells":["third"]}), &mut evaluation).is_err());
        let mut expired = TemplateEvaluation::new(&tree.model);
        expired.started = Instant::now() - EVALUATION_BUDGET;
        assert!(template_instantiate_with("t", &tree.nodes["t"], &set, &json!({"cells":["late"]}), &mut expired).is_err());
    }

    #[test]
    fn a_resolved_tree_rebuilds_its_binding_set() {
        let doc = crate::parse("---\nscene: 1\nname: budget\ncitizen: test\n---\n```mix\nroot: {widget: \"list\", rows: [], row: \"t\", row_height: 20}\nt: {widget: \"text\", text: \"= $item.cells[0]\", size: \"= $model.size\"}\n```\n").unwrap();
        let tree = crate::resolve(&doc).unwrap();
        let set = BindingSet::from_resolved(&tree);
        assert_eq!(set.bindings.keys().collect::<Vec<_>>(), ["t.size", "t.text"]);
        assert!(set.bindings["t.text"].reads_item);
        assert_eq!(set.bindings["t.size"].deps, ["model.size".to_owned()].into());
        let node = template_instantiate("t", &tree.nodes["t"], &set, &json!({"size": 20}), &json!({"cells":["row"]})).unwrap();
        assert_eq!(node.ports["text"], json!("row"));
        assert_eq!(node.ports["size"], json!(20.0));
    }
}

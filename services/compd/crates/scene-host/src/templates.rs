//! List row templates, instantiated once per accepted revision.
//!
//! Ingress runs it as a preflight before a revision commits, so a document
//! the renderer cannot instantiate is refused, not half-drawn; the renderer
//! then reads the prepared rows instead of evaluating again.

use std::collections::{BTreeMap, BTreeSet};

use scene::bindings::{BindingSet, TemplateEvaluation, template_instantiate_with};
use scene::{Diagnostic, Node, ResolvedScene, Severity};
use serde_json::{Value, json};

/// One list's rows: row id → the instantiated template subtree (node id →
/// node), in the list's `rows` order by id.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ListData {
    pub node: String,
    pub instances: BTreeMap<String, BTreeMap<String, Node>>,
}

/// Every list of a revision, by list node id.
pub type PreparedLists = BTreeMap<String, ListData>;

/// The ingress preflight. Model-only patches call it on the candidate tree.
pub fn validate_templates(tree: &ResolvedScene) -> Result<PreparedLists, Value> {
    prepare_lists(tree).map_err(|diagnostic| {
        json!({
            "error_code": "scene_template",
            "message": diagnostic.message,
            "scene": tree.name,
            "diagnostics": [diagnostic],
        })
    })
}

fn nested_list(line: usize) -> Diagnostic {
    Diagnostic {
        severity: Severity::Error,
        code: "invalid-template".into(),
        line,
        message: "Nested lists are not supported by this renderer".into(),
    }
}

fn prepare_lists(tree: &ResolvedScene) -> Result<PreparedLists, Diagnostic> {
    // Resolved expressions were already policy-checked by scene core.
    let set = BindingSet::from_resolved(tree);
    let mut evaluation = TemplateEvaluation::new(&tree.model);
    let mut lists = BTreeMap::new();
    let templates = template_ids(tree);
    for (id, node) in &tree.nodes {
        if node.family != "list" {
            continue;
        }
        if templates.contains(id) {
            return Err(nested_list(node.line));
        }
        let mut data = ListData { node: id.clone(), instances: BTreeMap::new() };
        for item in rows(node) {
            let mut nodes = BTreeMap::new();
            template_node(tree, text(node, "row"), item, &set, &mut evaluation, &mut nodes)?;
            data.instances.insert(item["id"].as_str().unwrap_or_default().into(), nodes);
        }
        lists.insert(id.clone(), data);
    }
    Ok(lists)
}

/// Every node reachable from a list row template (drawn per row, never
/// as a document node).
pub fn template_ids(tree: &ResolvedScene) -> BTreeSet<String> {
    fn visit(tree: &ResolvedScene, id: &str, ids: &mut BTreeSet<String>) {
        if !ids.insert(id.into()) {
            return;
        }
        if let Some(node) = tree.nodes.get(id) {
            for child in children(node) {
                visit(tree, child, ids);
            }
        }
    }
    let mut ids = BTreeSet::new();
    for id in &tree.templates {
        visit(tree, id, &mut ids);
    }
    ids
}

// The core bindings evaluate before the legacy cells substitution.
fn template_node(
    tree: &ResolvedScene,
    id: &str,
    item: &Value,
    set: &BindingSet,
    evaluation: &mut TemplateEvaluation,
    nodes: &mut BTreeMap<String, Node>,
) -> Result<(), Diagnostic> {
    // An unknown row id is resolve's to refuse; never index past it here.
    let Some(template) = tree.nodes.get(id) else {
        return Ok(());
    };
    if template.family == "list" {
        return Err(nested_list(template.line));
    }
    let mut node = template_instantiate_with(id, template, set, item, evaluation)?;
    let port = match node.family.as_str() {
        "text" => Some("text"),
        "image" => Some("src"),
        _ => None,
    };
    if let Some(port) = port {
        // A binding result is literal data; never interpret its {cells[..]}.
        if !tree.bindings.contains_key(&format!("{id}.{port}")) {
            let value = substitute_cells(text(&node, port), &item["cells"]);
            node.ports.insert(port.into(), json!(value));
        }
    }
    let kids: Vec<String> = children(&node).map(str::to_owned).collect();
    for child in &kids {
        template_node(tree, child, item, set, evaluation, nodes)?;
    }
    nodes.insert(id.into(), node);
    Ok(())
}

/// `{cells[N]}` → the row's Nth cell string; anything unresolvable stays
/// literal.
pub fn substitute_cells(mut source: &str, cells: &Value) -> String {
    let mut out = String::new();
    while let Some(start) = source.find("{cells[") {
        out.push_str(&source[..start]);
        let token = &source[start + 7..];
        let Some(end) = token.find("]}") else {
            out.push_str(&source[start..]);
            return out;
        };
        if let Some(value) = token[..end]
            .parse::<usize>()
            .ok()
            .and_then(|index| cells.get(index))
            .and_then(Value::as_str)
        {
            out.push_str(value);
        } else {
            out.push_str(&source[start..start + 7 + end + 2]);
        }
        source = &token[end + 2..];
    }
    out.push_str(source);
    out
}

pub fn children(node: &Node) -> impl Iterator<Item = &str> {
    node.ports
        .get("children")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
}

pub fn rows(node: &Node) -> &[Value] {
    node.ports.get("rows").and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

pub fn text<'a>(node: &'a Node, port: &str) -> &'a str {
    node.ports.get(port).and_then(Value::as_str).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_substitute_and_unresolvable_tokens_stay_literal() {
        let cells = json!(["a", "b", 3]);
        assert_eq!(substitute_cells("{cells[0]}-{cells[1]}", &cells), "a-b");
        assert_eq!(substitute_cells("{cells[2]}|{cells[9]}|{cells[x]}", &cells), "{cells[2]}|{cells[9]}|{cells[x]}");
        assert_eq!(substitute_cells("open {cells[0]", &cells), "open {cells[0]");
    }

    #[test]
    fn a_list_inside_a_row_template_is_refused() {
        let source = "---\nscene: 1\nname: nested\ncitizen: c\n---\n```mix\nroot: {widget: \"list\", rows: [{id: \"1\", cells: [\"x\"]}], row: \"t\"}\nt: {widget: \"row\", children: [\"inner\"]}\ninner: {widget: \"list\", rows: [], row: \"leaf\"}\nleaf: {widget: \"text\", text: \"x\"}\n```\n";
        // Refused somewhere before acceptance; when scene core lets it
        // through, this preflight is what refuses it.
        let tree = scene::parse(source).ok().and_then(|document| scene::resolve(&document).ok());
        if let Some(tree) = tree {
            let error = validate_templates(&tree).unwrap_err();
            assert_eq!(error["diagnostics"][0]["code"], "invalid-template");
        }
    }
}

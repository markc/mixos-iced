//! Static facts about a compiled tree: its depth and the global paths it
//! reads.

use std::collections::BTreeSet;

use crate::ast::{Node, Part, Segment};

/// Deepest nesting a tree may have. A left-associative operator chain
/// parses iteratively but still builds a deep tree, so the evaluator's
/// recursion is bounded here rather than by the parser's nesting cap.
pub(crate) const MAX_DEPTH: usize = 256;

/// Every child node, including those inside interpolations.
fn children<'a>(node: &'a Node, out: &mut Vec<&'a Node>) {
    match node {
        Node::Number(_) | Node::Str(_) | Node::Bool(_) | Node::Nil | Node::Var(_) => {}
        Node::Interp(parts) => {
            for part in parts {
                if let Part::Var(var) = part {
                    for segment in &var.segments {
                        if let Segment::Index(index) = segment {
                            out.push(index);
                        }
                    }
                    if let Some((_, Some(payload))) = &var.coalesce {
                        out.push(payload);
                    }
                }
            }
        }
        Node::Binary { left, right, .. } => out.extend([&**left, &**right]),
        Node::Unary { operand, .. } => out.push(operand),
        Node::Ternary { cond, then, otherwise } => out.extend([&**cond, &**then, &**otherwise]),
        Node::If { branches, otherwise } => {
            for branch in branches {
                out.push(&branch.cond);
                out.extend(branch.body.as_ref());
            }
            out.extend(otherwise.as_deref());
        }
        Node::Field { object, .. } => out.push(object),
        Node::Index { object, index } => out.extend([&**object, &**index]),
        Node::List(items) => out.extend(items.iter()),
        Node::Map(entries) => out.extend(entries.iter().map(|(_, value)| value)),
    }
}

/// The nesting depth of the tree, counted without recursion.
pub(crate) fn depth(root: &Node) -> usize {
    let mut deepest = 0;
    let mut work = vec![(root, 1usize)];
    let mut scratch = Vec::new();
    while let Some((node, level)) = work.pop() {
        deepest = deepest.max(level);
        scratch.clear();
        children(node, &mut scratch);
        work.extend(scratch.iter().map(|child| (*child, level + 1)));
    }
    deepest
}

/// The dotted global paths the tree reads, one per access chain: `$model`
/// is `model`, `$model.a.b` is `model.a.b`, and a chain stops at its first
/// index, so `$model.m[$k].x` reads `model.m` (and `k`). Interpolations
/// follow the same rule. The tree must already be within [`MAX_DEPTH`].
pub(crate) fn reads(root: &Node) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    collect(root, &mut out);
    out
}

fn collect(node: &Node, out: &mut BTreeSet<String>) {
    match node {
        Node::Var(name) => {
            out.insert(name.clone());
        }
        Node::Field { .. } | Node::Index { .. } => {
            if let Some((path, _)) = access_path(node) {
                out.insert(path);
                chain_indices(node, out);
                return;
            }
            let mut kids = Vec::new();
            children(node, &mut kids);
            for child in kids {
                collect(child, out);
            }
        }
        Node::Interp(parts) => {
            for part in parts {
                if let Part::Var(var) = part {
                    let mut path = var.head.clone();
                    for segment in &var.segments {
                        match segment {
                            Segment::Field(name) => {
                                path.push('.');
                                path.push_str(name);
                            }
                            Segment::Index(_) => break,
                        }
                    }
                    out.insert(path);
                    for segment in &var.segments {
                        if let Segment::Index(index) = segment {
                            collect(index, out);
                        }
                    }
                    if let Some((_, Some(payload))) = &var.coalesce {
                        collect(payload, out);
                    }
                }
            }
        }
        _ => {
            let mut kids = Vec::new();
            children(node, &mut kids);
            for child in kids {
                collect(child, out);
            }
        }
    }
}

/// The dotted path of a `$var.a.b[...]` chain, and whether it contains an
/// index (fields after an index are not part of the path).
fn access_path(node: &Node) -> Option<(String, bool)> {
    match node {
        Node::Var(name) => Some((name.clone(), false)),
        Node::Field { object, field } => {
            let (path, indexed) = access_path(object)?;
            if indexed {
                Some((path, true))
            } else {
                Some((format!("{path}.{field}"), false))
            }
        }
        Node::Index { object, .. } => access_path(object).map(|(path, _)| (path, true)),
        _ => None,
    }
}

/// The reads of every index expression inside an access chain.
fn chain_indices(node: &Node, out: &mut BTreeSet<String>) {
    match node {
        Node::Field { object, .. } => chain_indices(object, out),
        Node::Index { object, index } => {
            chain_indices(object, out);
            collect(index, out);
        }
        _ => {}
    }
}

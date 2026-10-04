//! Where a resolved scene mounts: its page id, edge, or the dialog seat.

use scene::ResolvedScene;
use serde_json::{Value, json};

use crate::seat::Edge;

/// The window envelope: the document's `window` header, else the ports of a
/// `window`-family node.
pub fn mount_config(tree: &ResolvedScene) -> Option<Value> {
    tree.window.clone().or_else(|| {
        tree.nodes
            .values()
            .find(|node| node.family == "window")
            .map(|node| json!(node.ports))
    })
}

/// The mount address: the envelope's `panel` (a declared sub-panel name,
/// which may carry characters the scene-name grammar forbids), else
/// `scene-<name>`.
pub fn page_id(tree: &ResolvedScene) -> String {
    mount_config(tree)
        .as_ref()
        .and_then(|window| window["panel"].as_str())
        .filter(|name| !name.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("scene-{}", tree.name))
}

/// The authored edge; right when absent or unknown.
pub fn scene_edge(tree: &ResolvedScene) -> Edge {
    match mount_config(tree).as_ref().and_then(|w| w["edge"].as_str()).unwrap_or("right") {
        "left" => Edge::Left,
        "top" => Edge::Top,
        "bottom" => Edge::Bottom,
        _ => Edge::Right,
    }
}

/// A `kind:"dialog"` window: the centred dialog seat, never an edge page.
pub fn is_dialog(tree: &ResolvedScene) -> bool {
    mount_config(tree).is_some_and(|window| window["kind"] == "dialog")
}

/// Authored dialog size (240..=2048, default 480), title, and chrome flag
/// (default true).
pub fn dialog_geometry(tree: &ResolvedScene) -> (f32, f32, Option<String>, bool) {
    let config = mount_config(tree).unwrap_or(Value::Null);
    let size = |key: &str| config[key].as_f64().unwrap_or(480.0).clamp(240.0, 2048.0) as f32;
    (
        size("w"),
        size("h"),
        config["title"].as_str().map(str::to_owned),
        config["chrome"].as_bool() != Some(false),
    )
}

//! `shell.scene.layout`: per-node geometry measured from the engine.
//!
//! The reply is Quoin's frozen shape (shell-verbs.json `shell.scene.layout`):
//! `{scene, revision, applied_revision, visible, surface:{kind, edge, output,
//! x, y, w, h}, nodes:{id:{x,y,w,h,hidden}}, instances:{list:{item:{x,y,w,h}}},
//! chrome:{close:{x,y,w,h}}}`, logical px relative to the scene's surface.
//! An unmapped scene answers `visible:false` with empty `nodes`,
//! `instances` and `chrome`. compd adds `page`: the opaque `#rrggbbaa` it
//! paints under the scene (the design `base` surface), null while unmapped,
//! so a pixel check reads the colour from the host instead of assuming one.
//!
//! The rects come from a widget operation over the live iced instance (the
//! view builder gives each document node, list row and the close button a
//! widget id), corrected for scrolling. A document node that is not drawn
//! (hidden, inside a hidden parent, or a template) reports `hidden:true` at
//! zero size: unlike Quoin's Taffy tree, iced keeps no layout for a widget
//! it does not build.

use std::collections::HashMap;

use iced_core::widget::operation::Scrollable;
use iced_core::widget::{Id, Operation};
use iced_core::{Rectangle, Vector};
use serde_json::{Map, Value, json};

use crate::templates::rows;
use crate::view::{CLOSE_ID, node_id, row_id};

/// Every identified widget's bounds, as drawn (scroll offsets applied).
#[derive(Default)]
pub struct Measure {
    /// The accumulated scroll translation of the scrollables we are inside.
    offset: Vector,
    /// A scrollable's translation, applied to the traversal that follows it.
    pending: Option<Vector>,
    pub found: HashMap<Id, Rectangle>,
}

impl Operation for Measure {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        let shift = self.pending.take().unwrap_or(Vector::new(0.0, 0.0));
        self.offset += shift;
        operate(self);
        self.offset -= shift;
    }

    fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
        if let Some(id) = id {
            let shown = Rectangle { x: bounds.x - self.offset.x, y: bounds.y - self.offset.y, ..bounds };
            self.found.insert(id.clone(), shown);
        }
    }

    fn scrollable(
        &mut self,
        _id: Option<&Id>,
        _bounds: Rectangle,
        _content_bounds: Rectangle,
        translation: Vector,
        _state: &mut dyn Scrollable,
    ) {
        self.pending = Some(translation);
    }
}

fn rect_json(rect: &Rectangle, factor: f32, hidden: Option<bool>) -> Value {
    let round = |v: f32| ((v * factor) as f64 * 100.0).round() / 100.0;
    let mut out = json!({"x": round(rect.x), "y": round(rect.y), "w": round(rect.width), "h": round(rect.height)});
    if let Some(hidden) = hidden {
        out["hidden"] = json!(hidden);
    }
    out
}

/// `nodes`, `instances` and `chrome` from a measurement of `tree`.
/// `factor` converts the instance's logical px to the document's (its iced
/// scale over the output scale; 1 when they agree). `node` narrows to one
/// document node and, for a list, its rows.
pub fn measured(
    tree: &scene::ResolvedScene,
    found: &HashMap<Id, Rectangle>,
    factor: f32,
    node: Option<&str>,
) -> (Value, Value, Value) {
    let mut nodes = Map::new();
    let mut instances = Map::new();
    let templates = crate::templates::template_ids(tree);
    for (id, n) in &tree.nodes {
        if node.is_some_and(|wanted| wanted != id.as_str()) || templates.contains(id) {
            continue;
        }
        let entry = match found.get(&node_id(id)) {
            Some(rect) => rect_json(rect, factor, Some(false)),
            None => json!({"x": 0.0, "y": 0.0, "w": 0.0, "h": 0.0, "hidden": true}),
        };
        nodes.insert(id.clone(), entry);
        if n.family == "list" {
            let mut items = Map::new();
            for item in rows(n) {
                let item_id = item["id"].as_str().unwrap_or_default();
                if let Some(rect) = found.get(&row_id(id, item_id)) {
                    items.insert(item_id.to_owned(), rect_json(rect, factor, None));
                }
            }
            if !items.is_empty() {
                instances.insert(id.clone(), Value::Object(items));
            }
        }
    }
    let mut chrome = Map::new();
    if let Some(rect) = found.get(&Id::new(CLOSE_ID)) {
        chrome.insert("close".into(), rect_json(rect, factor, None));
    }
    (Value::Object(nodes), Value::Object(instances), Value::Object(chrome))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rects_report_drawn_nodes_hidden_ones_and_rows_with_the_scroll_applied() {
        let tree = scene::resolve(
            &scene::parse(scene::fixtures::CLIPPANEL).unwrap(),
        )
        .unwrap();
        let mut measure = Measure::default();
        measure.container(Some(&node_id("root")), Rectangle { x: 0.0, y: 32.0, width: 720.0, height: 488.0 });
        // A scrolled list: its rows are reported where they are drawn.
        measure.scrollable(None, Rectangle::default(), Rectangle::default(), Vector::new(0.0, 10.0), &mut NoScroll);
        measure.traverse(&mut |op| {
            op.container(Some(&row_id("table", "e1")), Rectangle { x: 14.0, y: 100.0, width: 692.0, height: 38.0 });
        });
        measure.container(Some(&Id::new(CLOSE_ID)), Rectangle { x: 681.0, y: 5.0, width: 28.0, height: 24.0 });
        let (nodes, instances, chrome) = measured(&tree, &measure.found, 1.0, None);
        assert_eq!(nodes["root"], json!({"x":0.0,"y":32.0,"w":720.0,"h":488.0,"hidden":false}));
        assert_eq!(nodes["remote_title"]["hidden"], true, "a node not drawn is reported hidden");
        assert!(nodes.get("entry_row").is_none(), "templates are rows, not nodes");
        assert_eq!(instances["table"]["e1"], json!({"x":14.0,"y":90.0,"w":692.0,"h":38.0}));
        assert_eq!(chrome["close"], json!({"x":681.0,"y":5.0,"w":28.0,"h":24.0}));
        // Narrowed to one node.
        let (nodes, instances, _) = measured(&tree, &measure.found, 1.0, Some("root"));
        assert_eq!(nodes.as_object().unwrap().len(), 1);
        assert!(instances.as_object().unwrap().is_empty());
    }

    struct NoScroll;
    impl Scrollable for NoScroll {
        fn snap_to(&mut self, _offset: iced_core::widget::operation::scrollable::RelativeOffset<Option<f32>>) {}
        fn scroll_to(&mut self, _offset: iced_core::widget::operation::scrollable::AbsoluteOffset<Option<f32>>) {}
        fn scroll_by(
            &mut self,
            _offset: iced_core::widget::operation::scrollable::AbsoluteOffset,
            _bounds: Rectangle,
            _content_bounds: Rectangle,
        ) {
        }
    }
}

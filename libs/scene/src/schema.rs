//! The widget families and their ports: names, types, defaults, enum
//! values and minimums, plus the value checks every port goes through.

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::HashSet;

use crate::{Diagnostic, MAX_ROWS};

/// One port as `describe` reports it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PortDescribe {
    pub path: String,
    #[serde(rename = "type")]
    pub ty: String,
    pub mutable: bool,
    pub sensitive: bool,
    pub description: String,
    #[serde(rename = "enum", skip_serializing_if = "Option::is_none")]
    pub enum_values: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<JsonValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
}

#[derive(Clone, Copy)]
pub(crate) struct Port {
    pub(crate) name: &'static str,
    pub(crate) ty: &'static str,
    pub(crate) required: bool,
    pub(crate) default: Option<&'static str>,
    pub(crate) enum_values: &'static [&'static str],
    pub(crate) min: Option<f64>,
}

const fn p(n: &'static str, t: &'static str, r: bool, d: Option<&'static str>) -> Port {
    Port { name: n, ty: t, required: r, default: d, enum_values: &[], min: None }
}
const fn pe(n: &'static str, d: &'static str, e: &'static [&'static str]) -> Port {
    Port { name: n, ty: "string", required: false, default: Some(d), enum_values: e, min: None }
}
const fn pnd(n: &'static str, d: &'static str, m: Option<f64>) -> Port {
    Port {
        name: n,
        ty: "number",
        required: false,
        default: Some(d),
        enum_values: &[],
        min: Some(match m {
            Some(value) => value,
            None => 0.0,
        }),
    }
}
const fn pn(n: &'static str, r: bool, m: Option<f64>) -> Port {
    Port {
        name: n,
        ty: "number",
        required: r,
        default: None,
        enum_values: &[],
        min: Some(match m {
            Some(value) => value,
            None => 0.0,
        }),
    }
}

/// The ports of family `f`, or `None` for an unknown family.
pub(crate) fn schema(f: &str) -> Option<Vec<Port>> {
    static EDGE: &[&str] = &["right", "left", "top", "bottom"];
    static ALIGN: &[&str] = &["start", "center", "end", "stretch"];
    static TONE: &[&str] = &["normal", "danger", "primary"];
    static KIND: &[&str] = &["edge", "dialog"];
    static WINDOW: [Port; 6] = [
        Port { name: "kind", ty: "string", required: true, default: None, enum_values: KIND, min: None },
        pe("edge", "\"right\"", EDGE),
        p("title", "string", false, None),
        pn("w", false, None),
        pn("h", false, None),
        p("chrome", "bool", false, Some("true")),
    ];
    static BOX: [Port; 5] = [
        p("children", "list", true, None),
        pnd("gap", "0", None),
        pnd("padding", "0", None),
        p("fill", "bool", false, Some("false")),
        // A column stretched its children before `align` existed; the
        // default keeps every v0 document laying out exactly as it did.
        pe("align", "\"stretch\"", ALIGN),
    ];
    static ROW: [Port; 10] = [
        p("children", "list", true, None),
        pnd("gap", "0", None),
        pnd("padding", "0", None),
        p("fill", "bool", false, Some("false")),
        Port { name: "align", ty: "string", required: false, default: Some("\"start\""), enum_values: ALIGN, min: None },
        pn("height", false, None),
        pn("radius", false, None),
        p("background", "string", false, None),
        p("hover", "string", false, None),
        p("on_click", "string", false, None),
    ];
    static TEXT: [Port; 10] = [
        p("text", "string", true, None),
        pnd("size", "13", Some(0.0)),
        p("bold", "bool", false, Some("false")),
        p("mono", "bool", false, Some("false")),
        p("color", "string", false, None),
        p("elide", "bool", false, Some("false")),
        pn("width", false, None),
        p("fill", "bool", false, Some("false")),
        p("hidden", "bool", false, Some("false")),
        pe("align", "\"left\"", &["left", "center", "right"]),
    ];
    static FIELD: [Port; 6] = [
        p("value", "string", true, None),
        p("placeholder", "string", false, None),
        pn("width", false, None),
        p("password", "bool", false, Some("false")),
        p("on_change", "string", false, None),
        p("on_submit", "string", false, None),
    ];
    static BUTTON: [Port; 4] = [
        p("label", "string", true, None),
        Port { name: "tone", ty: "string", required: false, default: Some("\"normal\""), enum_values: TONE, min: None },
        pn("width", false, Some(0.0)),
        p("on_click", "string", false, None),
    ];
    static TOGGLE: [Port; 3] = [
        p("value", "bool", true, None),
        p("label", "string", true, None),
        p("on_change", "string", false, None),
    ];
    static LIST: [Port; 10] = [
        p("rows", "list", true, None),
        p("row", "string", true, None),
        pn("row_height", true, Some(0.0)),
        pnd("gap", "0", None),
        pn("max_rows", false, Some(1.0)),
        p("fill", "bool", false, Some("false")),
        p("hidden_if_empty", "bool", false, Some("false")),
        p("on_click", "string", false, None),
        // No default: absence keeps the historical resolved document and the
        // renderer's vertical list.
        p("flow", "string", false, None),
        p("align", "string", false, None),
    ];
    static IMAGE: [Port; 3] = [p("src", "string", true, None), pn("w", false, Some(0.0)), pn("h", false, Some(0.0))];
    static SPACER: [Port; 1] = [pn("size", false, None)];
    let family: &[Port] = match f {
        "window" => &WINDOW,
        "column" => &BOX,
        "row" => &ROW,
        "text" => &TEXT,
        "field" => &FIELD,
        "button" => &BUTTON,
        "toggle" => &TOGGLE,
        "list" => &LIST,
        "image" => &IMAGE,
        "spacer" => &SPACER,
        _ => return None,
    };
    let mut ports = family.to_vec();
    if f == "list" {
        ports.iter_mut().find(|p| p.name == "flow").unwrap().enum_values = &["vertical", "horizontal"];
        ports.iter_mut().find(|p| p.name == "align").unwrap().enum_values = ALIGN;
    }
    // The flex ports have no defaults: absence preserves the legacy mapping
    // and canonical resolved documents. A window is edge metadata, not a
    // flex child.
    if f != "window" {
        if f != "text" {
            ports.push(p("hidden", "bool", false, None));
        }
        ports.extend([
            p("align_self", "string", false, None),
            pn("grow", false, None),
            pn("shrink", false, None),
            pn("basis", false, None),
            pn("min_width", false, None),
            pn("max_width", false, None),
            pn("min_height", false, None),
            pn("max_height", false, None),
        ]);
        ports.iter_mut().find(|p| p.name == "align_self").unwrap().enum_values =
            &["auto", "start", "center", "end", "stretch"];
    }
    if matches!(f, "row" | "column") {
        let mut justify = p("justify", "string", false, None);
        justify.enum_values = &["start", "center", "end", "between", "around", "evenly"];
        ports.extend([
            justify,
            pn("row_gap", false, None),
            pn("column_gap", false, None),
            pn("padding_top", false, None),
            pn("padding_right", false, None),
            pn("padding_bottom", false, None),
            pn("padding_left", false, None),
        ]);
    }
    Some(ports)
}

/// The ports of family `f` for an editor or an agent, or `None` for an
/// unknown family.
pub fn describe(f: &str) -> Option<Vec<PortDescribe>> {
    schema(f).map(|ps| {
        ps.iter()
            .map(|p| PortDescribe {
                path: p.name.into(),
                ty: p.ty.into(),
                mutable: true,
                sensitive: p.name == "value" && f == "field",
                description: format!("{} port of {}", p.name, f),
                enum_values: (!p.enum_values.is_empty()).then(|| p.enum_values.iter().map(|x| (*x).into()).collect()),
                default: p.default.and_then(|x| serde_json::from_str(x).ok()).map(normalize_number),
                min: p.min,
                max: None,
            })
            .collect()
    })
}

pub(crate) fn port_for(family: &str, name: &str) -> Option<Port> {
    schema(family)?.iter().find(|p| p.name == name).copied()
}

pub(crate) fn check_port_value(id: &str, line: usize, p: Port, v: &JsonValue, out: &mut Vec<Diagnostic>) {
    let k = p.name;
    if !type_matches(v, p.ty) {
        out.push(Diagnostic::error("port-type", line, format!("port {k} on {id} must be {}", p.ty)));
    }
    if !p.enum_values.is_empty() && v.as_str().is_some_and(|x| !p.enum_values.contains(&x)) {
        out.push(Diagnostic::error("enum-value", line, format!("invalid value for {k} on {id}")));
    }
    if let Some(min) = p.min {
        let exclusive = k == "row_height";
        if v.as_f64().is_some_and(|x| if exclusive { x <= min } else { x < min }) {
            out.push(Diagnostic::error(
                "port-min",
                line,
                format!("port {k} on {id} must be {} {min}", if exclusive { ">" } else { ">=" }),
            ));
        }
    }
    if k == "rows" {
        validate_rows(v, line, out);
    }
}

/// Every number as an `f64` JSON number, so `13` and `13.0` resolve alike.
pub(crate) fn normalize_number(v: JsonValue) -> JsonValue {
    if let Some(n) = v.as_f64() {
        serde_json::Number::from_f64(n).map(JsonValue::Number).unwrap_or(JsonValue::Null)
    } else if let Some(a) = v.as_array() {
        JsonValue::Array(a.iter().cloned().map(normalize_number).collect())
    } else if let Some(o) = v.as_object() {
        JsonValue::Object(o.iter().map(|(k, v)| (k.clone(), normalize_number(v.clone()))).collect())
    } else {
        v
    }
}

fn type_matches(v: &JsonValue, t: &str) -> bool {
    match t {
        "string" => v.is_string(),
        "number" => v.is_number(),
        "bool" => v.is_boolean(),
        "list" => v.is_array(),
        "object" => v.is_object(),
        _ => true,
    }
}

fn validate_rows(value: &JsonValue, line: usize, o: &mut Vec<Diagnostic>) {
    if let Some(rs) = value.as_array() {
        if rs.len() > MAX_ROWS {
            o.push(Diagnostic::error("row-limit", line, "list has more than 500 rows"));
        }
        let mut ids = HashSet::new();
        for r in rs {
            if let Some(id) = r.get("id").and_then(JsonValue::as_str)
                && (id.is_empty() || !ids.insert(id))
            {
                o.push(Diagnostic::error("row-type", line, "row ids must be non-empty and unique within a list"));
            }
            if r.get("id").and_then(JsonValue::as_str).is_none()
                || r.get("cells").and_then(JsonValue::as_array).is_none_or(|cs| cs.iter().any(|c| !c.is_string()))
            {
                o.push(Diagnostic::error("row-type", line, "each row must contain string id and string cells"));
            }
        }
    }
}

// SPDX-License-Identifier: MIT OR Apache-2.0
//! Strict app commands. GUI buttons and Bus callers share these operations.
use crate::{
    capture::Request,
    document::{Crop, Document, Shape},
};
use serde::Deserialize;
use serde_json::{Value, json};
pub const SERVICE: &str = "cap";
pub fn is_edit(verb: &str) -> bool {
    matches!(
        verb,
        "cap.annotate" | "cap.delete" | "cap.move" | "cap.crop" | "cap.undo" | "cap.redo"
    )
}
#[derive(Debug)]
pub enum Operation {
    Capture(Request),
    Open(String),
    Export(String),
    Show,
    Quit,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PathArg {
    path: String,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct IdArg {
    id: u64,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MoveArg {
    id: u64,
    dx: f32,
    dy: f32,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CropArg {
    crop: Option<Crop>,
}
pub fn parse(verb: &str, body: &str) -> Result<Value, String> {
    if body.len() > 1024 * 1024 {
        return Err("command exceeds 1 MiB".into());
    }
    let value: Value = serde_json::from_str(if body.trim().is_empty() { "{}" } else { body })
        .map_err(|e| e.to_string())?;
    if !value.is_object() {
        return Err("expected JSON object".into());
    }
    if matches!(
        verb,
        "cap.ping" | "cap.info" | "cap.show" | "cap.quit" | "cap.cancel" | "cap.undo" | "cap.redo"
    ) && !value.as_object().unwrap().is_empty()
    {
        return Err("this command takes an empty object".into());
    }
    Ok(value)
}
pub fn operation(verb: &str, value: Value) -> Result<Operation, String> {
    Ok(match verb {
        "cap.capture" => {
            let r: Request = serde_json::from_value(value).map_err(|e| e.to_string())?;
            r.validate()?;
            Operation::Capture(r)
        }
        "cap.open" => Operation::Open(
            serde_json::from_value::<PathArg>(value)
                .map_err(|e| e.to_string())?
                .path,
        ),
        "cap.export" => Operation::Export(
            serde_json::from_value::<PathArg>(value)
                .map_err(|e| e.to_string())?
                .path,
        ),
        "cap.show" => Operation::Show,
        "cap.quit" => Operation::Quit,
        _ => return Err("unknown operation".into()),
    })
}
pub fn edit(document: &mut Document, verb: &str, value: Value) -> Result<Value, String> {
    match verb {
        "cap.annotate" => {
            let shape: Shape = serde_json::from_value(value).map_err(|e| e.to_string())?;
            return document.add(shape).map(|id| json!({"id":id}));
        }
        "cap.delete" => {
            let a: IdArg = serde_json::from_value(value).map_err(|e| e.to_string())?;
            document.delete(a.id)?
        }
        "cap.move" => {
            let a: MoveArg = serde_json::from_value(value).map_err(|e| e.to_string())?;
            document.move_object(a.id, a.dx, a.dy)?
        }
        "cap.crop" => {
            let a: CropArg = serde_json::from_value(value).map_err(|e| e.to_string())?;
            document.set_crop(a.crop)?
        }
        "cap.undo" => {
            document.undo();
        }
        "cap.redo" => {
            document.redo();
        }
        _ => return Err("unknown edit command".into()),
    }
    Ok(document.info())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_extra_keys_and_incorrect_shapes() {
        assert!(parse("cap.info", "{\"extra\":true}").is_err());
        assert!(operation("cap.export", json!({"path":"/tmp/a","overwrite":true})).is_err());
        assert!(parse("cap.capture", "[]").is_err());
    }
}

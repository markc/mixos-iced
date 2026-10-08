// SPDX-License-Identifier: MIT OR Apache-2.0
//! Finite scale control, fenced by the actual Output allocation and topology.

use crate::reply::ControlReply;
use crate::request::invalid_argument;
use serde_json::{Value, json};

pub struct OutputIdentity(pub uuid::Uuid);

#[derive(Clone, Debug, PartialEq)]
pub struct ScaleSpec {
    pub output: String,
    pub instance: uuid::Uuid,
    pub generation: u64,
    pub scale: f64,
}

pub fn parse(args: &Value) -> Result<ScaleSpec, ControlReply> {
    let object = args
        .as_object()
        .ok_or_else(|| invalid_argument("args", "object", "{output,instance,generation,scale}"))?;
    if let Some(field) = object
        .keys()
        .find(|key| !["output", "instance", "generation", "scale"].contains(&key.as_str()))
    {
        return Err(ControlReply::InvalidArgs {
            field: field.clone(),
            allowed: &["output", "instance", "generation", "scale"],
        });
    }
    let output = object
        .get("output")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty() && name.len() <= 128 && !name.chars().any(char::is_control))
        .ok_or_else(|| {
            invalid_argument(
                "output",
                "exact output name",
                "1..128 bytes without control characters",
            )
        })?;
    let instance = object
        .get("instance")
        .and_then(Value::as_str)
        .and_then(|value| uuid::Uuid::parse_str(value).ok())
        .filter(|id| !id.is_nil())
        .ok_or_else(|| invalid_argument("instance", "UUID", "current actual output instance"))?;
    let generation = object
        .get("generation")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            invalid_argument("generation", "positive u64", "current output generation")
        })?;
    let scale = object
        .get("scale")
        .and_then(Value::as_f64)
        .filter(|scale| scale.is_finite() && (0.5..=4.0).contains(scale))
        .ok_or_else(|| invalid_argument("scale", "finite number", "0.5..4.0"))?;
    Ok(ScaleSpec {
        output: output.into(),
        instance,
        generation,
        scale,
    })
}

/// The owner passes actual Output facts, never caller-supplied availability.
pub fn admit(
    spec: &ScaleSpec,
    instance: uuid::Uuid,
    generation: u64,
    available: bool,
    current_scale: f64,
) -> Result<bool, ControlReply> {
    if spec.instance != instance || spec.generation != generation {
        return Err(ControlReply::refused(
            "stale_output",
            json!({"output":spec.output,"instance":instance,"generation":generation}),
        ));
    }
    if !available {
        return Err(ControlReply::refused(
            "output_unavailable",
            json!({"output":spec.output}),
        ));
    }
    let changed = current_scale != spec.scale;
    if changed && generation == u64::MAX {
        return Err(ControlReply::refused(
            "output_generation_exhausted",
            json!({"output":spec.output}),
        ));
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_fractional_request_and_retired_output_fences() {
        let id = uuid::Uuid::now_v7();
        let args = json!({"output":"DP-1","instance":id,"generation":7,"scale":1.25});
        let spec = parse(&args).unwrap();
        assert!(admit(&spec, id, 7, true, 1.0).unwrap());
        assert!(!admit(&spec, id, 7, true, 1.25).unwrap());
        assert!(
            admit(&spec, uuid::Uuid::now_v7(), 7, true, 1.0).is_err(),
            "same name/geometry replacement cannot reuse old instance"
        );
        assert!(
            admit(&spec, id, 8, true, 1.0).is_err(),
            "scale/mode/return generation fence"
        );
        assert!(admit(&spec, id, 7, false, 1.0).is_err());
        let mut exhausted = spec.clone();
        exhausted.generation = u64::MAX;
        assert!(admit(&exhausted, id, u64::MAX, true, 1.0).is_err());
        assert!(!admit(&exhausted, id, u64::MAX, true, 1.25).unwrap());
        for scale in [
            Value::Null,
            json!("1.25"),
            json!(0.0),
            json!(0.49),
            json!(4.01),
        ] {
            let mut bad = args.clone();
            bad["scale"] = scale;
            assert!(parse(&bad).is_err());
        }
        for field in ["output", "instance", "generation", "scale"] {
            let mut bad = args.clone();
            bad.as_object_mut().unwrap().remove(field);
            assert!(parse(&bad).is_err());
        }
        for generation in [json!(0), json!(-1), json!(7.5), json!("7")] {
            let mut bad = args.clone();
            bad["generation"] = generation;
            assert!(parse(&bad).is_err());
        }
        let mut bad = args.clone();
        bad["extra"] = json!(true);
        assert!(parse(&bad).is_err());
        let mut bad = args;
        bad["instance"] = json!(uuid::Uuid::nil());
        assert!(parse(&bad).is_err());
    }
}

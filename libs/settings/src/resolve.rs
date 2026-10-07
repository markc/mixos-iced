// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const APPS: &[&str] = &["ced", "dopus", "term", "cap", "busviewer", "scene-editor"];

pub fn describe() -> Value {
    json!({"version": CONTRACT_VERSION, "schema": SCHEMA,
        "implementation": "headless authority; renderer integrations pending",
        "revision_encoding": "canonical decimal u64 string", "max_receipts": MAX_RECEIPTS,
        "receipt_expiry": "missing receipts are unknown; no ordering of opaque operation IDs",
        "fields": {
            "appearance.scheme": {"type":"string", "enum":["ocean","crimson","stone","forest","sunset","mono"]},
            "appearance.mode": {"type":"string", "enum":["light","dark"]},
            "appearance.contrast": {"type":"string", "enum":["normal","high"]},
            "appearance.source": {"type":"string|null", "max_bytes":MAX_SOURCE_BYTES},
            "ui.density": {"type":"number", "minimum":0.5, "maximum":2.0},
            "ui.text_scale": {"type":"number", "minimum":0.5, "maximum":3.0},
            "ui.reduced_motion": {"type":"boolean"},
            "shell.panels.<id>": {"type":"panel", "edge":["top","bottom","left","right"], "mode":["dock","overlay","hidden"], "thickness_range":[16,256]},
            "shell.page_order": {"type":"list", "max_items":64},
            "apps.<id>": {"type":"app_override", "fields":["scheme","mode","contrast","text_scale"]}
        }, "reset":"remove explicit app values or restore field package default",
        "defaults": Desktop::default(), "native_apps": APPS,
        "deferred":["renderer application","output overrides","font registration","artifacts","preview","replication","compatibility","policy"]})
}

fn error(path: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic::new("validation_failed", path, message)
}
fn bounded(value: f64, min: f64, max: f64, path: &str) -> Result<(), Diagnostic> {
    if !value.is_finite() || !(min..=max).contains(&value) {
        Err(error(path, format!("Expected {min}..{max}")))
    } else {
        Ok(())
    }
}
fn key(value: &str, path: &str) -> Result<(), Diagnostic> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        Err(error(path, "Invalid scoped identifier"))
    } else {
        Ok(())
    }
}

/// Build every advertised app context before acceptance. Compiler itself checks
/// all claimed source axes. Returned projections cannot become apply lineage.
pub fn resolve(desktop: &Desktop) -> Result<BTreeMap<String, Effective>, Vec<Diagnostic>> {
    resolve_with_embedded(desktop, design::EMBEDDED_DEFAULT_SOURCE)
}

/// Authorities pin the package source at profile creation. A binary upgrade
/// cannot silently alter effective values within an accepted revision.
pub fn resolve_with_embedded(
    desktop: &Desktop,
    embedded_source: &str,
) -> Result<BTreeMap<String, Effective>, Vec<Diagnostic>> {
    let validate = || -> Result<(), Diagnostic> {
        bounded(desktop.ui.density, 0.5, 2.0, "ui.density")?;
        bounded(desktop.ui.text_scale, 0.5, 3.0, "ui.text_scale")?;
        if desktop.shell.panels.len() > 16
            || desktop.shell.page_order.len() > 64
            || desktop.apps.len() > 16
        {
            return Err(error("desktop", "Collection limit exceeded"));
        }
        for (id, panel) in &desktop.shell.panels {
            key(id, "shell.panels")?;
            if !["top", "bottom", "left", "right"].contains(&panel.edge.as_str())
                || !["dock", "overlay", "hidden"].contains(&panel.mode.as_str())
                || !(16..=256).contains(&panel.thickness)
            {
                return Err(error(
                    &format!("shell.panels.{id}"),
                    "Invalid edge, mode or thickness",
                ));
            }
        }
        let mut pages = BTreeSet::new();
        for id in &desktop.shell.page_order {
            key(id, "shell.page_order")?;
            if !pages.insert(id) {
                return Err(error("shell.page_order", "Duplicate page"));
            }
        }
        for (app, value) in &desktop.apps {
            key(app, "apps")?;
            if value
                .scheme
                .as_deref()
                .is_some_and(|name| design::Scheme::from_name(name).is_none())
            {
                return Err(error(&format!("apps.{app}.scheme"), "Unknown scheme"));
            }
            if value
                .mode
                .as_deref()
                .is_some_and(|name| design::Mode::from_name(name).is_none())
            {
                return Err(error(&format!("apps.{app}.mode"), "Unknown mode"));
            }
            if value
                .contrast
                .as_deref()
                .is_some_and(|name| design::Contrast::from_name(name).is_none())
            {
                return Err(error(&format!("apps.{app}.contrast"), "Unknown contrast"));
            }
            if let Some(scale) = value.text_scale {
                bounded(scale, 0.5, 3.0, &format!("apps.{app}.text_scale"))?;
            }
        }
        if desktop
            .appearance
            .source
            .as_ref()
            .is_some_and(|s| s.len() > MAX_SOURCE_BYTES)
        {
            return Err(error("appearance.source", "Source limit exceeded"));
        }
        Ok(())
    };
    validate().map_err(|e| vec![e])?;
    let source = desktop
        .appearance
        .source
        .as_deref()
        .unwrap_or(embedded_source);
    let source_id = crate::source_digest(source);
    let doc = design::parse_design_source(design::SourceIdentity::new(&source_id), source)
        .map_err(|e| vec![error("appearance.source", e.to_string())])?;
    let mut contexts = BTreeSet::from([String::from("desktop")]);
    contexts.extend(APPS.iter().map(|a| format!("app:{a}")));
    contexts.extend(desktop.apps.keys().map(|a| format!("app:{a}")));
    let mut result = BTreeMap::new();
    for context in contexts {
        let app = context.strip_prefix("app:");
        let overlay = app.and_then(|id| desktop.apps.get(id));
        let mut appearance = desktop.appearance.clone();
        let mut ui = desktop.ui.clone();
        let mut provenance = BTreeMap::from([
            ("appearance.scheme".into(), "profile".into()),
            ("appearance.mode".into(), "profile".into()),
            ("appearance.contrast".into(), "profile".into()),
            ("ui.text_scale".into(), "profile".into()),
        ]);
        if let Some(overlay) = overlay {
            for (name, input, destination) in [
                ("scheme", &overlay.scheme, &mut appearance.scheme),
                ("mode", &overlay.mode, &mut appearance.mode),
                ("contrast", &overlay.contrast, &mut appearance.contrast),
            ] {
                if let Some(value) = input {
                    *destination = value.clone();
                    provenance.insert(
                        format!("appearance.{name}"),
                        format!("apps.{}", app.unwrap()),
                    );
                }
            }
            if let Some(scale) = overlay.text_scale {
                ui.text_scale = scale;
                provenance.insert("ui.text_scale".into(), format!("apps.{}", app.unwrap()));
            }
            // A requested high contrast cannot be cancelled by an app.
            if desktop.appearance.contrast == "high" {
                appearance.contrast = "high".into();
                provenance.insert("appearance.contrast".into(), "profile_accessibility".into());
            }
        }
        let selection = || -> Result<design::DesignContext, Diagnostic> {
            Ok(design::DesignContext {
                scheme: design::Scheme::from_name(&appearance.scheme)
                    .ok_or_else(|| error("appearance.scheme", "Unknown scheme"))?,
                mode: design::Mode::from_name(&appearance.mode)
                    .ok_or_else(|| error("appearance.mode", "Unknown mode"))?,
                contrast: design::Contrast::from_name(&appearance.contrast)
                    .ok_or_else(|| error("appearance.contrast", "Unknown contrast"))?,
                app: app.map(str::to_owned),
            })
        };
        let compiled = design::compile_design(&doc, selection().map_err(|e| vec![e])?);
        let projection = match compiled {
            design::DesignCompileResult::Success(success) => success.candidate.read_projection(),
            design::DesignCompileResult::Fatal(failure) => {
                return Err(failure
                    .diagnostics
                    .into_iter()
                    .map(|d| {
                        Diagnostic::new(d.code, &format!("appearance.source.{}", d.path), d.message)
                    })
                    .collect());
            }
        };
        result.insert(
            context.clone(),
            Effective {
                scheme: appearance.scheme,
                mode: appearance.mode,
                contrast: appearance.contrast,
                ui,
                design: projection,
                provenance,
            },
        );
    }
    Ok(result)
}

/// The patch vocabulary is explicit. No recursive JSON merge and no executing
/// data. null resets an optional app override; reset is the same transaction.
pub fn patch(
    current: &Desktop,
    changes: &BTreeMap<String, Value>,
    reset: &[String],
) -> Result<Desktop, Diagnostic> {
    if changes.len() + reset.len() > 64 {
        return Err(error("changes", "Batch limit exceeded"));
    }
    let mut result = current.clone();
    for path in reset {
        if changes.contains_key(path) {
            return Err(error(path, "Cannot apply and reset the same field"));
        }
        set(&mut result, path, None)?;
    }
    for (path, value) in changes {
        set(&mut result, path, Some(value.clone()))?;
    }
    Ok(result)
}
fn typed<T: serde::de::DeserializeOwned>(path: &str, value: Value) -> Result<T, Diagnostic> {
    serde_json::from_value(value).map_err(|e| error(path, e.to_string()))
}
fn set(desktop: &mut Desktop, path: &str, value: Option<Value>) -> Result<(), Diagnostic> {
    let defaults = Desktop::default();
    match path {
        "appearance.scheme" => {
            desktop.appearance.scheme =
                typed(path, value.unwrap_or(json!(defaults.appearance.scheme)))?
        }
        "appearance.mode" => {
            desktop.appearance.mode = typed(path, value.unwrap_or(json!(defaults.appearance.mode)))?
        }
        "appearance.contrast" => {
            desktop.appearance.contrast =
                typed(path, value.unwrap_or(json!(defaults.appearance.contrast)))?
        }
        "appearance.source" => {
            desktop.appearance.source = typed(path, value.unwrap_or(Value::Null))?
        }
        "ui.density" => {
            desktop.ui.density = typed(path, value.unwrap_or(json!(defaults.ui.density)))?
        }
        "ui.text_scale" => {
            desktop.ui.text_scale = typed(path, value.unwrap_or(json!(defaults.ui.text_scale)))?
        }
        "ui.reduced_motion" => {
            desktop.ui.reduced_motion =
                typed(path, value.unwrap_or(json!(defaults.ui.reduced_motion)))?
        }
        "shell.page_order" => desktop.shell.page_order = typed(path, value.unwrap_or(json!([])))?,
        _ => {
            let parts: Vec<_> = path.split('.').collect();
            match parts.as_slice() {
                ["shell", "panels", id] => {
                    key(id, path)?;
                    if let Some(value) = value {
                        desktop
                            .shell
                            .panels
                            .insert((*id).into(), typed(path, value)?);
                    } else if let Some(panel) = defaults.shell.panels.get(*id) {
                        desktop.shell.panels.insert((*id).into(), panel.clone());
                    } else {
                        desktop.shell.panels.remove(*id);
                    }
                }
                ["shell", "panels", id, "thickness"] => {
                    let panel = desktop
                        .shell
                        .panels
                        .get_mut(*id)
                        .ok_or_else(|| error(path, "Unknown panel"))?;
                    let number =
                        typed::<f64>(path, value.unwrap_or(json!(Panel::default().thickness)))?;
                    panel.thickness = crate::model::integer_u32(number)
                        .ok_or_else(|| error(path, "Expected an integral u32 number"))?;
                }
                ["apps", id] => {
                    key(id, path)?;
                    if let Some(value) = value {
                        desktop.apps.insert((*id).into(), typed(path, value)?);
                    } else {
                        desktop.apps.remove(*id);
                    }
                }
                _ => return Err(error(path, "Unknown field or scope")),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mix_whole_numbers_are_accepted_but_fractional_string_and_overflow_are_not() {
        let current = Desktop::default();
        for value in [json!(48), json!(48.0)] {
            let next = patch(
                &current,
                &BTreeMap::from([("shell.panels.bottom.thickness".into(), value.clone())]),
                &[],
            )
            .unwrap();
            assert_eq!(next.shell.panels["bottom"].thickness, 48);
            let record: Panel = serde_json::from_value(json!({"thickness":value})).unwrap();
            assert_eq!(record.thickness, 48);
        }
        for value in [json!(48.1), json!("48"), json!(-1), json!(4294967296u64)] {
            assert!(
                patch(
                    &current,
                    &BTreeMap::from([("shell.panels.bottom.thickness".into(), value.clone())]),
                    &[]
                )
                .is_err()
            );
            assert!(serde_json::from_value::<Panel>(json!({"thickness":value})).is_err());
        }
    }
    #[test]
    fn high_contrast_cannot_hide_invalid_authored_app_values() {
        let mut desktop = Desktop::default();
        desktop.appearance.contrast = "high".into();
        desktop.apps.insert(
            "term".into(),
            AppOverride {
                contrast: Some("typo".into()),
                ..Default::default()
            },
        );
        assert_eq!(resolve(&desktop).unwrap_err()[0].path, "apps.term.contrast");
        desktop.apps.get_mut("term").unwrap().contrast = Some("normal".into());
        assert_eq!(resolve(&desktop).unwrap()["app:term"].contrast, "high");
    }
    #[test]
    fn bounds_and_strict_source_fail_before_acceptance() {
        let mut desktop = Desktop::default();
        desktop.ui.density = f64::NAN;
        assert!(resolve(&desktop).is_err());
        desktop.ui.density = 1.0;
        desktop.appearance.source = Some("run_argv([\"do-not-execute\"])".into());
        assert!(resolve(&desktop).is_err());
        desktop.appearance.source = None;
        desktop.shell.page_order = vec!["same".into(), "same".into()];
        assert!(resolve(&desktop).is_err());
    }
    #[test]
    fn bad_batch_cannot_partially_mutate_and_unknown_fields_are_rejected() {
        let current = Desktop::default();
        assert!(
            patch(
                &current,
                &BTreeMap::from([
                    ("appearance.mode".into(), json!("dark")),
                    ("bad.field".into(), json!(2))
                ]),
                &[]
            )
            .is_err()
        );
        assert_eq!(current.appearance.mode, "light");
        assert!(serde_json::from_value::<Desktop>(json!({"typo":1})).is_err());
    }
    #[test]
    fn app_override_and_reset_do_not_change_other_apps() {
        let current = Desktop::default();
        let next = patch(
            &current,
            &BTreeMap::from([("apps.term".into(), json!({"mode":"dark"}))]),
            &[],
        )
        .unwrap();
        let effective = resolve(&next).unwrap();
        assert_eq!(effective["app:term"].mode, "dark");
        assert_eq!(effective["app:ced"].mode, "light");
        assert_eq!(
            patch(&next, &BTreeMap::new(), &["apps.term".into()]).unwrap(),
            current
        );
    }
}

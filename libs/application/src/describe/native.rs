// SPDX-License-Identifier: MIT OR Apache-2.0
//! Pure completion from an already reconciled native presentation owner.
use super::{Identity, Violation, code, complete};
use crate::presentation::native::Session;
use serde_json::{Map, Value};

fn evidence(path: &str, value: impl serde::Serialize) -> Result<Value, Violation> {
    serde_json::to_value(value)
        .map_err(|error| Violation::new(path, code::INVALID_EVIDENCE, error.to_string()))
}
fn install(
    object: &mut Map<String, Value>,
    key: &str,
    value: Value,
    path: &str,
) -> Result<(), Violation> {
    if let Some(existing) = object.get(key)
        && existing != &value
    {
        return Err(Violation::new(
            path,
            code::RESERVED_CONFLICT,
            "existing evidence contradicts its presentation owner",
        ));
    }
    object.insert(key.into(), value);
    Ok(())
}

/// Serialize the owner's installed settings, preparation, resources and optional
/// cache evidence. This performs no drain, transport sample, I/O, context change,
/// activation, ACK, layout or redraw. Product-specific preparation members remain.
/// Contradictions and validation failures leave the caller's value untouched.
pub fn complete_native<T, C>(
    value: &mut Value,
    identity: Identity<'_>,
    session: &Session<T, C>,
) -> Result<(), Violation> {
    let mut staged = value.clone();
    let object = staged.as_object_mut().ok_or_else(|| {
        Violation::new(
            "",
            code::INVALID_ROOT,
            "describe value must be a JSON object",
        )
    })?;
    #[cfg(not(feature = "settings-cache"))]
    if object.contains_key("settings_cache") {
        return Err(Violation::new(
            "settings_cache",
            code::RESERVED_CONFLICT,
            "cache evidence requires a cache-enabled presentation owner",
        ));
    }
    install(
        object,
        "settings",
        evidence("settings", session.host().consumer().evidence())?,
        "settings",
    )?;
    #[cfg(feature = "settings-cache")]
    install(
        object,
        "settings_cache",
        evidence("settings_cache", session.cache_evidence())?,
        "settings_cache",
    )?;
    let preparation = evidence("preparation", session.preparation_evidence())?;
    let target = object
        .entry("preparation")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| {
            Violation::new(
                "preparation",
                code::INVALID_EVIDENCE,
                "preparation must be an object",
            )
        })?;
    for (key, value) in preparation
        .as_object()
        .expect("typed preparation evidence is an object")
    {
        install(target, key, value.clone(), &format!("preparation.{key}"))?;
    }
    let resources = session
        .host()
        .presentation()
        .and_then(|presentation| presentation.appearance().resources());
    install(
        object,
        "resources",
        evidence("resources", resources.map(|resources| resources.evidence()))?,
        "resources",
    )?;
    complete(&mut staged, identity)?;
    *value = staged;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn owner() -> Session<std::rc::Rc<()>, std::rc::Rc<()>> {
        Session::with_context(
            settings::consumer::Consumer::for_app(
                settings::Binding {
                    instance: "fixture".into(),
                    profile: "default".into(),
                },
                "probe",
            )
            .unwrap(),
            std::rc::Rc::new(()),
        )
    }
    fn identity() -> Identity<'static> {
        Identity {
            app_id: None,
            version: "fixture",
            pid: 1,
            service: "probe",
        }
    }
    #[test]
    fn bootstrap_readback_is_pure_and_preserves_product_members_without_content_bounds() {
        let session = owner();
        let before = serde_json::to_value(session.preparation_evidence()).unwrap();
        let mut value = json!({"verbs":["probe.ping","app.describe"],"preparation":{"zoom":2.5},"document":{"text":"unfinished"}});
        complete_native(&mut value, identity(), &session).unwrap();
        assert_eq!(value["preparation"]["zoom"], 2.5);
        assert_eq!(value["preparation"]["desired"], 0);
        assert_eq!(value["preparation"]["applied"], Value::Null);
        assert_eq!(value["preparation"]["current"], false);
        assert_eq!(value["resources"], Value::Null);
        assert_eq!(value["document"]["text"], "unfinished");
        assert_eq!(
            serde_json::to_value(session.preparation_evidence()).unwrap(),
            before
        );
        assert!(session.frame_stamp().is_none());
        let completed = value.clone();
        complete_native(&mut value, identity(), &session).unwrap();
        assert_eq!(value, completed);
    }
    #[test]
    fn contradictory_owner_or_invalid_inventory_never_partially_completes() {
        let session = owner();
        for mut value in [
            json!({"verbs":["app.describe"],"preparation":{"current":true}}),
            json!({"verbs":["app.describe"],"settings":{"invented":true}}),
            json!({"verbs":["app.describe"],"resources":{"invented":true}}),
            json!({"verbs":["app.describe","app.describe"]}),
        ] {
            let before = value.clone();
            assert!(complete_native(&mut value, identity(), &session).is_err());
            assert_eq!(value, before);
        }
    }
}

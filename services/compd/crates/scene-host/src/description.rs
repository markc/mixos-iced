// SPDX-License-Identifier: MIT OR Apache-2.0
//! Pure discovery of the embedded owner's installed presentation.
use crate::appearance::Look;
use application::{describe, presentation::native::Session};
use serde_json::{Value, json};

pub(crate) fn complete(
    identity: describe::Identity<'_>,
    session: &Session<Look>,
) -> Result<Value, describe::Violation> {
    let mut verbs = crate::verb::SceneVerb::names(identity.service);
    verbs.push(describe::VERB.into());
    let mut value = json!({"schema":"quoin.v1","app":"quoin","embedded":true,"transport":"native","verbs":verbs});
    describe::complete_native(&mut value, identity, session)?;
    Ok(value)
}

pub(crate) fn refusal(error: &describe::Violation) -> Value {
    json!({"error_code":"ARGUMENT","message":error.to_string(),"describe_code":error.code,"path":error.path})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn embedded_description_uses_its_executable_identity_without_inventing_a_window() {
        let session = Session::new(
            settings::consumer::Consumer::for_shell(settings::Binding {
                instance: "fixture".into(),
                profile: "default".into(),
            })
            .unwrap(),
        );
        let identity = describe::Identity {
            app_id: None,
            version: "compd-fixture",
            pid: std::process::id(),
            service: "shell-overridden",
        };
        let preparation = serde_json::to_value(session.preparation_evidence()).unwrap();
        let value = complete(identity, &session).unwrap();
        describe::validate(&value).unwrap();
        assert_eq!(value["version"], "compd-fixture");
        assert_eq!(value["pid"], std::process::id());
        assert_eq!(value["service"], "shell-overridden");
        assert!(value["app_id"].is_null());
        assert!(value["resources"].is_null());
        assert!(value["settings_cache"].is_object());
        let mut expected = crate::verb::SceneVerb::names("shell-overridden");
        expected.push("app.describe".into());
        assert_eq!(value["verbs"], json!(expected));
        assert_eq!(value, complete(identity, &session).unwrap());
        assert_eq!(
            serde_json::to_value(session.preparation_evidence()).unwrap(),
            preparation
        );
        assert!(session.frame_stamp().is_none());
    }
}

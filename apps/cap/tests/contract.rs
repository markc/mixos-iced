// SPDX-License-Identifier: MIT OR Apache-2.0
//! The checked-in native surface, not a second test implementation.
use serde_json::json;
#[test]
fn every_cap_contract_is_registered_and_strict() {
    let registry = include_str!("../../../docs/spec/bus/verbs.conf.mix");
    for verb in [
        "cap.ping",
        "cap.info",
        "cap.capture",
        "cap.cancel",
        "cap.open",
        "cap.annotate",
        "cap.move",
        "cap.delete",
        "cap.crop",
        "cap.undo",
        "cap.redo",
        "cap.export",
        "cap.show",
        "cap.quit",
    ] {
        assert!(
            registry.contains(&format!("name:\"{verb}\"")),
            "unregistered {verb}"
        );
        assert!(cap::verbs::parse(verb, "[]").is_err());
    }
    assert!(
        cap::verbs::operation("cap.capture", json!({"window":{"id":1,"generation":2}})).is_err()
    );
    assert!(
        cap::verbs::operation(
            "cap.capture",
            json!({"mode":"window","window":{"id":1,"generation":2,"extra":true}})
        )
        .is_err()
    );
}

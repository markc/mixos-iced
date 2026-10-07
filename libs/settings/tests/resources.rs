// SPDX-License-Identifier: MIT OR Apache-2.0
use settings::{domains::ChangePlan, *};

/// Golden byte fixture of the default desktop as serialised before the
/// resources field existed. The optional field must stay omitted when None in
/// BOTH authored and effective serialisation, or every digest over old
/// profiles/snapshots would change.
const OLD_DEFAULT_DESKTOP: &str = "{\"appearance\":{\"scheme\":\"ocean\",\"mode\":\"light\",\"contrast\":\"normal\",\"source\":null},\"ui\":{\"density\":1.0,\"text_scale\":1.0,\"reduced_motion\":false},\"shell\":{\"panels\":{\"bottom\":{\"edge\":\"bottom\",\"mode\":\"dock\",\"thickness\":40}},\"page_order\":[]},\"apps\":{}}";

fn binding() -> Binding {
    Binding {
        instance: "fixture".into(),
        profile: "default".into(),
    }
}
fn reference(set_id: &str) -> ResourceReference {
    ResourceReference {
        schema: RESOURCE_SCHEMA,
        set_id: set_id.into(),
        manifest_blake3: "0".repeat(64),
        icons: None,
    }
}
fn snapshot_of(desktop: Desktop) -> Snapshot {
    Snapshot {
        schema: SCHEMA,
        binding: binding(),
        incarnation: "authority".into(),
        revision: Revision(1),
        design_revision: Revision(1),
        source_digest: source_digest(EMBEDDED_DEFAULT_SOURCE),
        effective: resolve(&desktop).unwrap(),
        desktop,
    }
}
#[test]
fn golden_old_default_desktop_bytes_and_digests_are_preserved() {
    let old: Desktop = serde_json::from_str(OLD_DEFAULT_DESKTOP).unwrap();
    assert_eq!(old, Desktop::default());
    assert_eq!(serde_json::to_string(&old).unwrap(), OLD_DEFAULT_DESKTOP);
    assert_eq!(
        serde_json::to_string(&Desktop::default()).unwrap(),
        OLD_DEFAULT_DESKTOP
    );
    assert_eq!(digest(&old).unwrap(), digest(&Desktop::default()).unwrap());
    // Old effective projections resolve identically and keep the field out of
    // every serialised context; digests over old and new resolution agree.
    let from_old = resolve(&old).unwrap();
    let fresh = resolve(&Desktop::default()).unwrap();
    assert_eq!(from_old, fresh);
    assert_eq!(digest(&from_old).unwrap(), digest(&fresh).unwrap());
    for effective in from_old.values() {
        let value = serde_json::to_value(effective).unwrap();
        assert!(value.get("resources").is_none(), "omission stays omitted");
    }
    // A snapshot built from the golden old bytes is digest-identical to one
    // built from the current defaults: old snapshot identity survives.
    assert_eq!(
        digest(&snapshot_of(old)).unwrap(),
        digest(&snapshot_of(Desktop::default())).unwrap()
    );
}
#[test]
fn explicit_reference_round_trips_strictly_and_carries_into_every_context() {
    let reference = reference("core-icons");
    let mut desktop = Desktop::default();
    desktop.appearance.resources = Some(reference.clone());
    let value = serde_json::to_value(&desktop).unwrap();
    assert_eq!(value["appearance"]["resources"]["schema"], RESOURCE_SCHEMA);
    assert_eq!(value["appearance"]["resources"]["set_id"], "core-icons");
    assert!(value["appearance"]["resources"].get("icons").is_none());
    assert_eq!(serde_json::from_value::<Desktop>(value).unwrap(), desktop);
    let effective = resolve(&desktop).unwrap();
    for context in effective.values() {
        assert_eq!(context.resources.as_ref(), Some(&reference));
    }
    // Resource identity participates in authority identity: the effective
    // digest differs from the omitted default, and the snapshot digest too.
    let plain = resolve(&Desktop::default()).unwrap();
    assert_ne!(digest(&effective).unwrap(), digest(&plain).unwrap());
    assert_ne!(
        digest(&snapshot_of(desktop)).unwrap(),
        digest(&snapshot_of(Desktop::default())).unwrap()
    );
}
#[test]
fn resource_change_plan_is_conservative_and_colour_only_skips_resources() {
    let default = snapshot_of(Desktop::default());
    let mut with_resources = Desktop::default();
    with_resources.appearance.resources = Some(reference("core-icons"));
    let referenced = snapshot_of(with_resources.clone());
    let plan = ChangePlan::between(Some(&default), &referenced, "app:ced", false);
    assert!(plan.resources && plan.text && plan.layout && plan.paint);
    assert!(!plan.motion && !plan.shell);
    // Icon-only changes inside the reference are equally conservative.
    let mut icons = with_resources;
    icons.appearance.resources.as_mut().unwrap().icons = Some(IconReference {
        family: "Symbols".into(),
        style: "rounded".into(),
        weight: 400,
    });
    let icon_change = snapshot_of(icons);
    let plan = ChangePlan::between(Some(&referenced), &icon_change, "app:ced", false);
    assert!(plan.resources && plan.text && plan.layout && plan.paint);
    // A colour-only change with unchanged (omitted) resources does not
    // re-register resources; paint alone is invalidated.
    let mut crimson = Desktop::default();
    crimson.appearance.scheme = "crimson".into();
    let recoloured = snapshot_of(crimson);
    let plan = ChangePlan::between(Some(&default), &recoloured, "app:ced", false);
    assert!(plan.paint && !plan.resources && !plan.text && !plan.layout);
}
#[test]
fn resource_interpretation_requires_no_optional_features() {
    let value = resource_interpretation();
    assert_eq!(value.len(), 64);
    assert!(value.bytes().all(|b| b.is_ascii_hexdigit()));
}
#[test]
fn strict_versions_and_unknown_fields_fail_without_partial_application() {
    // Unsupported subdocument schema is refused, never interpreted.
    let mut desktop = Desktop::default();
    desktop.appearance.resources = Some(ResourceReference {
        schema: RESOURCE_SCHEMA + 1,
        ..reference("core-icons")
    });
    assert_eq!(
        resolve(&desktop).unwrap_err()[0].code,
        "unsupported_resources"
    );
    // Unknown fields inside the subdocument fail strict deserialisation.
    let value = serde_json::json!({
        "schema": RESOURCE_SCHEMA,
        "set_id": "core-icons",
        "manifest_blake3": "0".repeat(64),
        "future": true
    });
    assert!(serde_json::from_value::<ResourceReference>(value).is_err());
    // Unknown nested patch paths fail whole-batch validation.
    let desktop = Desktop::default();
    assert!(
        resolve::patch(
            &desktop,
            &[("appearance.resources.icons".into(), serde_json::json!({}))]
                .into_iter()
                .collect(),
            &[],
        )
        .is_err()
    );
    assert_eq!(desktop, Desktop::default());
}

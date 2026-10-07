// SPDX-License-Identifier: MIT OR Apache-2.0
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// JSON/Mix integers must retain precision beyond 2^53. Numeric input refused.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Revision(pub u64);
impl Serialize for Revision {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.to_string())
    }
}
impl<'de> Deserialize<'de> for Revision {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        if text.is_empty()
            || (text.len() > 1 && text.starts_with('0'))
            || !text.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(serde::de::Error::custom(
                "revision must be canonical decimal u64 string",
            ));
        }
        text.parse::<u64>()
            .map(Self)
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub instance: String,
    pub profile: String,
}
impl Binding {
    pub fn validate(&self) -> Result<(), Diagnostic> {
        for (path, value) in [("instance", &self.instance), ("profile", &self.profile)] {
            if value.is_empty()
                || value.len() > 64
                || !value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            {
                return Err(Diagnostic::new(
                    "invalid_binding",
                    path,
                    "Use 1–64 ASCII letters, digits, dash or underscore",
                ));
            }
        }
        Ok(())
    }
}

/// Schema of the versioned `appearance.resources` subdocument. The top-level
/// snapshot schema is unchanged; unsupported subdocument versions fail.
pub const RESOURCE_SCHEMA: u32 = 1;

/// Envelope schema of resource-aware cache captures. Owned here (not by the
/// optional cache module) so the resource interpretation below is available
/// to every consumer regardless of feature selection.
pub(crate) const RESOURCE_CACHE_SCHEMA: u32 = 2;

/// Versioned resource-selection semantics for `ResourceBinding` and schema-2
/// cache envelopes. Available without the optional cache feature; the cache
/// module re-exports this exact value rather than duplicating the hash.
pub fn resource_interpretation() -> String {
    crate::source_digest(&format!(
        "settings-cache-resource-{RESOURCE_CACHE_SCHEMA}-{}-{}",
        env!("CARGO_PKG_VERSION"),
        crate::source_digest(crate::EMBEDDED_DEFAULT_SOURCE)
    ))
}

/// Structural identity of one icon catalogue selection inside a resource
/// reference. Renderer-local availability is consumer preparation, never
/// authority validation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IconReference {
    pub family: String,
    pub style: String,
    pub weight: u16,
}

/// Optional versioned authored resource reference. Omission keeps the
/// profile-pinned packaged default; the authority validates structure only and
/// never reads files or host font databases.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceReference {
    /// Exactly `RESOURCE_SCHEMA`; anything else is unsupported, not ignored.
    pub schema: u32,
    /// The assets set-ID contract: 1–96 ASCII letters, digits, `-` or `_`.
    pub set_id: String,
    /// Exactly 64 lowercase hex characters.
    pub manifest_blake3: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icons: Option<IconReference>,
}

/// Shared structural rules for authored references and cache bindings. Both
/// name the same immutable set identity, so one validator serves both.
fn validate_resource_identity(
    set_id: &str,
    manifest_blake3: &str,
    icons: Option<&IconReference>,
    path: &str,
) -> Result<(), Diagnostic> {
    if set_id.is_empty()
        || set_id.len() > 96
        || !set_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(Diagnostic::new(
            "invalid_resources",
            path,
            "set_id must be 1–96 ASCII letters, digits, dash or underscore",
        ));
    }
    if manifest_blake3.len() != 64
        || !manifest_blake3.bytes().all(|b| b.is_ascii_hexdigit())
        || manifest_blake3.bytes().any(|b| b.is_ascii_uppercase())
    {
        return Err(Diagnostic::new(
            "invalid_resources",
            path,
            "manifest_blake3 must be exactly 64 lowercase hex characters",
        ));
    }
    if let Some(icons) = icons {
        if icons.family.is_empty()
            || icons.family.len() > 256
            || icons.style.is_empty()
            || icons.style.len() > 96
            || !(1..=1000).contains(&icons.weight)
        {
            return Err(Diagnostic::new(
                "invalid_resources",
                path,
                "icons needs family (1–256 bytes), style (1–96 bytes) and an exact weight 1–1000",
            ));
        }
    }
    Ok(())
}
impl ResourceReference {
    /// Structural authority validation: exact subdocument schema plus the set
    /// identity contract. No local file, font or asset I/O occurs here.
    pub fn validate(&self, path: &str) -> Result<(), Diagnostic> {
        if self.schema != RESOURCE_SCHEMA {
            return Err(Diagnostic::new(
                "unsupported_resources",
                path,
                format!("Unsupported resources schema {}; expected {RESOURCE_SCHEMA}", self.schema),
            ));
        }
        validate_resource_identity(&self.set_id, &self.manifest_blake3, self.icons.as_ref(), path)
    }
}

/// Renderer-neutral record of the resource set a successful presentation was
/// bound to. For explicit references it must equal the authored reference; for
/// omission it records this host's actual pinned default identity. It never
/// carries face IDs, pointers, roots or font bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceBinding {
    /// Exactly `RESOURCE_SCHEMA`.
    pub schema: u32,
    pub set_id: String,
    pub manifest_blake3: String,
    /// Versioned resource-selection semantics; produced by the
    /// feature-independent `settings::resource_interpretation`, validated on
    /// cache load. The optional cache module re-exports the same value.
    pub interpretation: String,
    /// The authored optional icon selector, verbatim: None means the
    /// descriptor default. The actually resolved default family/style/weight
    /// is appearance evidence, never cache binding data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icons: Option<IconReference>,
}
impl ResourceBinding {
    /// Structural validation of a captured binding, before it enters a cache
    /// envelope. Selection-semantics interpretation is validated separately
    /// against the cache interpretation.
    pub fn validate(&self) -> Result<(), Diagnostic> {
        if self.schema != RESOURCE_SCHEMA {
            return Err(Diagnostic::new(
                "unsupported_resources",
                "cache.binding",
                format!(
                    "Unsupported resource binding schema {}; expected {RESOURCE_SCHEMA}",
                    self.schema
                ),
            ));
        }
        validate_resource_identity(
            &self.set_id,
            &self.manifest_blake3,
            self.icons.as_ref(),
            "cache.binding",
        )
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Appearance {
    pub scheme: String,
    pub mode: String,
    pub contrast: String,
    /// None selects the profile-pinned package source; Some is complete strict data.
    pub source: Option<String>,
    /// Omitted in BOTH authored and effective serialisation: serialising a
    /// null resources field would change old omitted-resource bytes and every
    /// digest over them (accepted profile, snapshot, cache envelope).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<ResourceReference>,
}
impl Default for Appearance {
    fn default() -> Self {
        Self {
            scheme: "ocean".into(),
            mode: "light".into(),
            contrast: "normal".into(),
            source: None,
            resources: None,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CommonUi {
    pub density: f64,
    pub text_scale: f64,
    pub reduced_motion: bool,
}
impl Default for CommonUi {
    fn default() -> Self {
        Self {
            density: 1.0,
            text_scale: 1.0,
            reduced_motion: false,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Panel {
    pub edge: String,
    pub mode: String,
    #[serde(deserialize_with = "deserialize_integer_u32")]
    pub thickness: u32,
}
pub(crate) fn integer_u32(value: f64) -> Option<u32> {
    (value.is_finite() && (0.0..=f64::from(u32::MAX)).contains(&value) && value.fract() == 0.0)
        .then_some(value as u32)
}
fn deserialize_integer_u32<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<u32, D::Error> {
    integer_u32(f64::deserialize(deserializer)?)
        .ok_or_else(|| serde::de::Error::custom("expected an integral u32 number"))
}
impl Default for Panel {
    fn default() -> Self {
        Self {
            edge: "bottom".into(),
            mode: "dock".into(),
            thickness: 40,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Shell {
    pub panels: BTreeMap<String, Panel>,
    pub page_order: Vec<String>,
}
impl Default for Shell {
    fn default() -> Self {
        Self {
            panels: BTreeMap::from([("bottom".into(), Panel::default())]),
            page_order: Vec::new(),
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppOverride {
    pub scheme: Option<String>,
    pub mode: Option<String>,
    pub contrast: Option<String>,
    pub text_scale: Option<f64>,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Desktop {
    pub appearance: Appearance,
    pub ui: CommonUi,
    pub shell: Shell,
    pub apps: BTreeMap<String, AppOverride>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Diagnostic {
    pub code: String,
    pub path: String,
    pub message: String,
}
impl Diagnostic {
    pub fn new(code: &str, path: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            path: path.into(),
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadRequest {
    pub binding: Binding,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplyRequest {
    pub binding: Binding,
    pub expected_incarnation: String,
    pub expected_revision: Revision,
    pub operation_id: String,
    pub changes: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub reset: Vec<String>,
    /// Optional supplied digest is checked; every receipt stores the computed digest.
    #[serde(default)]
    pub request_digest: Option<String>,
}
impl ApplyRequest {
    pub fn digest(&self) -> Result<String, serde_json::Error> {
        let mut canonical = self.clone();
        canonical.request_digest = None;
        crate::digest(&canonical)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Changed,
    Unchanged,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub operation_id: String,
    pub request_digest: String,
    pub incarnation: String,
    pub revision: Revision,
    pub outcome: Outcome,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Effective {
    pub scheme: String,
    pub mode: String,
    pub contrast: String,
    pub ui: CommonUi,
    pub design: design::DesignReadProjection,
    pub provenance: BTreeMap<String, String>,
    /// The exact authored reference in this context. Omission stays omitted in
    /// serialisation; apps never override resources, so every context carries
    /// the profile reference verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<ResourceReference>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub schema: u32,
    pub binding: Binding,
    pub incarnation: String,
    pub revision: Revision,
    pub design_revision: Revision,
    pub source_digest: String,
    pub desktop: Desktop,
    pub effective: BTreeMap<String, Effective>,
}
impl Snapshot {
    pub fn encoded_len(&self) -> Result<usize, serde_json::Error> {
        Ok(serde_json::to_vec(self)?.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn revisions_keep_full_precision_and_refuse_noncanonical_input() {
        let value = Revision(u64::MAX);
        assert_eq!(
            serde_json::from_str::<Revision>(&serde_json::to_string(&value).unwrap()).unwrap(),
            value
        );
        for input in [
            "9007199254740993",
            "\"01\"",
            "\"+1\"",
            "\"18446744073709551616\"",
        ] {
            assert!(serde_json::from_str::<Revision>(input).is_err());
        }
        assert_eq!(
            strict::from_str::<BTreeMap<String, Revision>>("{revision: \"9007199254740993\"}")
                .unwrap()["revision"]
                .0,
            9007199254740993
        );
    }
    fn reference() -> ResourceReference {
        ResourceReference {
            schema: RESOURCE_SCHEMA,
            set_id: "core-icons".into(),
            manifest_blake3: "0".repeat(64),
            icons: Some(IconReference {
                family: "Symbols".into(),
                style: "rounded".into(),
                weight: 400,
            }),
        }
    }
    #[test]
    fn resource_reference_is_strictly_versioned_and_rejects_unknown_fields() {
        reference().validate("appearance.resources").unwrap();
        let mut future = reference();
        future.schema = RESOURCE_SCHEMA + 1;
        assert_eq!(
            future.validate("appearance.resources").unwrap_err().code,
            "unsupported_resources"
        );
        let mut bad = reference();
        bad.set_id = "a/b".into();
        assert_eq!(
            bad.validate("appearance.resources").unwrap_err().code,
            "invalid_resources"
        );
        let mut bad = reference();
        bad.set_id = "x".repeat(97);
        assert!(bad.validate("appearance.resources").is_err());
        let mut bad = reference();
        bad.manifest_blake3 = "A".repeat(64);
        assert!(bad.validate("appearance.resources").is_err());
        let mut bad = reference();
        bad.manifest_blake3 = "ab".into();
        assert!(bad.validate("appearance.resources").is_err());
        let mut bad = reference();
        bad.icons.as_mut().unwrap().weight = 0;
        assert!(bad.validate("appearance.resources").is_err());
        let mut bad = reference();
        bad.icons.as_mut().unwrap().family = String::new();
        assert!(bad.validate("appearance.resources").is_err());
        let mut bad = reference();
        bad.icons.as_mut().unwrap().style = "y".repeat(97);
        assert!(bad.validate("appearance.resources").is_err());
        let wire = serde_json::to_value(reference()).unwrap();
        assert!(wire.get("icons").is_some());
        let without_icons: ResourceReference = serde_json::from_value({
            let mut wire = wire.clone();
            wire.as_object_mut().unwrap().remove("icons");
            wire
        })
        .unwrap();
        assert_eq!(without_icons.icons, None);
        assert!(serde_json::to_value(without_icons).unwrap().get("icons").is_none());
        let mut unknown = serde_json::to_value(reference()).unwrap();
        unknown["typo"] = serde_json::json!(true);
        assert!(serde_json::from_value::<ResourceReference>(unknown).is_err());
    }
    #[test]
    fn omitted_resources_stay_out_of_authored_and_effective_bytes() {
        let appearance = Appearance::default();
        let value = serde_json::to_value(&appearance).unwrap();
        assert!(value.get("resources").is_none());
        assert_eq!(
            serde_json::from_value::<Appearance>(value).unwrap().resources,
            None
        );
        let mut with_resources = appearance;
        with_resources.resources = Some(reference());
        let value = serde_json::to_value(&with_resources).unwrap();
        assert_eq!(
            serde_json::from_value::<Appearance>(value)
                .unwrap()
                .resources,
            with_resources.resources
        );
    }
    #[test]
    fn resource_binding_validates_the_same_set_identity_contract() {
        let binding = ResourceBinding {
            schema: RESOURCE_SCHEMA,
            set_id: "core-icons".into(),
            manifest_blake3: "f".repeat(64),
            interpretation: "versioned-selection-semantics".into(),
            icons: None,
        };
        binding.validate().unwrap();
        let mut bad = binding.clone();
        bad.schema = 2;
        assert_eq!(bad.validate().unwrap_err().code, "unsupported_resources");
        let mut bad = binding;
        bad.set_id = "";
        assert!(bad.validate().is_err());
        let value = serde_json::to_value(ResourceBinding {
            schema: RESOURCE_SCHEMA,
            set_id: "core-icons".into(),
            manifest_blake3: "f".repeat(64),
            interpretation: "versioned-selection-semantics".into(),
            icons: None,
        })
        .unwrap();
        assert!(value.get("icons").is_none());
    }
    #[test]
    fn resource_interpretation_is_available_without_the_cache_feature() {
        let value = resource_interpretation();
        assert_eq!(value.len(), 64);
        assert!(value.bytes().all(|b| b.is_ascii_hexdigit()));
        let again = resource_interpretation();
        assert_eq!(value, again, "the formula is stable within one release");
    }
}

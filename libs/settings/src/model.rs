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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Appearance {
    pub scheme: String,
    pub mode: String,
    pub contrast: String,
    /// None selects the profile-pinned package source; Some is complete strict data.
    pub source: Option<String>,
}
impl Default for Appearance {
    fn default() -> Self {
        Self {
            scheme: "ocean".into(),
            mode: "light".into(),
            contrast: "normal".into(),
            source: None,
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
}

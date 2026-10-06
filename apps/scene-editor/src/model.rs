// SPDX-License-Identifier: MIT OR Apache-2.0
//! Selection is app-owned; scene state and action plans are loader-owned.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const APP_ID: &str = "dev.mixos.scene-editor";
pub const SERVICE: &str = "scene-editor";
pub const EDGES: [&str; 4] = ["top", "bottom", "left", "right"];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum View {
    #[default]
    Gallery,
    Installed,
    Arrange,
}
impl View {
    pub const ALL: [Self; 3] = [Self::Gallery, Self::Installed, Self::Arrange];
    pub fn key(self) -> &'static str {
        match self {
            Self::Gallery => "gallery",
            Self::Installed => "installed",
            Self::Arrange => "arrange",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page {
    pub edge: String,
    pub page: String,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    #[serde(default)]
    pub view: View,
    pub template: Option<String>,
    pub scene: Option<String>,
    pub page: Option<Page>,
}
impl Selection {
    pub fn validate(&self) -> Result<(), String> {
        for value in [&self.template, &self.scene].into_iter().flatten() {
            if !valid_name(value) {
                return Err("invalid scene or template name".into());
            }
        }
        if let Some(page) = &self.page
            && (!EDGES.contains(&page.edge.as_str())
                || page.page.is_empty()
                || page.page.len() > 256)
        {
            return Err("invalid page selection".into());
        }
        Ok(())
    }
}
pub fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"-_".contains(&c))
}

#[derive(Debug, Clone, Default)]
pub struct Snapshot(pub Value);
impl Snapshot {
    pub fn parse(value: Value) -> Result<Self, String> {
        if value["schema"] != "scene-editor.snapshot.v1"
            || value["state_token"]
                .as_str()
                .is_none_or(|s| s.len() != 64 || !s.bytes().all(|c| c.is_ascii_hexdigit()))
            || !value["model"].is_object()
            || !value["inventory"]["scenes"].is_array()
            || !value["templates"]["templates"].is_array()
        {
            return Err("unsupported or malformed scene editor snapshot".into());
        }
        let selection: Selection = serde_json::from_value(value["selection"].clone())
            .map_err(|e| format!("invalid snapshot selection: {e}"))?;
        selection.validate()?;
        Ok(Self(value))
    }
    pub fn selection(&self) -> Selection {
        serde_json::from_value(self.0["selection"].clone()).unwrap_or_default()
    }
    pub fn scene(&self, selection: &Selection) -> Option<&Value> {
        self.0["inventory"]["scenes"]
            .as_array()?
            .iter()
            .find(|row| row["name"].as_str() == selection.scene.as_deref())
    }
    pub fn template(&self, selection: &Selection) -> Option<&Value> {
        self.0["templates"]["templates"]
            .as_array()?
            .iter()
            .find(|row| row["template"].as_str() == selection.template.as_deref())
    }
    pub fn writable(&self) -> bool {
        self.0["inventory"]["state_ok"].as_bool() == Some(true)
    }
    pub fn page_owner(&self, page: &str) -> Option<String> {
        self.0["inventory"]["scenes"]
            .as_array()?
            .iter()
            .find(|row| row["page"].as_str() == Some(page))?["name"]
            .as_str()
            .map(str::to_owned)
    }
    pub fn model(&self) -> &Value {
        &self.0["model"]
    }
}
pub fn string(value: &Value) -> &str {
    value.as_str().unwrap_or("")
}
pub fn rows(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or_default()
}

/// Launcher URI carries a validated selection, never source or a file path.
pub fn launch_selection(uri: &str) -> Result<Selection, String> {
    let suffix = uri
        .strip_prefix("mixos-scene-editor:")
        .ok_or("unknown launcher URI")?;
    let (view, scene) = suffix
        .split_once('/')
        .ok_or("launcher URI needs view/scene")?;
    let view = match view {
        "gallery" => View::Gallery,
        "installed" => View::Installed,
        "arrange" => View::Arrange,
        _ => return Err("unknown launcher view".into()),
    };
    let selection = Selection {
        view,
        scene: (!scene.is_empty()).then(|| scene.into()),
        ..Default::default()
    };
    selection.validate()?;
    Ok(selection)
}

pub fn describe() -> Value {
    json!({"schema":"scene-editor.v1", "app_id":APP_ID, "version":env!("CARGO_PKG_VERSION"),
        "transport":"native", "verbs":["scene-editor.ping","scene-editor.info","scene-editor.show",
        "scene-editor.action","scene-editor.quit"], "views":View::ALL.map(View::key)})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn launch_arguments_are_data_and_never_paths_or_code() {
        assert_eq!(
            launch_selection("mixos-scene-editor:installed/panel")
                .unwrap()
                .scene
                .as_deref(),
            Some("panel")
        );
        for uri in [
            "/tmp/a.mix",
            "mixos-scene-editor:unknown/panel",
            "mixos-scene-editor:gallery/../x",
            "mixos-scene-editor:gallery/X",
            "mixos-scene-editor:gallery/a/b",
        ] {
            assert!(launch_selection(uri).is_err());
        }
        assert!(
            launch_selection("mixos-scene-editor:gallery/")
                .unwrap()
                .scene
                .is_none()
        );
    }
    #[test]
    fn bad_snapshots_never_replace_authoritative_state() {
        assert!(Snapshot::parse(json!({"schema":"old", "model":{}})).is_err());
        assert!(Snapshot::parse(json!({"schema":"scene-editor.snapshot.v1", "model":{}, "inventory":{"scenes":[]}, "templates":{"templates":[]}, "selection":{"view":"arrange", "scene":"../x"}})).is_err());
    }
}

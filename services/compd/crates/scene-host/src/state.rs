//! Shared Quoin state: <var>/quoin.state.mix, v3 connector keys.
//! v1 pinned=true migrates to docked; v1/v2's `default` is claimed once.
//! Invalid/unreadable state disables writing rather than destroying it.
//! MIXOS_VAR selects both reads and writes; no fallback after a failure.
//! State and its atomic temporary file stay in that directory, without
//! following a linked state file (unlike the editable conf.mix).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use config::{Dir, Value, parse as parse_mix_data};
use edges::{Edge, PanelConfig, PanelMode, ShellModel};

#[derive(Clone, Debug, Default, PartialEq)]
struct SavedEdge {
    mode: PanelMode,
    page: String,
    thickness: Option<f32>,
}

#[derive(Clone, Debug, PartialEq)]
struct Saved {
    scheme: String,
    outputs: BTreeMap<String, [SavedEdge; 4]>,
}

impl Default for Saved {
    fn default() -> Self {
        Self {
            scheme: "builtin".into(),
            outputs: BTreeMap::new(),
        }
    }
}

fn identity(output: &str) -> Option<String> {
    (!output.is_empty() && !output.starts_with("wl-output-")).then(|| format!("connector:{output}"))
}

fn edge_set(value: &Value, legacy: bool) -> Result<[SavedEdge; 4], String> {
    let invalid = || "invalid Quoin state fields".to_owned();
    let Value::Map(fields) = value else {
        return Err(invalid());
    };
    let mut edges = std::array::from_fn(|_| SavedEdge::default());
    for edge in Edge::ALL {
        let Some(Value::Map(entry)) = fields.get(crate::conf::edge_name(edge)) else {
            return Err(invalid());
        };
        let Some(Value::String(page)) = entry.get("page") else {
            return Err(invalid());
        };
        let thickness = match entry.get("thickness_px") {
            Some(Value::Number(n)) => Some(*n as f32),
            None if !legacy => None,
            _ => return Err(invalid()),
        };
        if entry.len() != 2 + usize::from(thickness.is_some()) {
            return Err(invalid());
        }
        let mode = if legacy {
            match entry.get("pinned") {
                Some(Value::Bool(true)) => PanelMode::Docked,
                Some(Value::Bool(false)) => PanelMode::Hidden,
                _ => return Err(invalid()),
            }
        } else {
            let Some(Value::String(mode)) = entry.get("mode") else {
                return Err(invalid());
            };
            PanelMode::parse(mode).ok_or_else(invalid)?
        };
        if let Some(thickness) = thickness {
            PanelConfig::new(
                thickness,
                Duration::from_millis(800),
                Duration::from_millis(200),
            )
            .map_err(|error| error.to_string())?;
        }
        edges[edge.index()] = SavedEdge {
            mode,
            page: page.clone(),
            thickness,
        };
    }
    Ok(edges)
}

impl Saved {
    fn parse(source: &str) -> Result<Self, String> {
        let value = parse_mix_data(source).map_err(|error| error.to_string())?;
        let invalid = || "invalid Quoin state fields".to_owned();
        let Value::Map(root) = &value else {
            return Err(invalid());
        };
        let Some(Value::String(scheme)) = root.get("scheme") else {
            return Err(invalid());
        };
        let mut saved = Self {
            scheme: scheme.clone(),
            ..Self::default()
        };
        match root.get("version") {
            None if root.len() == 5 => {
                saved
                    .outputs
                    .insert("default".into(), edge_set(&value, true)?);
            }
            Some(Value::Number(v)) if *v == 2.0 && root.len() == 6 => {
                saved
                    .outputs
                    .insert("default".into(), edge_set(&value, false)?);
            }
            Some(Value::Number(v)) if *v == 3.0 && root.len() == 3 => {
                let Some(Value::Map(outputs)) = root.get("outputs") else {
                    return Err(invalid());
                };
                for (key, value) in outputs.iter() {
                    let Value::Map(fields) = value else {
                        return Err(invalid());
                    };
                    if key.trim().is_empty() || fields.len() != 4 {
                        return Err(invalid());
                    }
                    saved.outputs.insert(key.clone(), edge_set(value, false)?);
                }
            }
            _ => return Err(invalid()),
        }
        Ok(saved)
    }

    fn encode(&self) -> Result<String, String> {
        let outputs = self
            .outputs
            .iter()
            .map(|(key, edges)| {
                let fields = Edge::ALL
                    .into_iter()
                    .map(|edge| {
                        let saved = &edges[edge.index()];
                        let mut fields = BTreeMap::from([
                            ("mode".into(), Value::String(saved.mode.as_str().into())),
                            ("page".into(), Value::String(saved.page.clone())),
                        ]);
                        if let Some(thickness) = saved.thickness {
                            fields
                                .insert("thickness_px".into(), Value::Number(f64::from(thickness)));
                        }
                        (
                            crate::conf::edge_name(edge).into(),
                            Value::map(fields.into_iter().collect()),
                        )
                    })
                    .collect();
                (key.clone(), Value::map(fields))
            })
            .collect();
        Value::map(
            [
                ("version".into(), Value::Number(3.0)),
                ("scheme".into(), Value::String(self.scheme.clone())),
                ("outputs".into(), Value::map(outputs)),
            ]
            .into_iter()
            .collect(),
        )
        .encode_pretty()
        .map_err(|error| error.to_string())
    }
}

/// Defaults deliberately do no disk IO: only the real host enables state.
#[derive(Default)]
pub(crate) struct StateStore {
    path: Option<PathBuf>,
    saved: Saved,
    writer: Option<std::sync::mpsc::Sender<Saved>>,
}

impl StateStore {
    pub(crate) fn startup() -> Self {
        let mut store = Self::load(&config::path(Dir::Var).join("quoin.state.mix"));
        if let Some(path) = store.path.clone() {
            let (sender, receiver) = std::sync::mpsc::channel::<Saved>();
            // One ordered writer, off the compositor thread. Persist after a
            // quiet interval, coalescing rapid carousel/mode changes.
            match std::thread::Builder::new()
                .name("scene-state".into())
                .spawn(move || {
                    while let Ok(mut next) = receiver.recv() {
                        while let Ok(newer) = receiver.recv_timeout(Duration::from_millis(300)) {
                            next = newer;
                        }
                        if let Err(error) = next
                            .encode()
                            .and_then(|encoded| crate::conf::replace_atomically_at(&path, &encoded))
                        {
                            tracing::warn!(
                                "scene host: Quoin state {} save failed: {error}",
                                path.display()
                            );
                        }
                    }
                }) {
                Ok(_) => store.writer = Some(sender),
                Err(error) => {
                    tracing::warn!(
                        "scene host: state writer unavailable, persistence disabled: {error}"
                    );
                    store.path = None;
                }
            }
        }
        store
    }

    pub(crate) fn load(path: &Path) -> Self {
        if path.is_symlink() {
            tracing::warn!(
                "scene host: Quoin state {} is a symlink; persistence disabled",
                path.display()
            );
            return Self::default();
        }
        match std::fs::read_to_string(path) {
            Ok(source) => match Saved::parse(&source) {
                Ok(saved) => Self {
                    path: Some(path.into()),
                    saved,
                    writer: None,
                },
                Err(error) => {
                    tracing::warn!(
                        "scene host: Quoin state {} invalid; persistence disabled: {error}",
                        path.display()
                    );
                    Self::default()
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self {
                path: Some(path.into()),
                ..Self::default()
            },
            Err(error) => {
                tracing::warn!(
                    "scene host: Quoin state {} unreadable; persistence disabled: {error}",
                    path.display()
                );
                Self::default()
            }
        }
    }

    pub(crate) fn restore(&mut self, model: &mut ShellModel) {
        let Some(key) = identity(model.output().as_str()) else {
            return;
        };
        let Some(edges) = self
            .saved
            .outputs
            .remove(&key)
            .or_else(|| self.saved.outputs.remove("default"))
        else {
            return;
        };
        for edge in Edge::ALL {
            let saved = &edges[edge.index()];
            if let Some(thickness) = saved.thickness {
                model
                    .restore_thickness(edge, thickness)
                    .expect("validated saved thickness");
            }
            model
                .carousel_mut(edge)
                .restore_saved_selection(&saved.page);
            model
                .restore_mode(edge, model.last_update(), saved.mode)
                .expect("model time");
        }
        self.saved.outputs.insert(key, edges);
    }

    /// Only called for accepted mode/page/resize changes, never hover or tick.
    pub(crate) fn save(&mut self, model: &ShellModel) {
        let Some(path) = self.path.as_deref() else {
            return;
        };
        let Some(key) = identity(model.output().as_str()) else {
            return;
        };
        let edges = Edge::ALL.map(|edge| {
            let panel = model.persistent_panel(edge);
            let carousel = model.carousel(edge);
            SavedEdge {
                mode: panel.mode,
                page: carousel
                    .pending_restore()
                    .or_else(|| carousel.active_id())
                    .unwrap_or_default()
                    .into(),
                thickness: panel.remembered.then_some(panel.thickness),
            }
        });
        if self.saved.outputs.get(&key) == Some(&edges) {
            return;
        }
        let mut next = self.saved.clone();
        next.outputs.insert(key, edges);
        if let Some(writer) = &self.writer {
            match writer.send(next.clone()) {
                Ok(()) => self.saved = next,
                Err(error) => tracing::warn!("scene host: state writer stopped: {error}"),
            }
            return;
        }
        match next
            .encode()
            .and_then(|encoded| crate::conf::replace_atomically_at(path, &encoded))
        {
            Ok(()) => self.saved = next,
            Err(error) => tracing::warn!(
                "scene host: Quoin state {} save failed: {error}",
                path.display()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use edges::{LogicalSize, OutputKey};

    #[test]
    fn carousel_save_queues_complete_state_without_opening_the_path() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut store = StateStore {
            path: Some(PathBuf::from("/nonexistent/scene-state.mix")),
            saved: Saved::default(),
            writer: Some(sender),
        };
        let mut current = model("DP-1");
        current
            .carousel_mut(Edge::Right)
            .restore_saved_selection("scene-calendar");
        store.save(&current);
        current
            .carousel_mut(Edge::Right)
            .restore_saved_selection("scene-notes");
        store.save(&current);
        let queued: Vec<_> = receiver.try_iter().collect();
        assert_eq!(queued.len(), 2);
        assert_eq!(
            queued[1].outputs["connector:DP-1"][Edge::Right.index()].page,
            "scene-notes"
        );
        assert_eq!(store.saved, queued[1]);
    }

    #[test]
    fn saving_page_selection_during_settings_ownership_keeps_local_mode_and_size() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut store = StateStore {
            path: Some(PathBuf::from("/nonexistent/scene-state.mix")),
            saved: Saved::default(),
            writer: Some(sender),
        };
        let mut current = model("DP-1");
        current
            .restore_mode(Edge::Left, Duration::ZERO, PanelMode::Pinned)
            .unwrap();
        current.restore_thickness(Edge::Left, 180.0).unwrap();
        let mut values = [None; 4];
        values[Edge::Left.index()] =
            Some(edges::PanelPreference::new(PanelMode::Docked, 256.0).unwrap());
        current.set_preferences(values, Duration::ZERO);
        current
            .carousel_mut(Edge::Left)
            .restore_saved_selection("scene-new");
        store.save(&current);
        let saved = receiver.recv().unwrap();
        let edge = &saved.outputs["connector:DP-1"][Edge::Left.index()];
        assert_eq!(edge.mode, PanelMode::Pinned);
        assert_eq!(edge.thickness, Some(180.0));
        assert_eq!(edge.page, "scene-new");
        assert_eq!(current.panel(Edge::Left).mode, PanelMode::Docked);
    }

    fn model(name: &str) -> ShellModel {
        ShellModel::new(
            OutputKey::new(name).unwrap(),
            LogicalSize::new(1280.0, 800.0).unwrap(),
            Duration::ZERO,
            Duration::from_millis(800),
            Duration::from_millis(200),
        )
        .unwrap()
    }

    #[test]
    fn state_round_trip_restores_modes_and_preserves_other_outputs_and_scheme() {
        let path =
            std::env::temp_dir().join(format!("compd-quoin-state-{}.mix", std::process::id()));
        let mut saved = Saved {
            scheme: "Ocean".into(),
            ..Saved::default()
        };
        saved.outputs.insert(
            "connector:HDMI-1".into(),
            std::array::from_fn(|_| SavedEdge::default()),
        );
        std::fs::write(&path, saved.encode().unwrap()).unwrap();
        let mut store = StateStore::load(&path);
        let mut first = model("DP-1");
        first
            .restore_mode(Edge::Bottom, Duration::ZERO, PanelMode::Pinned)
            .unwrap();
        first
            .restore_mode(Edge::Right, Duration::ZERO, PanelMode::Docked)
            .unwrap();
        first.restore_thickness(Edge::Right, 360.0).unwrap();
        first
            .carousel_mut(Edge::Right)
            .restore_saved_selection("scene-notes");
        store.save(&first);
        let mut restarted = StateStore::load(&path);
        let mut second = model("DP-1");
        restarted.restore(&mut second);
        assert_eq!(second.panel(Edge::Bottom).mode, PanelMode::Pinned);
        assert_eq!(second.panel(Edge::Right).mode, PanelMode::Docked);
        assert_eq!(
            second.carousel(Edge::Right).pending_restore(),
            Some("scene-notes")
        );
        assert!(second.has_remembered_thickness(Edge::Right));
        assert_eq!(restarted.saved.scheme, "Ocean");
        assert!(restarted.saved.outputs.contains_key("connector:HDMI-1"));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn legacy_pin_migrates_to_dock_and_default_is_claimed_once() {
        let edge = "{pinned: true, page: \"\", thickness_px: 52}";
        let source = format!(
            "{{scheme: \"builtin\", left: {edge}, bottom: {edge}, right: {edge}, top: {edge}}}"
        );
        let mut store = StateStore {
            path: None,
            saved: Saved::parse(&source).unwrap(),
            writer: None,
        };
        store.restore(&mut model("wl-output-1"));
        assert!(store.saved.outputs.contains_key("default"));
        let mut first = model("DP-1");
        store.restore(&mut first);
        assert_eq!(first.panel(Edge::Bottom).mode, PanelMode::Docked);
        let mut other = model("DP-2");
        store.restore(&mut other);
        assert_eq!(other.panel(Edge::Bottom).mode, PanelMode::Hidden);
        assert!(!store.saved.outputs.contains_key("default"));
        assert!(Saved::parse("{version: 4}").is_err());
    }

    #[test]
    fn invalid_state_is_never_overwritten() {
        let path =
            std::env::temp_dir().join(format!("compd-quoin-invalid-{}.mix", std::process::id()));
        std::fs::write(&path, "broken").unwrap();
        let mut store = StateStore::load(&path);
        store.save(&model("DP-1"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "broken");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn state_links_cannot_restore_or_overwrite_another_runs_state() {
        use std::os::unix::fs::symlink;

        let dir = std::env::temp_dir().join(format!("compd-quoin-links-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("run/var")).unwrap();
        let other = dir.join("other.state.mix");
        let path = dir.join("run/var/quoin.state.mix");
        let mut first = model("winit");
        first
            .restore_mode(Edge::Left, Duration::ZERO, PanelMode::Docked)
            .unwrap();
        let mut store = StateStore::load(&other);
        store.save(&first);
        let original = std::fs::read_to_string(&other).unwrap();

        symlink(&other, &path).unwrap();
        let mut linked = StateStore::load(&path);
        let mut fresh = model("winit");
        linked.restore(&mut fresh);
        assert_eq!(fresh.panel(Edge::Left).mode, PanelMode::Hidden);
        linked.save(&fresh);
        assert_eq!(std::fs::read_to_string(&other).unwrap(), original);

        // A link introduced after startup must also be refused on save.
        std::fs::remove_file(&path).unwrap();
        let mut private = StateStore::load(&path);
        symlink(&other, &path).unwrap();
        private.save(&fresh);
        assert_eq!(std::fs::read_to_string(&other).unwrap(), original);
        assert!(path.is_symlink());
        std::fs::remove_dir_all(dir).unwrap();
    }
}

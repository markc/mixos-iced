// SPDX-License-Identifier: MIT OR Apache-2.0
//! Prepare the global profile once; each output fits it in its existing model.
use edges::{Edge, PanelMode, PanelPreference};
use settings::{Diagnostic, Shell};

#[derive(Clone, Debug, Default)]
pub(crate) struct Preferences {
    records: [Option<Record>; 4],
}

#[derive(Clone, Debug)]
pub(crate) struct Record {
    pub id: String,
    pub preference: PanelPreference,
}

impl Preferences {
    pub fn prepare(shell: &Shell) -> Result<Self, Diagnostic> {
        if !shell.page_order.is_empty() {
            return Err(Diagnostic::new(
                "unsupported_presentation",
                "shell.page_order",
                "Global page ordering has no edge/page identity mapping",
            ));
        }
        let mut policy = Self::default();
        for (id, panel) in &shell.panels {
            let path = format!("shell.panels.{id}");
            let edge = match panel.edge.as_str() {
                "left" => Edge::Left,
                "bottom" => Edge::Bottom,
                "right" => Edge::Right,
                "top" => Edge::Top,
                _ => {
                    return Err(Diagnostic::new(
                        "unsupported_presentation",
                        &path,
                        "Unknown panel edge",
                    ));
                }
            };
            if policy.records[edge.index()].is_some() {
                return Err(Diagnostic::new(
                    "unsupported_presentation",
                    &path,
                    "Multiple settings records target the same edge",
                ));
            }
            let mode = match panel.mode.as_str() {
                "dock" => PanelMode::Docked,
                "overlay" => PanelMode::Pinned,
                "hidden" => PanelMode::Hidden,
                _ => {
                    return Err(Diagnostic::new(
                        "unsupported_presentation",
                        &path,
                        "Unknown panel mode",
                    ));
                }
            };
            if !(16..=256).contains(&panel.thickness) {
                return Err(Diagnostic::new(
                    "unsupported_presentation",
                    &path,
                    "Invalid requested thickness",
                ));
            }
            let preference =
                PanelPreference::new(mode, panel.thickness as f32).map_err(|error| {
                    Diagnostic::new("unsupported_presentation", &path, &error.to_string())
                })?;
            policy.records[edge.index()] = Some(Record {
                id: id.clone(),
                preference,
            });
        }
        Ok(policy)
    }

    pub fn values(&self) -> [Option<PanelPreference>; 4] {
        std::array::from_fn(|index| self.records[index].as_ref().map(|record| record.preference))
    }

    pub fn record(&self, edge: Edge) -> Option<&Record> {
        self.records[edge.index()].as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arbitrary_ids_map_to_unique_edges_and_modes() {
        let mut shell = Shell::default();
        shell.panels.clear();
        for (edge, name, mode, expected) in [
            (Edge::Top, "top", "dock", PanelMode::Docked),
            (Edge::Left, "left", "overlay", PanelMode::Pinned),
            (Edge::Right, "right", "hidden", PanelMode::Hidden),
        ] {
            shell.panels.insert(
                format!("record-{name}"),
                settings::Panel {
                    edge: name.into(),
                    mode: mode.into(),
                    thickness: 16,
                },
            );
            let policy = Preferences::prepare(&shell).unwrap();
            let record = policy.record(edge).unwrap();
            assert_eq!(record.id, format!("record-{name}"));
            assert_eq!(record.preference.mode(), expected);
            assert_eq!(record.preference.thickness(), 16.0);
        }
    }

    #[test]
    fn ambiguous_edges_and_unowned_order_refuse_the_whole_policy() {
        let mut shell = Shell::default();
        shell
            .panels
            .insert("another".into(), settings::Panel::default());
        assert_eq!(
            Preferences::prepare(&shell).unwrap_err().code,
            "unsupported_presentation"
        );
        shell.panels.remove("another");
        shell.page_order.push("page".into());
        assert_eq!(
            Preferences::prepare(&shell).unwrap_err().path,
            "shell.page_order"
        );
    }
}

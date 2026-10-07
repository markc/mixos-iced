// SPDX-License-Identifier: MIT OR Apache-2.0
//! Versioned render data only. This DTO cannot construct an accepted design or
//! participate in its apply lineage; recipes/ownership/provenance remain opaque.
use crate::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadPair {
    pub surface: [f64; 4],
    pub foreground: [f64; 4],
    pub rendered_surface: [f64; 4],
    pub rendered_foreground: [f64; 4],
    pub backdrop: Option<[f64; 4]>,
    pub contrast_ratio: f64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadMetric {
    pub kind: String,
    pub value: f64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadType {
    pub family: String,
    pub fallbacks: Vec<String>,
    pub generic: String,
    pub font_size: f64,
    pub weight: u16,
    pub line_height: Option<f64>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadButton {
    pub variant: String,
    pub size: String,
    pub interaction: String,
    pub focus_visible: bool,
    pub pair: ReadPair,
    pub border: Option<[f64; 4]>,
    pub ring: Option<[f64; 4]>,
    pub height: f64,
    pub min_width: f64,
    pub padding_x: f64,
    pub border_width: f64,
    pub radius: f64,
    pub typography: BTreeMap<String, String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesignReadProjection {
    pub schema: u32,
    pub source: String,
    pub primitives: BTreeMap<String, [f64; 4]>,
    pub pairs: BTreeMap<String, ReadPair>,
    pub non_text: BTreeMap<String, [f64; 4]>,
    pub metrics: BTreeMap<String, ReadMetric>,
    pub scales: BTreeMap<String, Vec<f64>>,
    pub typography: BTreeMap<String, ReadType>,
    pub buttons: Vec<ReadButton>,
}
fn rgba(value: LinearRgba) -> [f64; 4] {
    [value.red, value.green, value.blue, value.alpha]
}
fn pair(value: &ResolvedPair) -> ReadPair {
    ReadPair {
        surface: rgba(value.surface),
        foreground: rgba(value.foreground),
        rendered_surface: rgba(value.rendered_surface),
        rendered_foreground: rgba(value.rendered_foreground),
        backdrop: value.backdrop.map(rgba),
        contrast_ratio: value.contrast_ratio,
    }
}
fn project(
    source: &SourceIdentity,
    dict: &ResolvedDictionary,
    types: &ResolvedTypography,
    tables: &ResolvedTables,
) -> DesignReadProjection {
    let mut buttons = Vec::with_capacity(BUTTON_CELL_COUNT);
    for variant in ButtonVariant::ALL {
        for size in ButtonSize::ALL {
            for interaction in InteractionState::ALL {
                for focus_visible in [false, true] {
                    let cell = tables.button.cell(ButtonCellKey {
                        variant,
                        size,
                        interaction,
                        focus_visible,
                    });
                    buttons.push(ReadButton {
                        variant: variant.name().into(),
                        size: size.name().into(),
                        interaction: interaction.name().into(),
                        focus_visible,
                        pair: pair(&cell.pair),
                        border: cell.border.map(rgba),
                        ring: cell.ring.map(rgba),
                        height: cell.height,
                        min_width: cell.min_width,
                        padding_x: cell.padding_x,
                        border_width: cell.border_width,
                        radius: cell.radius,
                        typography: ButtonPart::ALL
                            .into_iter()
                            .map(|part| {
                                (
                                    part.name().into(),
                                    types
                                        .button(ButtonTypographyKey {
                                            variant,
                                            size,
                                            part,
                                        })
                                        .name
                                        .into(),
                                )
                            })
                            .collect(),
                    });
                }
            }
        }
    }
    DesignReadProjection {
        schema: 1,
        source: source.as_str().into(),
        primitives: dict
            .colours
            .primitives
            .iter()
            .map(|(k, v)| (k.clone(), rgba(*v)))
            .collect(),
        pairs: dict
            .colours
            .pairs
            .iter()
            .map(|(k, v)| (k.clone(), pair(v)))
            .collect(),
        non_text: dict
            .colours
            .non_text
            .iter()
            .map(|(k, v)| (k.clone(), rgba(v.value)))
            .collect(),
        metrics: dict
            .metrics
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    ReadMetric {
                        kind: match v.kind {
                            ResolvedMetricKind::Px => "px",
                            ResolvedMetricKind::Ratio => "ratio",
                        }
                        .into(),
                        value: v.value,
                    },
                )
            })
            .collect(),
        scales: dict.scales.clone(),
        typography: types
            .scale()
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    ReadType {
                        family: v.family.clone(),
                        fallbacks: v.fallbacks.clone(),
                        generic: match v.generic {
                            TypographyGeneric::SansSerif => "sans_serif",
                            TypographyGeneric::Monospace => "monospace",
                        }
                        .into(),
                        font_size: v.font_size,
                        weight: v.weight,
                        line_height: v.line_height,
                    },
                )
            })
            .collect(),
        buttons,
    }
}
impl ResolvedDesign {
    pub fn read_projection(&self) -> DesignReadProjection {
        project(
            self.source(),
            self.dictionary(),
            self.typography(),
            self.tables(),
        )
    }
}
impl UnstampedResolvedDesign {
    pub fn read_projection(&self) -> DesignReadProjection {
        project(
            self.source(),
            self.dictionary(),
            self.typography(),
            self.tables(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn projection_round_trips_without_changing_lineage_or_cell_data() {
        let doc =
            parse_design_source(SourceIdentity::new("embedded"), EMBEDDED_DEFAULT_SOURCE).unwrap();
        let result = compile_design(&doc, DesignContext::default());
        let transition = apply_compiled_design(None, result, std::time::SystemTime::UNIX_EPOCH);
        let live = transition.design.unwrap();
        let view = live.read_projection();
        assert_eq!(view.buttons.len(), BUTTON_CELL_COUNT);
        let text = strict::to_string_pretty(&view).unwrap();
        assert_eq!(
            strict::from_str::<DesignReadProjection>(&text).unwrap(),
            view
        );
        assert_eq!(live.revision(), DesignRevision::FIRST);
        let key = ButtonCellKey {
            variant: ButtonVariant::Primary,
            size: ButtonSize::Md,
            interaction: InteractionState::Hovered,
            focus_visible: true,
        };
        let cell = view
            .buttons
            .iter()
            .find(|c| {
                c.variant == "primary"
                    && c.size == "md"
                    && c.interaction == "hovered"
                    && c.focus_visible
            })
            .unwrap();
        assert_eq!(cell.pair, pair(&live.tables().button.cell(key).pair));
    }
}

// SPDX-License-Identifier: MIT OR Apache-2.0
//! Settings authority read data to one prepared toolkit presentation. No source
//! compilation, transport, font installation or renderer swap here. Registered
//! font faces may load during worker preparation, never in a draw callback.
//! Preparation belongs on the host's worker; activation belongs on its UI loop.
use design::{
    LinearRgba, ResolvedColours, ResolvedDictionary, ResolvedMetric, ResolvedMetricKind,
    ResolvedNonTextColour, ResolvedPair, ResolvedTypeRecord, TypographyGeneric, TypographyRole,
};
use settings::{Diagnostic, Effective};
use std::collections::BTreeMap;
use toolkit::{
    Tokens,
    fonts::{FontChoice, FontSelection, Role},
    typography::{TextStyle, Typography},
};

/// Read-only compiler data, deliberately without accepted-design ownership,
/// recipes or source provenance. Existing deliberate content mappers may borrow
/// the dictionary; standard controls use the shared tokens and typography.
#[derive(Clone, Debug)]
pub struct Projection {
    dictionary: ResolvedDictionary,
    types: BTreeMap<String, ResolvedTypeRecord>,
    tokens: Tokens,
    semantic: toolkit::tokens::Semantic,
    text_scale: f32,
    reduced_motion: bool,
    buttons: BTreeMap<design::ButtonCellKey, design::ReadButton>,
    density: f32,
}

#[derive(Clone, Debug)]
pub struct Prepared {
    projection: Projection,
    typography: Typography,
    choices: BTreeMap<String, FontChoice>,
    /// The immutable resource receipt of a host preparation. [`bootstrap`]
    /// keeps this `None`: no I/O or registration happens before connection.
    #[cfg(feature = "resources")]
    resources: Option<crate::resources::PreparedResources>,
}

/// Immediate generic appearance until the host activates checked resources.
/// This performs no font discovery, installation or I/O and claims no settings
/// identity, cache receipt or acknowledgement. Hosts label it Bootstrap.
pub fn bootstrap() -> Result<Prepared, Diagnostic> {
    let effective = settings::resolve(&settings::Desktop::default())
        .map_err(|errors| errors.into_iter().next().expect("resolution diagnostic"))?;
    let projection = Projection::new(&effective["desktop"])?;
    projection.prepare(|_, record| {
        Ok(FontSelection {
            font: iced_core::Font {
                family: match record.generic {
                    TypographyGeneric::SansSerif => iced_core::font::Family::SansSerif,
                    TypographyGeneric::Monospace => iced_core::font::Family::Monospace,
                },
                weight: toolkit::fonts::weight(record.weight),
                ..iced_core::Font::DEFAULT
            },
            choice: FontChoice::Generic,
        })
    })
}

fn fault(path: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic::new("unsupported_presentation", path, message)
}
fn bounded(value: f64, min: f64, max: f64, path: &str) -> Result<f32, Diagnostic> {
    if !value.is_finite() || !(min..=max).contains(&value) {
        return Err(fault(path, format!("Expected finite {min}..{max}")));
    }
    Ok(value as f32)
}
fn rgba(value: [f64; 4], path: &str) -> Result<LinearRgba, Diagnostic> {
    for channel in value {
        bounded(channel, 0.0, 1.0, path)?;
    }
    let [red, green, blue, alpha] = value;
    Ok(LinearRgba {
        red,
        green,
        blue,
        alpha,
    })
}

impl Projection {
    pub fn new(effective: &Effective) -> Result<Self, Diagnostic> {
        let d = &effective.design;
        if d.schema != 1 {
            return Err(fault("design.schema", "Unsupported design read schema"));
        }
        if d.typography.len() > 256
            || d.metrics.len() > 1024
            || d.scales.len() > 64
            || d.buttons.len() != design::BUTTON_CELL_COUNT
        {
            return Err(fault("design", "Unsupported projection collection sizes"));
        }
        if design::Scheme::from_name(&effective.scheme).is_none()
            || design::Mode::from_name(&effective.mode).is_none()
            || !["normal", "high"].contains(&effective.contrast.as_str())
        {
            return Err(fault("appearance", "Unsupported appearance selection"));
        }
        let density = bounded(effective.ui.density, 0.5, 2.0, "ui.density")?;
        let text_scale = bounded(effective.ui.text_scale, 0.5, 3.0, "ui.text_scale")?;
        let mut colours = ResolvedColours::default();
        for (name, value) in &d.primitives {
            colours.primitives.insert(name.clone(), rgba(*value, name)?);
        }
        for (name, p) in &d.pairs {
            bounded(p.contrast_ratio, 1.0, 21.0, name)?;
            colours.pairs.insert(
                name.clone(),
                ResolvedPair {
                    surface_name: String::new(),
                    foreground_name: String::new(),
                    backdrop_name: None,
                    recipe: None,
                    surface: rgba(p.surface, name)?,
                    foreground: rgba(p.foreground, name)?,
                    backdrop: p.backdrop.map(|v| rgba(v, name)).transpose()?,
                    rendered_surface: rgba(p.rendered_surface, name)?,
                    rendered_foreground: rgba(p.rendered_foreground, name)?,
                    contrast_ratio: p.contrast_ratio,
                },
            );
        }
        for (name, value) in &d.non_text {
            colours.non_text.insert(
                name.clone(),
                ResolvedNonTextColour {
                    value_name: String::new(),
                    value: rgba(*value, name)?,
                    adjacent: Default::default(),
                },
            );
        }
        // Require every palette input instead of silently filling a partial
        // projection with unrelated toolkit defaults.
        crate::conversion::from_colours(&colours)
            .map_err(|e| fault("design.pairs", e.to_string()))?;
        let mut dictionary = ResolvedDictionary {
            colours,
            metrics: BTreeMap::new(),
            scales: BTreeMap::new(),
        };
        for (name, m) in &d.metrics {
            let kind = match m.kind.as_str() {
                "px" => ResolvedMetricKind::Px,
                "ratio" => ResolvedMetricKind::Ratio,
                _ => return Err(fault(name, "Unsupported metric unit")),
            };
            bounded(m.value, 0.0, 4096.0, name)?;
            dictionary.metrics.insert(
                name.clone(),
                ResolvedMetric {
                    kind,
                    value: m.value,
                },
            );
        }
        for name in ["radius", "button.border_width", "type.compact"] {
            if !dictionary
                .metrics
                .get(name)
                .is_some_and(|m| m.kind == ResolvedMetricKind::Px)
            {
                return Err(fault(name, "Required logical-pixel metric is missing"));
            }
        }
        bounded(
            dictionary.metrics["type.compact"].value * f64::from(text_scale),
            0.01,
            4096.0,
            "type.compact",
        )?;
        for (name, scale) in &d.scales {
            if scale.is_empty() || scale.len() > 4096 {
                return Err(fault(name, "Invalid scale length"));
            }
            for value in scale {
                bounded(*value, 0.0, 4096.0, name)?;
            }
            dictionary.scales.insert(name.clone(), scale.clone());
        }
        if dictionary
            .scales
            .get("spacing")
            .is_none_or(|s| s.len() <= 9)
        {
            return Err(fault("spacing", "Required spacing steps are missing"));
        }
        let mut types = BTreeMap::new();
        for (name, t) in &d.typography {
            if t.family.trim().is_empty()
                || t.family.len() > 256
                || t.fallbacks.len() > 16
                || t.fallbacks
                    .iter()
                    .any(|s| s.trim().is_empty() || s.len() > 256)
                || !(1..=1000).contains(&t.weight)
            {
                return Err(fault(name, "Invalid font family chain or weight"));
            }
            let generic = match t.generic.as_str() {
                "sans_serif" => TypographyGeneric::SansSerif,
                "monospace" => TypographyGeneric::Monospace,
                _ => return Err(fault(name, "Unsupported generic font")),
            };
            bounded(t.font_size * f64::from(text_scale), 0.01, 4096.0, name)?;
            if let Some(height) = t.line_height {
                bounded(height * f64::from(text_scale), 0.01, 4096.0, name)?;
            }
            types.insert(
                name.clone(),
                ResolvedTypeRecord {
                    family: t.family.clone(),
                    fallbacks: t.fallbacks.clone(),
                    generic,
                    font_size_metric: String::new(),
                    font_size: t.font_size,
                    weight: t.weight,
                    line_height: t.line_height,
                },
            );
        }
        for role in [
            TypographyRole::Ui,
            TypographyRole::UiDisplay,
            TypographyRole::Small,
            TypographyRole::Mono,
            TypographyRole::Terminal,
        ] {
            if !types.contains_key(role.name()) {
                return Err(fault(role.name(), "Required typography role is missing"));
            }
        }
        let mut buttons = BTreeMap::new();
        for cell in &d.buttons {
            let key = design::ButtonCellKey {
                variant: design::ButtonVariant::ALL
                    .into_iter()
                    .find(|variant| variant.name() == cell.variant)
                    .ok_or_else(|| fault("button.variant", "Unsupported button variant"))?,
                size: design::ButtonSize::ALL
                    .into_iter()
                    .find(|size| size.name() == cell.size)
                    .ok_or_else(|| fault("button.size", "Unsupported button size"))?,
                interaction: design::InteractionState::ALL
                    .into_iter()
                    .find(|interaction| interaction.name() == cell.interaction)
                    .ok_or_else(|| fault("button.interaction", "Unsupported button interaction"))?,
                focus_visible: cell.focus_visible,
            };
            if buttons.insert(key, cell.clone()).is_some() {
                return Err(fault("button", "Invalid or duplicate compiled button cell"));
            }
            for part in design::ButtonPart::ALL {
                if !cell.typography.contains_key(part.name()) {
                    return Err(fault(
                        "button.typography",
                        "Required button part is missing",
                    ));
                }
            }
            for name in cell.typography.values() {
                if !types.contains_key(name) {
                    return Err(fault(name, "Button refers to missing typography"));
                }
            }
            for value in [
                cell.height,
                cell.min_width,
                cell.padding_x,
                cell.border_width,
                cell.radius,
            ] {
                bounded(value, 0.0, 4096.0, "button")?;
            }
            rgba(cell.pair.surface, "button")?;
            rgba(cell.pair.foreground, "button")?;
            rgba(cell.pair.rendered_surface, "button")?;
            rgba(cell.pair.rendered_foreground, "button")?;
            if let Some(value) = cell.pair.backdrop {
                rgba(value, "button.backdrop")?;
            }
            bounded(cell.pair.contrast_ratio, 1.0, 21.0, "button.contrast_ratio")?;
            if let Some(value) = cell.border {
                rgba(value, "button.border")?;
            }
            if let Some(value) = cell.ring {
                rgba(value, "button.ring")?;
            }
        }
        let label = d
            .buttons
            .iter()
            .find(|c| {
                c.variant == "default"
                    && c.size == "md"
                    && c.interaction == "resting"
                    && !c.focus_visible
            })
            .and_then(|c| c.typography.get("label"))
            .and_then(|name| types.get(name))
            .ok_or_else(|| {
                fault(
                    "button.default.md.label",
                    "Required button typography is missing",
                )
            })?;
        let mut metrics = crate::tokens::metrics_from_records(
            &dictionary,
            &types["ui"],
            &types["small"],
            label.weight,
        );
        metrics.spacing.xs *= density;
        metrics.spacing.sm *= density;
        metrics.spacing.md *= density;
        metrics.spacing.lg *= density;
        metrics.spacing.xl *= density;
        metrics.text.xs *= text_scale;
        metrics.text.sm *= text_scale;
        metrics.text.md *= text_scale;
        metrics.text.lg *= text_scale;
        metrics.text.xl *= text_scale;
        metrics.text.xxl *= text_scale;
        for value in [
            metrics.text.xs,
            metrics.text.sm,
            metrics.text.md,
            metrics.text.lg,
            metrics.text.xl,
            metrics.text.xxl,
        ] {
            bounded(f64::from(value), 0.01, 4096.0, "text scale")?;
        }
        let tokens = Tokens::new(crate::tokens::palette(&dictionary.colours), metrics);
        let semantic = crate::tokens::semantic_colours(&dictionary.colours);
        Ok(Self {
            dictionary,
            types,
            tokens,
            semantic,
            text_scale,
            reduced_motion: effective.ui.reduced_motion,
            buttons,
            density,
        })
    }
    pub fn dictionary(&self) -> &ResolvedDictionary {
        &self.dictionary
    }
    pub fn tokens(&self) -> Tokens {
        self.tokens
    }
    pub fn reduced_motion(&self) -> bool {
        self.reduced_motion
    }

    /// Resolve every referenced type record before returning an activatable
    /// presentation, including records used by compiled button assignments.
    /// Hosts may supply a renderer resource validator; this callback must not
    /// return a successful font until its resources are actually usable.
    pub fn prepare(
        self,
        mut resolve: impl FnMut(&str, &ResolvedTypeRecord) -> Result<FontSelection, Diagnostic>,
    ) -> Result<Prepared, Diagnostic> {
        let mut records = BTreeMap::new();
        let mut choices = BTreeMap::new();
        for (name, t) in &self.types {
            let selected = resolve(name, t)?;
            records.insert(
                name.clone(),
                TextStyle {
                    font: selected.font,
                    size: t.font_size as f32 * self.text_scale,
                    line_height: t.line_height.map(|v| v as f32 * self.text_scale),
                },
            );
            choices.insert(name.clone(), selected.choice);
        }
        let typography = Typography::new(records).map_err(|e| fault("typography", e))?;
        Ok(Prepared {
            projection: self,
            typography,
            choices,
            #[cfg(feature = "resources")]
            resources: None,
        })
    }
    /// Check the process's already registered fonts. An implicit package source
    /// may use its installed role or an explicitly reported generic rescue;
    /// custom sources must find a family from their declared chain.
    pub fn prepare_registered(self, package_source: bool) -> Result<Prepared, Diagnostic> {
        self.prepare_registered_checked(package_source, || Ok(()))
    }
    /// Host cancellation is checked between individual registered font probes.
    pub fn prepare_registered_checked(
        self,
        package_source: bool,
        mut check: impl FnMut() -> Result<(), Diagnostic>,
    ) -> Result<Prepared, Diagnostic> {
        self.prepare(|name, t| {
            check()?;
            let default = match name {
                "ui" => Some(TypographyRole::Ui),
                "ui_display" => Some(TypographyRole::UiDisplay),
                "small" => Some(TypographyRole::Small),
                "mono" => Some(TypographyRole::Mono),
                "terminal" => Some(TypographyRole::Terminal),
                _ => None,
            };
            let builtin = package_source
                && default.is_some_and(|role| {
                    let r = design::default_typography(role);
                    r.family == t.family && r.fallbacks == t.fallbacks && r.generic == t.generic
                });
            let role = if name == "ui_display" {
                Role::Display
            } else if t.generic == TypographyGeneric::Monospace {
                Role::Mono
            } else {
                Role::Sans
            };
            toolkit::fonts::try_font_for(
                &t.family,
                &t.fallbacks,
                t.weight,
                t.generic == TypographyGeneric::Monospace,
                builtin.then_some(role),
                package_source,
            )
            .map_err(|e| fault(&format!("typography.{name}"), e))
        })
    }
}
#[cfg(feature = "resources")]
impl Projection {
    /// Every resolved type record, including records referenced by compiled
    /// button assignments. Crate-private: resource preparation resolves all
    /// of them, but public callers must not treat this as preparation.
    pub(crate) fn type_records(&self) -> &BTreeMap<String, ResolvedTypeRecord> {
        &self.types
    }

    /// Attach a verified resource receipt to a fully resolved projection.
    /// This delegates typography construction to [`Projection::prepare`] and
    /// attaches the immutable receipt; callers cannot forge a successful
    /// receipt because the receipt only exists after a verified read and one
    /// atomic toolkit batch.
    pub(crate) fn prepare_with_resources(
        self,
        resources: crate::resources::PreparedResources,
    ) -> Result<Prepared, Diagnostic> {
        let mut prepared = self.prepare(|name, _| {
            resources.text_selection(name).ok_or_else(|| {
                fault(
                    &format!("typography.{name}"),
                    "verified batch resolved no text record",
                )
            })
        })?;
        prepared.resources = Some(resources);
        Ok(prepared)
    }
}
impl Prepared {
    /// A validated compiler read cell. This does not recreate accepted design
    /// ownership. Every closed-axis key is present after projection validation.
    pub fn button(&self, key: design::ButtonCellKey) -> &design::ReadButton {
        &self.projection.buttons[&key]
    }
    pub fn button_text(&self, key: design::ButtonCellKey, part: design::ButtonPart) -> TextStyle {
        self.typography
            .get(&self.button(key).typography[part.name()])
            .expect("validated button typography was prepared")
    }
    /// The prepared `ui` role. Projection validation requires the role, so
    /// this cannot fail.
    pub fn ui_text(&self) -> TextStyle {
        self.typography
            .get("ui")
            .expect("validated ui typography was prepared")
    }
    /// The prepared `small` role. Projection validation requires the role,
    /// so this cannot fail.
    pub fn small_text(&self) -> TextStyle {
        self.typography
            .get("small")
            .expect("validated small typography was prepared")
    }
    pub fn density(&self) -> f32 {
        self.projection.density
    }
    pub fn tokens(&self) -> Tokens {
        self.projection.tokens
    }
    pub fn theme(&self) -> toolkit::Theme {
        toolkit::Theme::new(self.tokens()).with_semantic(self.projection.semantic)
    }
    pub fn typography(&self) -> &Typography {
        &self.typography
    }
    pub fn dictionary(&self) -> &ResolvedDictionary {
        self.projection.dictionary()
    }
    pub fn font_choices(&self) -> &BTreeMap<String, FontChoice> {
        &self.choices
    }
    pub fn reduced_motion(&self) -> bool {
        self.projection.reduced_motion
    }
    /// The immutable resource receipt attached by a verified host preparation:
    /// the exact binding, the ready icons and the selection evidence.
    /// [`bootstrap`] prepared presentations return `None`.
    #[cfg(feature = "resources")]
    pub fn resources(&self) -> Option<&crate::resources::PreparedResources> {
        self.resources.as_ref()
    }
    /// Attach the honest receipt of a preparation that resolved its text
    /// records directly (the no-set generic rescue); crate-private, used by
    /// the resource host only.
    #[cfg(feature = "resources")]
    pub(crate) fn attach_resources(&mut self, resources: crate::resources::PreparedResources) {
        self.resources = Some(resources);
    }
}

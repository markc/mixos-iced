// SPDX-License-Identifier: MIT OR Apache-2.0
//! Pure requirements and adoption of the settings worker's immutable receipt.
use super::{ALL, Icon, Icons, tint_key};
use crate::app::PreparationContext;
use appearance::resources::{IconRequirement, ResourceRequirements};
use settings::Diagnostic;

pub(super) fn key(icon: Icon, tint: &str) -> String {
    format!("{}:{tint}", icon.material_name())
}

pub fn requirements(
    projection: &appearance::settings::Projection,
    snapshot: &settings::Snapshot,
    context: &PreparationContext,
) -> Result<ResourceRequirements, Diagnostic> {
    let effective = snapshot.effective.get("app:dopus").ok_or_else(|| {
        Diagnostic::new(
            "unsupported_content",
            "effective.app:dopus",
            "missing Dopus projection",
        )
    })?;
    let chrome = crate::theme::build_chrome(projection.dictionary())
        .map_err(|name| {
            Diagnostic::new(
                "unsupported_content",
                &name,
                "Chrome colour or metric missing",
            )
        })?
        .scaled(effective.ui.density as f32);
    let palette = projection.tokens().palette;
    let mut icons = Vec::new();
    for tint in [palette.text, palette.muted_text, palette.selection_text] {
        for icon in ALL {
            let requirement = IconRequirement {
                key: key(icon, &tint_key(tint)),
                name: icon.material_name().into(),
                logical_size: chrome.icon,
                scale: context.scale(),
                tint,
            };
            // Equal colours share one prepared variant and one host charge.
            if !icons
                .iter()
                .any(|existing: &IconRequirement| existing.key == requirement.key)
            {
                icons.push(requirement);
            }
        }
    }
    let sources = ALL
        .into_iter()
        .map(|icon| {
            appearance::resources::EmbeddedSvg::trusted_static(icon.material_name(), icon.bytes())
        })
        .collect::<Result<Vec<_>, _>>()?;
    ResourceRequirements::new(icons)?.with_embedded_svg_fallbacks(sources)
}

impl Icons {
    pub fn from_prepared(
        look: &appearance::settings::Prepared,
        colours: &[application::iced::Color],
    ) -> Result<Self, Diagnostic> {
        let tints: Vec<_> = colours.iter().copied().map(tint_key).collect();
        let colour_map =
            std::sync::Arc::new(tints.iter().cloned().zip(colours.iter().copied()).collect());
        if let Some(resources) = look.resources() {
            let mut glyphs = std::collections::HashMap::new();
            for tint in &tints {
                for icon in ALL {
                    if let Some(ready) = resources.icon(&key(icon, tint)) {
                        if let toolkit::icons::Ready::Text(text) = ready {
                            let glyph = text.glyph().ok_or_else(|| {
                                Diagnostic::new(
                                    "unsupported_content",
                                    &format!("icons.{}", icon.material_name()),
                                    "prepared glyph is missing",
                                )
                            })?;
                            glyphs.insert(icon, glyph);
                        }
                    } else {
                        return Err(Diagnostic::new(
                            "unsupported_content",
                            &format!("icons.{}", icon.material_name()),
                            "prepared icon is missing",
                        ));
                    }
                }
            }
            let mut icons = Self::lucide();
            icons.colours = colour_map;
            icons.asset_set = resources.binding().map(|binding| binding.set_id.clone());
            if !glyphs.is_empty() {
                icons.material = Some(std::sync::Arc::new(glyphs));
            }
            icons.pinned = Some(resources.clone());
            return Ok(icons);
        }
        // Standalone/bootstrap callers have no host receipt and draw no
        // icons. Live preparations always adopt the host's complete receipt.
        let mut icons = Self::lucide();
        icons.colours = colour_map;
        Ok(icons)
    }

    pub fn ready(&self, icon: Icon, tint: &str) -> Option<toolkit::icons::Ready> {
        self.pinned.as_ref()?.icon(&key(icon, tint)).cloned()
    }
}

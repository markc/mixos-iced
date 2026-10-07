// SPDX-License-Identifier: MIT OR Apache-2.0
//! One render-domain comparison for all native consumers. Authority identity,
//! source names, revisions and provenance are evidence, not render inputs.
use crate::{CommonUi, Effective, Snapshot};
use design::{DesignReadProjection, ReadButton, ReadPair, ReadType};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChangePlan {
    pub paint: bool,
    pub text: bool,
    pub layout: bool,
    pub resources: bool,
    pub motion: bool,
    pub shell: bool,
}
impl ChangePlan {
    pub fn is_empty(self) -> bool {
        self == Self::default()
    }
    /// Resource preparation completes before activating any of these domains.
    pub fn between(
        previous: Option<&Snapshot>,
        next: &Snapshot,
        context: &str,
        shell: bool,
    ) -> Self {
        let old = previous.and_then(|s| s.effective.get(context));
        let new = next.effective.get(context);
        let mut plan = match (old, new) {
            (Some(old), Some(new)) => compare(old, new),
            _ => Self {
                paint: true,
                text: true,
                layout: true,
                resources: true,
                motion: true,
                shell: false,
            },
        };
        plan.shell = shell && previous.is_none_or(|s| s.desktop.shell != next.desktop.shell);
        plan.layout |= plan.shell;
        plan
    }
}
fn pair_key(pair: &ReadPair) -> impl PartialEq + '_ {
    let ReadPair {
        surface,
        foreground,
        rendered_surface,
        rendered_foreground,
        backdrop,
        contrast_ratio: _,
    } = pair;
    (
        surface,
        foreground,
        rendered_surface,
        rendered_foreground,
        backdrop,
    )
}
fn button_key(button: &ReadButton) -> (&str, &str, &str, bool) {
    // The remaining fields are mapped by paint/text/layout below. Exhaustive
    // destructuring forces every added DTO field to be classified deliberately.
    let ReadButton {
        variant,
        size,
        interaction,
        focus_visible,
        pair: _,
        border: _,
        ring: _,
        height: _,
        min_width: _,
        padding_x: _,
        border_width: _,
        radius: _,
        typography: _,
    } = button;
    (variant, size, interaction, *focus_visible)
}
fn font_key(record: &ReadType) -> impl PartialEq + '_ {
    let ReadType {
        family,
        fallbacks,
        generic,
        weight,
        font_size: _,
        line_height: _,
    } = record;
    (family, fallbacks, generic, weight)
}
fn view(effective: &Effective) -> &DesignReadProjection {
    let Effective {
        scheme: _,
        mode: _,
        contrast: _,
        ui,
        design,
        provenance: _,
    } = effective;
    let CommonUi {
        density: _,
        text_scale: _,
        reduced_motion: _,
    } = ui;
    let DesignReadProjection {
        schema: _,
        source: _,
        primitives: _,
        pairs: _,
        non_text: _,
        metrics: _,
        scales: _,
        typography: _,
        buttons: _,
    } = design;
    // Source/provenance/schema identify and validate evidence; all remaining
    // fields feed the domains below. No wildcard permits an unnoticed addition.
    design
}
fn compare(old: &Effective, new: &Effective) -> ChangePlan {
    let a = view(old);
    let b = view(new);
    let resources = !a
        .typography
        .iter()
        .map(|(k, r)| (k, font_key(r)))
        .eq(b.typography.iter().map(|(k, r)| (k, font_key(r))));
    let text = resources
        || old.ui.text_scale != new.ui.text_scale
        || !a
            .typography
            .iter()
            .map(|(k, r)| (k, r.font_size, r.line_height))
            .eq(b
                .typography
                .iter()
                .map(|(k, r)| (k, r.font_size, r.line_height)))
        || !a
            .buttons
            .iter()
            .map(|c| (button_key(c), &c.typography))
            .eq(b.buttons.iter().map(|c| (button_key(c), &c.typography)));
    let paint = old.scheme != new.scheme
        || old.mode != new.mode
        || old.contrast != new.contrast
        || a.primitives != b.primitives
        || a.non_text != b.non_text
        || !a
            .pairs
            .iter()
            .map(|(k, p)| (k, pair_key(p)))
            .eq(b.pairs.iter().map(|(k, p)| (k, pair_key(p))))
        || !a
            .buttons
            .iter()
            .map(|c| (button_key(c), pair_key(&c.pair), c.border, c.ring))
            .eq(b
                .buttons
                .iter()
                .map(|c| (button_key(c), pair_key(&c.pair), c.border, c.ring)));
    let layout = text
        || old.ui.density != new.ui.density
        || a.metrics != b.metrics
        || a.scales != b.scales
        || !a
            .buttons
            .iter()
            .map(|c| {
                (
                    button_key(c),
                    c.height,
                    c.min_width,
                    c.padding_x,
                    c.border_width,
                    c.radius,
                )
            })
            .eq(b.buttons.iter().map(|c| {
                (
                    button_key(c),
                    c.height,
                    c.min_width,
                    c.padding_x,
                    c.border_width,
                    c.radius,
                )
            }));
    ChangePlan {
        paint,
        text,
        layout,
        resources,
        motion: old.ui.reduced_motion != new.ui.reduced_motion,
        shell: false,
    }
}

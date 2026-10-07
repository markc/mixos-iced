// SPDX-License-Identifier: MIT OR Apache-2.0
//! One render-domain comparison for all native consumers. Authority identity,
//! source names, revisions and provenance are evidence, not render inputs.
use crate::{Effective, Snapshot};
use design::{ReadButton, ReadPair};

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
    pub fn between(previous: Option<&Snapshot>, next: &Snapshot, context: &str, shell: bool) -> Self {
        let old = previous.and_then(|s| s.effective.get(context));
        let new = next.effective.get(context);
        let mut plan = match (old, new) {
            (Some(old), Some(new)) => compare(old, new),
            _ => Self { paint:true, text:true, layout:true, resources:true, motion:true, shell:false },
        };
        plan.shell = shell && previous.is_none_or(|s| s.desktop.shell != next.desktop.shell);
        plan.layout |= plan.shell;
        plan
    }
}
fn pair_key(pair: &ReadPair) -> impl PartialEq + '_ {
    (pair.surface, pair.foreground, pair.rendered_surface, pair.rendered_foreground, pair.backdrop)
}
fn button_key(button: &ReadButton) -> (&str, &str, &str, bool) {
    (&button.variant, &button.size, &button.interaction, button.focus_visible)
}
fn compare(old: &Effective, new: &Effective) -> ChangePlan {
    let a = &old.design;
    let b = &new.design;
    let resources = !a.typography.iter().map(|(k,r)| (k,&r.family,&r.fallbacks,&r.generic,r.weight))
        .eq(b.typography.iter().map(|(k,r)| (k,&r.family,&r.fallbacks,&r.generic,r.weight)));
    let text = resources || old.ui.text_scale != new.ui.text_scale
        || !a.typography.iter().map(|(k,r)| (k,r.font_size,r.line_height))
            .eq(b.typography.iter().map(|(k,r)| (k,r.font_size,r.line_height)))
        || !a.buttons.iter().map(|c| (button_key(c),&c.typography))
            .eq(b.buttons.iter().map(|c| (button_key(c),&c.typography)));
    let paint = old.scheme != new.scheme || old.mode != new.mode || old.contrast != new.contrast
        || a.primitives != b.primitives || a.non_text != b.non_text
        || !a.pairs.iter().map(|(k,p)| (k,pair_key(p))).eq(b.pairs.iter().map(|(k,p)| (k,pair_key(p))))
        || !a.buttons.iter().map(|c| (button_key(c),pair_key(&c.pair),c.border,c.ring))
            .eq(b.buttons.iter().map(|c| (button_key(c),pair_key(&c.pair),c.border,c.ring)));
    let layout = text || old.ui.density != new.ui.density || a.metrics != b.metrics || a.scales != b.scales
        || !a.buttons.iter().map(|c| (button_key(c),c.height,c.min_width,c.padding_x,c.border_width,c.radius))
            .eq(b.buttons.iter().map(|c| (button_key(c),c.height,c.min_width,c.padding_x,c.border_width,c.radius)));
    ChangePlan { paint, text, layout, resources, motion:old.ui.reduced_motion != new.ui.reduced_motion, shell:false }
}

// SPDX-License-Identifier: MIT OR Apache-2.0
//! The tab strip (ced E1 plan §4.4): one tab per buffer — name, `●` dirty,
//! `!` disk modified or deleted, `◆` agent edits since the tab was last
//! focused, `⟳` while (re)attaching. Click selects, middle-click or the `×`
//! closes, the strip scrolls sideways on overflow.

use application::iced::widget::{container, row};
use application::Element;
use application::iced::{Alignment, Length};
use edit::wire::DiskState;
use editor_model::mirror::{DetachReason, Phase};
use editor_model::types::TabId;

use super::{Look, TABS_H};
use crate::app::Msg;
use crate::controller::Tab;

/// What a tab shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabBadge {
    pub name: String,
    pub dirty: bool,
    pub disk_alert: bool,
    pub agent_edits: bool,
    pub attaching: bool,
    pub detached: bool,
    /// Tooltip / title: the full path, or the scratch name.
    pub title: String,
}

/// The display name of a tab: the file name, the buffer's own name, or the
/// path the user asked for while it is still opening.
pub fn display_name(tab: &Tab) -> String {
    let meta = tab.mirror.as_ref().map(|m| m.meta());
    let path = meta
        .and_then(|m| m.path.clone())
        .or_else(|| tab.path.clone());
    if let Some(path) = path {
        return std::path::Path::new(&path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or(path);
    }
    meta.and_then(|m| m.name.clone())
        .unwrap_or_else(|| "untitled".to_owned())
}

pub fn badge(tab: &Tab, agent_edits: bool) -> TabBadge {
    let name = display_name(tab);
    let Some(mirror) = tab.mirror.as_ref() else {
        return TabBadge {
            title: tab.path.clone().unwrap_or_else(|| name.clone()),
            name,
            dirty: false,
            disk_alert: false,
            agent_edits: false,
            attaching: true,
            detached: false,
        };
    };
    let meta = mirror.meta();
    let (attaching, detached) = match mirror.phase() {
        Phase::Bootstrapping { .. } | Phase::Recovering { .. } => (true, false),
        Phase::Detached {
            reason: DetachReason::EpochChanged,
        } => (true, false),
        Phase::Detached { .. } => (false, true),
        Phase::Live => (false, false),
    };
    TabBadge {
        title: meta.path.clone().unwrap_or_else(|| name.clone()),
        name,
        dirty: meta.dirty,
        disk_alert: matches!(meta.disk, DiskState::Modified | DiskState::Deleted),
        agent_edits,
        attaching,
        detached,
    }
}

impl TabBadge {
    /// The strip label: marks, then the name.
    pub fn label(&self) -> String {
        let mut s = String::new();
        if self.attaching {
            s.push_str("⟳ ");
        }
        if self.dirty {
            s.push_str("● ");
        }
        s.push_str(&self.name);
        if self.disk_alert {
            s.push_str(" !");
        }
        if self.agent_edits {
            s.push_str(" ◆");
        }
        s
    }
}

/// The strip. `agent_edits(id)` says whether an agent edited that tab since
/// it was last focused.
pub fn view<'a>(
    look: Look,
    tabs: &'a [Tab],
    active: Option<TabId>,
    agent_edits: impl Fn(TabId) -> bool,
) -> Element<'a, Msg> {
    let tokens = look.tokens;
    let badges: Vec<_> = tabs
        .iter()
        .map(|tab| (tab.id, badge(tab, agent_edits(tab.id))))
        .collect();
    let detached: Vec<_> = badges
        .iter()
        .filter(|(_, badge)| badge.detached)
        .map(|(id, _)| *id)
        .collect();
    let mut strip = toolkit::TabBar::with_tab_labels(
        badges
            .into_iter()
            .map(|(id, badge)| (id, toolkit::TabLabel::Text(badge.label())))
            .collect(),
        Msg::SelectTab,
    )
    .on_close(Msg::CloseTab)
    .text_font(look.ui)
    .text_size(look.ui_px)
    .close_size(look.ui_px)
    .tab_width(Length::Shrink)
    .width(Length::Shrink)
    .height(Length::Fixed(TABS_H))
    .padding([5.0, 12.0])
    .spacing(1.0)
    .text_colour(move |id| detached.contains(id).then_some(tokens.palette.muted_text))
    .style(move |_, status| {
        let mut style = toolkit::theme::tab_bar::default(&toolkit::Theme::new(tokens), status);
        if status == toolkit::tab_bar::Status::Active {
            style.tab_label_border_color = look.chrome.accent;
        } else if status == toolkit::tab_bar::Status::Disabled {
            style.tab_label_background = look.chrome.secondary.into();
            style.text_color = look.chrome.secondary_text;
        }
        style
    });
    if let Some(active) = active {
        strip = strip.set_active_tab(&active);
    }
    let new_tab = toolkit::CenteredButton::new(look.text("+").color(look.chrome.secondary_text))
        .padding([4.0, 10.0])
        .style(look.flat())
        .on_press(Msg::Action(crate::actions::ActionId::FileNew));
    container(row![strip.scrollable().width(Length::Fill), new_tab].align_y(Alignment::End))
        .width(Length::Fill)
        .height(Length::Fixed(TABS_H))
        .style(look.strip(look.chrome.secondary, look.chrome.secondary_text))
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_carry_every_mark() {
        let b = TabBadge {
            name: "scene.mix".into(),
            dirty: true,
            disk_alert: true,
            agent_edits: true,
            attaching: false,
            detached: false,
            title: "/x/scene.mix".into(),
        };
        assert_eq!(b.label(), "● scene.mix ! ◆");
        let b = TabBadge {
            dirty: false,
            disk_alert: false,
            agent_edits: false,
            attaching: true,
            ..b
        };
        assert_eq!(b.label(), "⟳ scene.mix");
    }
}

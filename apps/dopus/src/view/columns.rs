// SPDX-License-Identifier: MIT OR Apache-2.0
//! Sort policy and action labels for the shared responsive file header.
use super::{
    Look,
    rows::{Columns, presentation},
};
use crate::app::{Msg, PaneOp};
use application::Element;
use dopus_core::{PaneId, SortColumn};

pub struct Header;
impl Header {
    pub fn view(
        look: Look,
        pane: PaneId,
        sort: SortColumn,
        ascending: bool,
        actions: &[crate::verbs::ActionRow],
        columns: Columns,
    ) -> Element<'static, Msg> {
        use actions::filemgr;
        let sorts = [SortColumn::Name, SortColumn::Size, SortColumn::Modified];
        let selected = sorts.iter().position(|column| *column == sort).unwrap_or(0);
        toolkit::file_pane::Header::new(
            presentation(look),
            columns,
            ["Name".into(), "Size".into(), "Modified".into()],
            selected,
            ascending,
            move |column| Msg::Pane(pane, PaneOp::Sort(sorts[column])),
        )
        .colours(look.chrome.secondary, look.chrome.secondary_text)
        .tooltip(
            [
                (filemgr::VIEW_SORT_NAME, "Sort by name"),
                (filemgr::VIEW_SORT_SIZE, "Sort by size"),
                (filemgr::VIEW_SORT_MODIFIED, "Sort by modified time"),
            ]
            .map(|(id, label)| super::tips::action_label(actions, id, label)),
            move |label, size| {
                super::tips::tip(
                    look,
                    application::iced::widget::Space::new()
                        .width(size.width)
                        .height(size.height),
                    label,
                )
            },
        )
        .into()
    }
}

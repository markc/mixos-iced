// SPDX-License-Identifier: MIT OR Apache-2.0
//! Plain Properties sidebar: all data comes from a non-blocking core snapshot.
use super::{Look, elide::Label};
use crate::app::Msg;
use application::Element;
use application::iced::Length;
use application::iced::widget::{column, container, scrollable};
use dopus_core::{format_modified_at, format_size, properties::Properties};

pub fn sidebar<'a>(
    look: Look,
    first_row: super::FirstRow,
    properties: Properties,
) -> Element<'a, Msg> {
    let mut content = column![]
        .spacing(look.chrome.gap)
        .padding(application::iced::Padding {
            top: first_row.properties_top,
            right: look.chrome.pad,
            bottom: look.chrome.small,
            left: look.chrome.pad,
        })
        .width(Length::Fill);
    let mut fields = Vec::new();
    let title = match properties {
        Properties::Folder { path, summary } => {
            fields.push(("Contents", summary));
            path
        }
        Properties::Entry {
            entry,
            count_pending,
            metadata,
        } => {
            let size = if entry.is_dir {
                entry
                    .child_count
                    .map(|n| format!("{n} {}", if n == 1 { "item" } else { "items" }))
                    .unwrap_or_else(|| if count_pending { "…" } else { "Unavailable" }.into())
            } else {
                entry
                    .size
                    .map(|n| format!("{n} bytes ({})", format_size(n)))
                    .unwrap_or_else(|| "…".into())
            };
            fields.push(("Size", size));
            match metadata {
                None => fields.push(("Details", "…".into())),
                Some(Err(error)) => fields.push(("Details", format!("Unavailable: {error}"))),
                Some(Ok(meta)) => {
                    if !entry.is_dir {
                        fields[0].1 = format!("{} bytes ({})", meta.size, format_size(meta.size));
                    }
                    fields.insert(0, ("Kind", meta.kind));
                    for (label, time) in [
                        ("Modified", meta.modified),
                        ("Created", meta.created),
                        ("Accessed", meta.accessed),
                    ] {
                        fields.push((
                            label,
                            time.map(format_modified_at).unwrap_or_else(|| "—".into()),
                        ));
                    }
                    fields.push(("Permissions", meta.permissions));
                    fields.push(("Owner:group", meta.owner_group));
                    if let Some(target) = meta.symlink_target {
                        fields.push(("Link target", target));
                    }
                }
            }
            fields.push(("Path", dopus_core::sanitise_display_path(&entry.path)));
            entry.name
        }
    };
    content = content.push(Label {
        text: title,
        font: look.ui_font,
        px: look.sidebar_px(),
        line_height: look.sidebar_line_height(),
        color: look.tokens.palette.text,
    });
    for (label, value) in fields {
        content = content.push(
            column![
                look.ui_text(look.sidebar_px() / look.px).text(label)
                    .color(look.tokens.palette.muted_text),
                look.ui_text(look.sidebar_px() / look.px).text(value)
                    .wrapping(application::iced::advanced::text::Wrapping::WordOrGlyph)
                    .shaping(application::iced::advanced::text::Shaping::Advanced)
                    .color(look.tokens.palette.text)
                    .width(Length::Fill),
            ]
            .spacing(look.chrome.small),
        );
    }
    container(scrollable(content))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(look.strip(look.chrome.secondary, look.chrome.secondary_text))
        .into()
}

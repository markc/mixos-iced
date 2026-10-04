// SPDX-License-Identifier: MIT OR Apache-2.0
//! The "Lists & trees" page: a 100,000-row `VirtualList` with a column
//! header, selection, activation and type-ahead, beside a lazy `TreeView`
//! whose children are made up when a node is first expanded.
use toolkit::iced::widget::{column, row, text};
use toolkit::iced::{self, Fill};
use toolkit::theme::{self, Theme};
use toolkit::tree::{Children, Nodes, TreeView};
use toolkit::virtual_list::{self, Columns, Selection, VirtualList};
use toolkit::{Tokens, icon};

use super::strings::{format, label};

pub type Element<'a> = iced::Element<'a, Message, Theme>;

/// Rows in the virtual list.
pub const ROWS: usize = 100_000;
/// Children a tree node gets when it is first expanded.
const BRANCH: usize = 5;
/// Tree nodes deeper than this are leaves.
const MAX_DEPTH: usize = 3;
const LIST_HEIGHT: f32 = 360.0;

/// One list row, made up from its index: nothing is stored per row.
#[derive(Debug, Clone, Copy)]
struct Item {
    index: usize,
}

impl Item {
    fn at(index: usize) -> Self {
        Self { index }
    }

    fn name(self) -> String {
        format!("Item {:06}", self.index)
    }

    fn size(self) -> String {
        format!("{} KB", (self.index * 7919) % 100_000)
    }

    fn kind(self) -> usize {
        self.index % 4
    }

    fn icon(self) -> &'static str {
        ["description", "folder", "image", "audio_file"][self.kind()]
    }

    fn kind_label(self) -> &'static str {
        ["kind-document", "kind-folder", "kind-image", "kind-audio"][self.kind()]
    }
}

/// A tree node's data.
#[derive(Debug, Clone)]
pub struct Branch {
    name: String,
}

pub struct Lists {
    selection: Selection,
    activated: Option<usize>,
    /// Sorted column and whether ascending (only ascending/descending by
    /// index here, so the header shows the arrow).
    sort: (usize, bool),
    tree: Nodes<String, Branch>,
    tree_selection: Selection,
    tree_activated: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Message {
    Select(Selection),
    Activate(usize),
    Sort(usize),
    TreeSelect(Selection),
    TreeToggle(String),
    TreeActivate(usize),
}

impl Default for Lists {
    fn default() -> Self {
        Self::new()
    }
}

impl Lists {
    pub fn new() -> Self {
        let mut tree = Nodes::new();
        for index in 0..4 {
            let key = index.to_string();
            tree.push(
                None,
                key,
                Branch {
                    name: format("tree-node", &[("index", index.to_string())]),
                },
                Children::Lazy,
            );
        }
        let mut lists = Self {
            selection: Selection::single(0),
            activated: None,
            sort: (0, true),
            tree,
            tree_selection: Selection::single(0),
            tree_activated: None,
        };
        // The first root starts open, so the guides show.
        lists.tree.set_expanded(&"0".to_owned(), true);
        lists.load_children("0".to_owned());
        lists.tree.set_expanded(&"0/1".to_owned(), true);
        lists.load_children("0/1".to_owned());
        lists
    }

    /// Supplies a lazy node's children once it is expanded.
    fn load_children(&mut self, key: String) {
        if !self.tree.needs_children(&key) {
            return;
        }
        let depth = key.matches('/').count() + 1;
        let items = (0..BRANCH)
            .map(|index| {
                (
                    format!("{key}/{index}"),
                    Branch {
                        name: format("tree-node", &[("index", index.to_string())]),
                    },
                    if depth < MAX_DEPTH {
                        Children::Lazy
                    } else {
                        Children::None
                    },
                )
            })
            .collect();
        self.tree.set_children(&key, items);
    }

    pub fn update(&mut self, message: Message) {
        match message {
            Message::Select(selection) => self.selection = selection,
            Message::Activate(row) => self.activated = Some(row),
            Message::Sort(column) => {
                self.sort = if self.sort.0 == column {
                    (column, !self.sort.1)
                } else {
                    (column, true)
                };
            }
            Message::TreeSelect(selection) => self.tree_selection = selection,
            Message::TreeToggle(key) => {
                self.tree.toggle(&key);
                self.load_children(key);
            }
            Message::TreeActivate(row) => {
                self.tree_activated = self.tree.visible(row).map(|row| row.key.clone());
            }
        }
    }

    pub fn view(&self, tokens: Tokens) -> Element<'_> {
        let spacing = tokens.metrics.spacing;
        let heading = tokens.metrics.text.xxl;
        let columns = Columns::new()
            .column(label("column-name"), Fill)
            .column(label("column-size"), 110.0)
            .column(label("column-kind"), 120.0);
        let header = columns.header(Some(self.sort), Message::Sort);
        let ascending = self.sort.1;
        let row_of = move |index: usize| {
            if ascending {
                index
            } else {
                ROWS - 1 - index
            }
        };
        let list: VirtualList<'_, Message, Theme, iced::Renderer> = VirtualList::new(ROWS, move |index| {
            let item = Item::at(row_of(index));
            columns.row([
                row![
                    icon(item.icon()).size(tokens.metrics.text.md),
                    text(item.name())
                ]
                .spacing(spacing.sm)
                .align_y(iced::Center)
                .into(),
                text(item.size()).into(),
                text(label(item.kind_label())).into(),
            ])
        })
        .header(header)
        .selection(&self.selection)
        .on_select(Message::Select)
        .on_activate(Message::Activate)
        .type_ahead(move |prefix, from| {
            let prefix = prefix.to_lowercase();
            (from..ROWS).find(|index| {
                Item::at(row_of(*index))
                    .name()
                    .to_lowercase()
                    .starts_with(&prefix)
            })
        })
        .height(LIST_HEIGHT);
        let list_status = text(format(
            "list-selected",
            &[
                ("count", self.selection.len().to_string()),
                (
                    "cursor",
                    self.selection
                        .cursor()
                        .map_or_else(|| "-".to_owned(), |row| row_of(row).to_string()),
                ),
                (
                    "activated",
                    self.activated
                        .map_or_else(|| "-".to_owned(), |row| row_of(row).to_string()),
                ),
            ],
        ))
        .style(theme::text::muted);

        let tree: TreeView<'_, String, Branch, Message, Theme, iced::Renderer> = TreeView::new(&self.tree, move |node| {
            row![
                icon(if node.has_children() {
                    "folder"
                } else {
                    "description"
                })
                .size(tokens.metrics.text.md),
                text(node.data.name.clone())
            ]
            .spacing(spacing.sm)
            .align_y(iced::Center)
        })
        .on_toggle(Message::TreeToggle)
        .on_select(Message::TreeSelect)
        .selection(&self.tree_selection)
        .list(|list| {
            list.mode(virtual_list::Mode::Single)
                .on_activate(Message::TreeActivate)
                .height(LIST_HEIGHT)
        });
        let tree_status = text(format(
            "tree-selected",
            &[
                (
                    "selected",
                    self.tree_selection
                        .cursor()
                        .and_then(|row| self.tree.visible(row))
                        .map_or_else(|| "-".to_owned(), |row| row.key.clone()),
                ),
                (
                    "activated",
                    self.tree_activated.clone().unwrap_or_else(|| "-".to_owned()),
                ),
            ],
        ))
        .style(theme::text::muted);

        column![
            text(label("lists")).size(heading),
            row![
                column![
                    text(format("list-rows", &[("count", ROWS.to_string())])),
                    list,
                    list_status
                ]
                .spacing(spacing.sm)
                .width(Fill),
                column![text(label("tree-hint")), tree, tree_status]
                    .spacing(spacing.sm)
                    .width(Fill),
            ]
            .spacing(spacing.lg),
        ]
        .spacing(spacing.md)
        .into()
    }
}

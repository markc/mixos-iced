// SPDX-License-Identifier: MIT OR Apache-2.0
//! Virtual file presentation over a borrowed listing. Filesystem work,
//! selection policy and native transfer ownership belong to the caller.

use std::path::{Path, PathBuf};
use iced_core::{Element, Font, Length, Point, Rectangle, Size};
use crate::Tokens;

mod rows;
pub use rows::{Columns, FilePane, listing_size_width};

#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Press, Select(PathBuf), SelectModified(PathBuf, bool, bool),
    ContextMenu(Option<PathBuf>, Point), Toggle(PathBuf),
}

pub struct Row<'a> { pub path: &'a Path, pub name: &'a str, pub depth: usize, pub is_dir: bool }

pub trait Source {
    fn root(&self) -> &Path;
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool { self.len() == 0 }
    fn row(&self, index: usize) -> Option<Row<'_>>;
    fn selected(&self) -> Option<&Path> { None }
    fn is_selected(&self, path: &Path) -> bool { self.selected() == Some(path) }
    fn is_expanded(&self, _path: &Path) -> bool { false }
    fn size_text(&self, index: usize) -> String;
    fn modified_text(&self, index: usize) -> String;
    fn selected_index(&self) -> Option<usize> {
        let path = self.selected()?;
        (0..self.len()).find(|index| self.row(*index).is_some_and(|row| row.path == path))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decoration { ChevronDown, ChevronRight, Entry }

/// A narrow bridge to an externally owned transfer gesture. Returning true
/// from `active` suppresses clicks while the native operation owns the pointer.
pub trait Transfer {
    fn cancel_epoch(&self) -> u64;
    fn active(&self) -> bool;
    fn highlight(&self) -> Option<Rectangle>;
    fn hover(&self, directory: Option<&Path>, root: &Path, bounds: Rectangle, highlight: Rectangle, pointer: Point, busy: bool);
    fn start(&self, path: &Path, is_dir: bool, root: &Path, pointer: Point) -> bool;
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics { pub icon: f32, pub small: f32, pub pad: f32, pub gap: f32, pub edge: f32 }
impl Default for Metrics {
    fn default() -> Self { Self { icon: 16.0, small: 4.0, pad: 8.0, gap: 8.0, edge: 1.0 } }
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Presentation {
    pub ui_font: Font, pub mono_font: Font, pub px: f32, pub small_px: f32,
    pub chrome: Metrics, pub tokens: Tokens,
}
impl Default for Presentation {
    fn default() -> Self {
        Self { ui_font: Font::DEFAULT, mono_font: Font::MONOSPACE, px: 14.0,
            small_px: 11.0, chrome: Metrics::default(), tokens: Tokens::default() }
    }
}

/// Compose a file pane with the caller's navigation, shared column header and
/// summary controls. The listing consumes the remaining viewport.
pub fn column<'a, M: 'a, T: iced_widget::container::Catalog + 'a, R: iced_core::Renderer + 'a>(
    navigation: impl Into<Element<'a, M, T, R>>,
    header: impl Into<Element<'a, M, T, R>>,
    listing: impl Into<Element<'a, M, T, R>>,
    summary: impl Into<Element<'a, M, T, R>>,
    portion: u16,
) -> Element<'a, M, T, R> {
    iced_widget::container(iced_widget::Column::new().push(navigation).push(header).push(listing).push(summary)
        .width(Length::Fill).height(Length::Fill))
        .width(Length::FillPortion(portion)).height(Length::Fill).into()
}

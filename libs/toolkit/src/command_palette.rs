// SPDX-License-Identifier: MIT OR Apache-2.0
//! A command palette: [`Command`]s filtered by a Sublime-style fuzzy
//! search ([`fuzzy_match`]), shown by [`command_palette`] as a centred
//! query field over a dimmed backdrop — the Ctrl+Shift+P surface every
//! desktop app wants.
//!
//! The application owns the state (query, open, selection); the helpers
//! here build the view and the filtering. Key handling rides
//! [`crate::keys`]: route `Enter`/`ArrowUp`/`ArrowDown` to
//! [`activate_selection`]/[`move_selection`], or drive the whole palette
//! from the dialog-ready helpers below.

use iced_core::{Color, Element, Length};
use iced_widget::{column, container, row, text, text_input};

/// The widget id of the palette's query field, so the app can focus it
/// on open.
pub const INPUT_ID: &str = "toolkit-command-palette-query";

/// The maximum number of results shown before scrolling.
const MAX_RESULTS: usize = 12;

/// A command the palette can offer.
#[derive(Clone)]
pub struct Command<Message> {
    /// The display name shown in the palette.
    pub name: String,
    /// Optional description shown under the name.
    pub description: Option<String>,
    /// Extra search keywords, not displayed.
    pub keywords: Vec<String>,
    /// The message produced when the command is activated.
    pub message: Message,
}

impl<Message> Command<Message> {
    /// Creates a new [`Command`] with a name and the message it
    /// produces.
    #[must_use]
    pub fn new(name: impl Into<String>, message: Message) -> Self {
        Self {
            name: name.into(),
            description: None,
            keywords: Vec::new(),
            message,
        }
    }

    /// Sets the description.
    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Adds a search keyword.
    #[must_use]
    pub fn keyword(mut self, keyword: impl Into<String>) -> Self {
        self.keywords.push(keyword.into());
        self
    }

    /// The strings searched for this command: the name then keywords.
    fn haystack(&self) -> String {
        let mut haystack = self.name.clone();
        for keyword in &self.keywords {
            haystack.push(' ');
            haystack.push_str(keyword);
        }
        haystack
    }
}

/// A fuzzy match result: a score (higher is better) and the indices of
/// the matched characters in the target string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzyMatch {
    /// The match score.
    pub score: i32,
    /// The matched character indices.
    pub indices: Vec<usize>,
}

/// Performs fuzzy matching with Sublime-style scoring: word-boundary and
/// consecutive bonuses, a start-of-string bonus and a gap penalty.
/// Returns `None` when the pattern does not match.
#[must_use]
pub fn fuzzy_match(pattern: &str, target: &str) -> Option<FuzzyMatch> {
    if pattern.is_empty() {
        return Some(FuzzyMatch {
            score: 0,
            indices: vec![],
        });
    }

    let pattern_lower: Vec<char> = pattern.to_lowercase().chars().collect();
    let target_chars: Vec<char> = target.chars().collect();
    let target_lower: Vec<char> = target.to_lowercase().chars().collect();

    let mut indices = Vec::with_capacity(pattern_lower.len());
    let mut score: i32 = 0;
    let mut pattern_idx = 0;
    let mut last_match_idx: Option<usize> = None;

    for (target_idx, &target_char) in target_lower.iter().enumerate() {
        if pattern_idx >= pattern_lower.len() {
            break;
        }

        if target_char == pattern_lower[pattern_idx] {
            indices.push(target_idx);

            // Start of string bonus.
            if target_idx == 0 {
                score += 8;
            }

            // Word boundary bonus.
            if is_word_boundary(&target_chars, target_idx) {
                score += 10;
            }

            // Consecutive bonus, gap penalty.
            if let Some(last_idx) = last_match_idx {
                if target_idx == last_idx + 1 {
                    score += 5;
                } else {
                    let gap = (target_idx - last_idx - 1) as i32;
                    score -= gap;
                }
            }

            last_match_idx = Some(target_idx);
            pattern_idx += 1;
        }
    }

    if pattern_idx == pattern_lower.len() {
        score += 10;
        Some(FuzzyMatch { score, indices })
    } else {
        None
    }
}

/// Whether a position is a word boundary (after a separator, or a
/// camelCase transition).
fn is_word_boundary(chars: &[char], idx: usize) -> bool {
    if idx == 0 {
        return true;
    }

    let prev = chars[idx - 1];
    let curr = chars[idx];

    if matches!(prev, '_' | '-' | ' ' | '/' | '\\' | '.') {
        return true;
    }

    if prev.is_lowercase() && curr.is_uppercase() {
        return true;
    }

    false
}

/// A filtered command: its index into the caller's slice, its match
/// against the query, and the precomputed message.
pub struct Hit<'a, Message: 'a> {
    /// The index into the original command slice.
    pub index: usize,
    /// The fuzzy match against the query.
    pub match_: FuzzyMatch,
    /// The command.
    pub command: &'a Command<Message>,
}

/// Filters and ranks the commands against the query (best first; the
/// original order when the query is empty).
pub fn filter<'a, Message>(
    query: &str,
    commands: &'a [Command<Message>],
) -> Vec<Hit<'a, Message>>
where
    Message: 'a,
{
    let mut hits: Vec<Hit<'a, Message>> = commands
        .iter()
        .enumerate()
        .filter_map(|(index, command)| {
            fuzzy_match(query, &command.haystack()).map(|match_| Hit {
                index,
                match_,
                command,
            })
        })
        .collect();

    if !query.is_empty() {
        hits.sort_by(|a, b| {
            b.match_
                .score
                .cmp(&a.match_.score)
                .then_with(|| a.command.name.cmp(&b.command.name))
        });
    }

    hits
}

/// Moves the selection: `delta` rows through the filtered hits, wrapping
/// at the ends. Returns the new selection index.
#[must_use]
pub fn move_selection(current: Option<usize>, delta: i32, len: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let current = current.unwrap_or(0) as i32;
    let next = (current + delta).rem_euclid(len as i32);
    Some(next as usize)
}

/// The message a [`command_palette`] view produces.
#[derive(Debug, Clone)]
pub enum Event<Message> {
    /// The query changed.
    QueryChanged(String),
    /// A command was activated (Enter or click).
    Activated(Message),
    /// A click outside the palette or Escape requested it to close.
    Dismissed,
}

/// Builds the palette view: a centred column of the query field and the
/// filtered results, over a dimmed backdrop that dismisses on click.
///
/// The app shows it while open, stacked over its content.
#[allow(clippy::type_complexity)]
pub fn command_palette<'a, Message, Theme, Renderer>(
    query: &str,
    selection: Option<usize>,
    commands: &[Command<Message>],
    tokens: &crate::tokens::Tokens,
) -> Element<'a, Event<Message>, Theme, Renderer>
where
    Message: Clone + 'a,
    Theme: iced_widget::container::Catalog
        + iced_widget::text_input::Catalog
        + iced_core::widget::text::Catalog
        + iced_widget::scrollable::Catalog
        + iced_widget::button::Catalog
        + 'a,
    <Theme as iced_widget::container::Catalog>::Class<'a>:
        From<iced_widget::container::StyleFn<'a, Theme>>,
    <Theme as iced_widget::button::Catalog>::Class<'a>:
        From<iced_widget::button::StyleFn<'a, Theme>>,
    Renderer: iced_core::text::Renderer + 'a,
{
    let hits = filter(query, commands);
    let palette = iced_core::Background::Color(tokens.palette.popover);
    let scrim = iced_core::Background::Color(Color {
        a: 0.55,
        ..tokens.palette.surface
    });

    let field = text_input("Type a command…", query)
        .id(iced_core::widget::Id::new(INPUT_ID))
        .on_input(Event::QueryChanged)
        .size(tokens.metrics.text.md)
        .padding(tokens.metrics.spacing.md);

    let results: Vec<Element<'_, Event<Message>, Theme, Renderer>> =
        hits.iter().take(MAX_RESULTS).enumerate().map(|(row, hit)| {
            let selected = selection == Some(row);
            let name = text(hit.command.name.as_str()).size(tokens.metrics.text.md);
            let name = name;
            let mut entry = row![name]
                .spacing(tokens.metrics.spacing.sm)
                .align_y(iced_core::alignment::Vertical::Center);
            if let Some(description) = &hit.command.description {
                entry = entry.push(text(description.as_str()).size(tokens.metrics.text.sm));
            }
            let entry = if selected {
                container(entry).style(move |_| iced_widget::container::Style {
                    background: Some(iced_core::Background::Color(tokens.palette.selection)),
                    ..iced_widget::container::Style::default()
                })
            } else {
                container(entry)
            };
            entry
                .width(Length::Fill)
                .padding(tokens.metrics.spacing.sm)
                .into()
        })
        .collect();

    let body = column![field, column::with_children(results).spacing(2)]
        .spacing(tokens.metrics.spacing.sm)
        .padding(tokens.metrics.spacing.md)
        .width(520);

    let card = container(body)
        .style(move |_| iced_widget::container::Style {
            background: Some(palette),
            text_color: Some(tokens.palette.popover_text),
            border: iced_core::Border {
                color: tokens.palette.border,
                width: tokens.metrics.border.width,
                radius: tokens.metrics.radius.md.into(),
            },
            ..iced_widget::container::Style::default()
        })
        .width(520);

    container(
        container(card)
            .center_x(Length::Fill)
            .padding(80),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .style(move |_| iced_widget::container::Style {
        background: Some(scrim),
        ..iced_widget::container::Style::default()
    })
    .into()
}

/// Activates the selected hit, if any: the message of the command at
/// `selection` within the filtered `hits`.
#[must_use]
pub fn activate_selection<'a, Message>(
    query: &str,
    commands: &'a [Command<Message>],
    selection: Option<usize>,
) -> Option<&'a Message> {
    let hits = filter(query, commands);
    hits.get(selection?).map(|hit| &hit.command.message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commands() -> Vec<Command<u8>> {
        vec![
            Command::new("Save file", 1).keyword("write").description("Save the current file"),
            Command::new("Save all files", 2),
            Command::new("Open recent", 3).keyword("history"),
            Command::new("Close editor", 4),
        ]
    }

    #[test]
    fn fuzzy_match_scores_and_fails() {
        let m = fuzzy_match("sve", "Save file").expect("matches out of order? no: s-v-e");
        assert!(m.score > 0);
        assert_eq!(m.indices.len(), 3);
        assert!(fuzzy_match("xyz", "Save file").is_none());
        assert!(fuzzy_match("", "anything").is_some());
    }

    #[test]
    fn filtering_ranks_word_boundaries_first() {
        let all = commands();
        let hits = filter("save", &all);
        assert_eq!(hits.len(), 2);
        // "Save file" and "Save all files" both start with the word.
        assert!(hits[0].command.name.starts_with("Save"));
        assert!(hits[1].command.name.starts_with("Save"));

        // Keywords participate: "write" hits "Save file".
        let hits = filter("write", &all);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].command.message, 1);
    }

    #[test]
    fn selection_moves_and_wraps() {
        assert_eq!(move_selection(None, 1, 3), Some(1));
        assert_eq!(move_selection(Some(2), 1, 3), Some(0));
        assert_eq!(move_selection(Some(0), -1, 3), Some(2));
        assert_eq!(move_selection(Some(0), 1, 0), None);
    }

    #[test]
    fn activation_returns_the_selected_command_message() {
        let all = commands();
        assert_eq!(activate_selection("open", &all, Some(0)), Some(&3));
        assert_eq!(activate_selection("close", &all, None), None);
        assert_eq!(activate_selection("zzz", &all, Some(0)), None);
    }
}

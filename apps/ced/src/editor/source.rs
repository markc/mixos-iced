// SPDX-License-Identifier: MIT OR Apache-2.0
//! Borrowed presentation adapter. Buffer/OT ownership and bounded Unicode
//! measurement remain in the editor engine; toolkit owns widget mechanics.

use editor_model::{diag::Diagnostics, highlight::Highlight, mirror::Mirror, model::EditorModel};
use std::hash::{Hash, Hasher};
use std::ops::Range;
use toolkit::editor_pane as pane;

pub struct Source<'a> {
    identity: u64,
    mirror: &'a Mirror,
    model: &'a EditorModel,
    highlight: &'a Highlight,
    diagnostics: &'a Diagnostics,
}

impl<'a> Source<'a> {
    pub fn new(
        tab: u64,
        mirror: &'a Mirror,
        model: &'a EditorModel,
        highlight: &'a Highlight,
        diagnostics: &'a Diagnostics,
    ) -> Self {
        let mut identity = std::collections::hash_map::DefaultHasher::new();
        (tab, mirror.epoch(), mirror.buffer()).hash(&mut identity);
        Self {
            identity: identity.finish(),
            mirror,
            model,
            highlight,
            diagnostics,
        }
    }
    fn text(&self) -> &edit::text::Text {
        self.mirror.text()
    }
}

fn measure(cfg: &pane::MeasureCfg) -> edit::view::MeasureCfg {
    edit::view::MeasureCfg {
        tab_size: cfg.tab_size,
        ambiguous_wide: cfg.ambiguous_wide,
    }
}
fn origin(value: &edit::origin::Origin) -> pane::Origin<'_> {
    let kind = match value.kind {
        edit::origin::OriginKind::Human => pane::OriginKind::Human,
        edit::origin::OriginKind::Agent => pane::OriginKind::Agent,
        edit::origin::OriginKind::Tool => pane::OriginKind::Tool,
    };
    pane::Origin {
        kind,
        label: &value.label,
    }
}

impl pane::Source for Source<'_> {
    fn identity(&self) -> u64 {
        self.identity
    }
    fn revision(&self) -> u64 {
        self.mirror.view_gen()
    }
    fn len(&self) -> usize {
        self.text().len()
    }
    fn line_count(&self) -> usize {
        self.text().line_count()
    }
    fn line_start(&self, line: usize) -> Option<usize> {
        self.text().line_start(line)
    }
    fn line_range(&self, line: usize) -> Option<Range<usize>> {
        self.text().line_range(line)
    }
    fn content_end(&self, line: usize) -> usize {
        editor_model::model::content_end(self.text(), line)
    }
    fn line_of(&self, offset: usize) -> usize {
        editor_model::model::line_of(self.text(), offset)
    }
    fn clamp_offset(&self, offset: usize) -> usize {
        editor_model::model::clamp_offset(self.text(), offset)
    }
    fn read(&self, range: Range<usize>, output: &mut String) {
        let _profile = profile::Scope::new(profile::READ);
        self.text().read(range, output);
    }
    fn clusters(
        &self,
        cfg: &pane::MeasureCfg,
        range: Range<usize>,
        cells: usize,
    ) -> Box<dyn Iterator<Item = pane::Cluster> + '_> {
        let clusters =
            edit::view::clusters(self.text(), &measure(cfg), range, cells).map(|cluster| {
                pane::Cluster {
                    range: cluster.range,
                    cells: cluster.cells,
                    is_tab: cluster.is_tab,
                    ascii: cluster.ascii,
                }
            });
        if profile::enabled() {
            Box::new(profile::Walk::new(clusters))
        } else {
            Box::new(clusters)
        }
    }
    fn line_checkpoints(&self, cfg: &pane::MeasureCfg, line: usize) -> Vec<(usize, usize)> {
        let _profile = profile::Scope::new(profile::CHECKPOINTS);
        edit::view::line_checkpoints(self.text(), &measure(cfg), line)
    }
    fn state(&self) -> pane::ViewState {
        pane::ViewState {
            sel: pane::Selection {
                anchor: self.model.sel.anchor,
                head: self.model.sel.head,
            },
            scroll: pane::Scroll {
                first_line: self.model.scroll.first_line,
                x_cells: self.model.scroll.x_cells,
            },
            overwrite: self.model.overwrite,
            composition: self.model.composition.clone(),
        }
    }
    fn remote(&self) -> Box<dyn Iterator<Item = pane::Remote<'_>> + '_> {
        Box::new(self.model.remote.iter().map(|(who, selections)| {
            (
                origin(who),
                Box::new(selections.iter().map(|selection| pane::Selection {
                    anchor: selection.anchor,
                    head: selection.head,
                })) as Box<dyn Iterator<Item = pane::Selection> + '_>,
            )
        }))
    }
    fn markers(&self) -> Box<dyn Iterator<Item = pane::Marker<'_>> + '_> {
        Box::new(
            self.model
                .markers
                .changed
                .iter()
                .map(|(range, who, revision)| (range.clone(), origin(who), *revision)),
        )
    }
    fn diagnostics(&self) -> Box<dyn Iterator<Item = pane::Diagnostic> + '_> {
        Box::new(
            self.diagnostics
                .items()
                .iter()
                .map(|diagnostic| pane::Diagnostic {
                    range: diagnostic.range.clone(),
                    severity: match diagnostic.severity {
                        editor_model::diag::Severity::Error => pane::Severity::Error,
                        editor_model::diag::Severity::Warning => pane::Severity::Warning,
                        editor_model::diag::Severity::Note => pane::Severity::Note,
                    },
                }),
        )
    }
    fn highlight_spans(
        &self,
        line: usize,
        budget: &mut pane::SliceBudget,
    ) -> Vec<(Range<usize>, pane::Class)> {
        let _profile = profile::Scope::new(profile::HIGHLIGHT);
        let mut engine = editor_model::highlight::SliceBudget {
            max_lines: budget.max_lines,
        };
        let spans = self
            .highlight
            .with_spans(self.text(), line, &mut engine, |spans| {
                spans
                    .unwrap_or_default()
                    .iter()
                    .map(|(range, class)| (range.clone(), colour_class(*class)))
                    .collect()
            });
        budget.max_lines = engine.max_lines;
        spans
    }
}

/// Opt-in UI-thread samples. One aggregate per second; no document content,
/// paths or per-glyph logs. Cluster timing covers consumption of the iterator
/// (including its caller's loop), rather than adding a clock read per cluster.
mod profile {
    use std::cell::RefCell;
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};

    pub const READ: usize = 0;
    pub const CHECKPOINTS: usize = 1;
    pub const HIGHLIGHT: usize = 2;
    const CLUSTERS: usize = 3;

    pub fn enabled() -> bool {
        static ENABLED: OnceLock<bool> = OnceLock::new();
        *ENABLED.get_or_init(|| std::env::var("CED_PROFILE").is_ok_and(|value| value == "1"))
    }

    #[derive(Clone, Copy, Default)]
    struct Metric {
        calls: u64,
        us: u64,
        items: u64,
    }

    struct Samples {
        since: Instant,
        metrics: [Metric; 4],
    }

    thread_local! {
        static SAMPLES: RefCell<Samples> = RefCell::new(Samples {
            since: Instant::now(),
            metrics: [Metric::default(); 4],
        });
    }

    fn record(kind: usize, started: Instant, items: u64) {
        let now = Instant::now();
        SAMPLES.with(|samples| {
            let mut samples = samples.borrow_mut();
            let metric = &mut samples.metrics[kind];
            metric.calls += 1;
            metric.us += now.duration_since(started).as_micros() as u64;
            metric.items += items;
            if now.duration_since(samples.since) < Duration::from_secs(1) {
                return;
            }
            let [read, checkpoints, highlight, clusters] = samples.metrics;
            tracing::info!(
                read_calls = read.calls,
                read_us = read.us,
                checkpoint_calls = checkpoints.calls,
                checkpoint_us = checkpoints.us,
                highlight_calls = highlight.calls,
                highlight_us = highlight.us,
                cluster_walks = clusters.calls,
                cluster_us = clusters.us,
                clusters = clusters.items,
                "ced: source profile"
            );
            samples.metrics = [Metric::default(); 4];
            samples.since = now;
        });
    }

    pub struct Scope {
        kind: usize,
        started: Option<Instant>,
    }

    impl Scope {
        pub fn new(kind: usize) -> Self {
            Self {
                kind,
                started: enabled().then(Instant::now),
            }
        }
    }

    impl Drop for Scope {
        fn drop(&mut self) {
            if let Some(started) = self.started {
                record(self.kind, started, 0);
            }
        }
    }

    pub struct Walk<I> {
        inner: I,
        started: Instant,
        items: u64,
    }

    impl<I> Walk<I> {
        pub fn new(inner: I) -> Self {
            Self {
                inner,
                started: Instant::now(),
                items: 0,
            }
        }
    }

    impl<I: Iterator> Iterator for Walk<I> {
        type Item = I::Item;

        fn next(&mut self) -> Option<Self::Item> {
            let item = self.inner.next();
            self.items += u64::from(item.is_some());
            item
        }

        fn size_hint(&self) -> (usize, Option<usize>) {
            self.inner.size_hint()
        }
    }

    impl<I> Drop for Walk<I> {
        fn drop(&mut self) {
            record(CLUSTERS, self.started, self.items);
        }
    }
}

fn colour_class(class: editor_model::highlight::HlClass) -> pane::Class {
    use editor_model::highlight::HlClass as C;
    match class {
        C::Plain => pane::Class::Plain,
        C::Comment => pane::Class::Comment,
        C::Keyword => pane::Class::Keyword,
        C::String => pane::Class::String,
        C::Number => pane::Class::Number,
        C::Constant => pane::Class::Constant,
        C::Type => pane::Class::Type,
        C::Function => pane::Class::Function,
        C::Variable => pane::Class::Variable,
        C::Operator => pane::Class::Operator,
        C::Punctuation => pane::Class::Punctuation,
        C::Meta => pane::Class::Meta,
        C::Inserted => pane::Class::Inserted,
        C::Deleted => pane::Class::Deleted,
        C::Heading => pane::Class::Heading,
        C::Link => pane::Class::Link,
        C::Invalid => pane::Class::Invalid,
    }
}

pub(super) fn event(message: pane::Message) -> super::EditorMsg {
    use pane::Message as M;
    match message {
        M::Command(command) => super::EditorMsg::Command(command_for_engine(command)),
        M::Scrolled(scroll) => super::EditorMsg::Scrolled(editor_model::model::Scroll {
            first_line: scroll.first_line,
            x_cells: scroll.x_cells,
        }),
        M::Copy => super::EditorMsg::Copy,
        M::Cut => super::EditorMsg::Cut,
        M::PrimarySelection(text) => super::EditorMsg::PrimarySelection(text),
        M::Paste { primary } => super::EditorMsg::Paste { primary },
        M::Preedit(text) => super::EditorMsg::Preedit(text),
        M::ImeCommit(text) => super::EditorMsg::ImeCommit(text),
        M::Focus(focused) => super::EditorMsg::Focus(focused),
        M::Layout(layout) => super::EditorMsg::Layout(layout),
    }
}

fn motion_for_engine(motion: pane::Motion) -> editor_model::model::Motion {
    use editor_model::model::Motion as E;
    use pane::Motion as M;
    match motion {
        M::Left => E::Left,
        M::Right => E::Right,
        M::WordLeft => E::WordLeft,
        M::WordRight => E::WordRight,
        M::Up => E::Up,
        M::Down => E::Down,
        M::Home => E::Home,
        M::End => E::End,
        M::PageUp(rows) => E::PageUp(rows),
        M::PageDown(rows) => E::PageDown(rows),
        M::DocStart => E::DocStart,
        M::DocEnd => E::DocEnd,
        M::To(offset) => E::To(offset),
    }
}
fn command_for_engine(command: pane::Command) -> editor_model::model::EditCommand {
    use editor_model::model::EditCommand as E;
    use pane::Command as C;
    match command {
        C::Insert(text) => E::Insert(text),
        C::Newline => E::Newline,
        C::Backspace => E::Backspace,
        C::Delete => E::Delete,
        C::DeleteWordLeft => E::DeleteWordLeft,
        C::DeleteWordRight => E::DeleteWordRight,
        C::Tab => E::Tab,
        C::Outdent => E::Outdent,
        C::DuplicateLine => E::DuplicateLine,
        C::DeleteLine => E::DeleteLine,
        C::MoveLineUp => E::MoveLineUp,
        C::MoveLineDown => E::MoveLineDown,
        C::ToggleComment => E::ToggleComment,
        C::Move { to, extend } => E::Move {
            to: motion_for_engine(to),
            extend,
        },
        C::SelectAll => E::SelectAll,
        C::SelectWord(offset) => E::SelectWord(offset),
        C::SelectLine(offset) => E::SelectLine(offset),
        C::SetSelection(selection) => E::SetSelection(edit::anchor::Selection {
            anchor: selection.anchor,
            head: selection.head,
        }),
    }
}

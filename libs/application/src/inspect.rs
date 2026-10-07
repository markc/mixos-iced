// SPDX-License-Identifier: MIT OR Apache-2.0
//! Read-only native layout inspection.
//!
//! [`channel`] registers inspection [`Target`]s (an external alias, an actual
//! widget id and an optional kind filter) and returns a [`Handle`] plus a
//! bootstrap [`Task`]. The task runs on the host's existing executor and
//! converts incoming [`Request`]s into narrow selector-only runtime query
//! actions: it emits neither an application message for a request nor one for
//! a response, and it never requests a redraw. A query only reads cached
//! layouts; a missing interface or an overlay that has not been laid out is
//! reported as [`Error::NotReady`], never as manufactured layout.
//!
//! One query is in flight per [`Handle`]; excess requests return
//! [`Error::Busy`]. The reply carries raw layout bounds, clipped visible
//! bounds, the layer and the layout sequence — never text, editor state,
//! unique id debug strings or reconstructed rectangles.

use std::sync::Arc;
use std::sync::Mutex;

use iced::{Rectangle, Size, Task, widget};
use iced_runtime::core::window;
use iced_runtime::futures::futures::channel::{mpsc, oneshot};
use iced_runtime::widget::selector::{self as select, Candidate, QueryTarget};

/// The widget kind a [`Target`] selects, and a [`Record`] reports.
pub type Kind = select::Kind;

/// The layout layer a [`Request`] queries, and a [`Snapshot`] reports.
pub type Layer = select::Layer;

/// A registered inspection target: an external alias, an actual widget id
/// and an optional kind filter.
pub struct Target {
    alias: String,
    id: widget::Id,
    kind: Option<Kind>,
}

impl Target {
    /// A target for the widget with the given actual id, reported under
    /// `alias`.
    pub fn new(alias: impl Into<String>, id: widget::Id) -> Self {
        Self {
            alias: alias.into(),
            id,
            kind: None,
        }
    }

    /// Restricts the target to one candidate kind. Without a filter, every
    /// candidate kind under the id is reported; with one, only that kind.
    pub fn kind(mut self, kind: Kind) -> Self {
        self.kind = Some(kind);
        self
    }
}

/// Query limits with validated private fields.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    aliases: usize,
    alias_bytes: usize,
    records: usize,
    visited: usize,
    encoded_bytes: usize,
}

impl Limits {
    /// The default limits: 32 aliases, 128 bytes per alias, 128 result
    /// records, 8,192 visited candidates and a 64 KiB encoded response.
    pub fn new() -> Self {
        Self::default()
    }

    /// The maximum number of registered aliases.
    pub fn aliases(mut self, value: usize) -> Self {
        self.aliases = value;
        self
    }

    /// The maximum alias length, in bytes.
    pub fn alias_bytes(mut self, value: usize) -> Self {
        self.alias_bytes = value;
        self
    }

    /// The maximum number of result records per layer.
    pub fn records(mut self, value: usize) -> Self {
        self.records = value;
        self
    }

    /// The maximum number of visited candidates per layer.
    pub fn visited(mut self, value: usize) -> Self {
        self.visited = value;
        self
    }

    /// The maximum encoded response size, in bytes.
    pub fn encoded_bytes(mut self, value: usize) -> Self {
        self.encoded_bytes = value;
        self
    }

    /// The maximum number of registered aliases.
    pub fn max_aliases(&self) -> usize {
        self.aliases
    }

    /// The maximum alias length, in bytes.
    pub fn max_alias_bytes(&self) -> usize {
        self.alias_bytes
    }

    /// The maximum number of result records per layer.
    pub fn max_records(&self) -> usize {
        self.records
    }

    /// The maximum number of visited candidates per layer.
    pub fn max_visited(&self) -> usize {
        self.visited
    }

    /// The maximum encoded response size, in bytes.
    pub fn max_encoded_bytes(&self) -> usize {
        self.encoded_bytes
    }

    fn validate(&self) -> Result<(), Error> {
        if self.aliases < 1 {
            return Err(Error::LimitsInvalid("aliases"));
        }
        if self.alias_bytes < 1 {
            return Err(Error::LimitsInvalid("alias_bytes"));
        }
        if self.records < 1 {
            return Err(Error::LimitsInvalid("records"));
        }
        if self.visited < 1 {
            return Err(Error::LimitsInvalid("visited"));
        }
        if self.encoded_bytes < 1 {
            return Err(Error::LimitsInvalid("encoded_bytes"));
        }
        Ok(())
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            aliases: 32,
            alias_bytes: 128,
            records: 128,
            visited: 8_192,
            encoded_bytes: 65_536,
        }
    }
}

/// Which window a [`Request`] queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
    /// The single live window; fails when there is more than one. It never
    /// silently picks the first.
    Only,
    /// An explicit runtime window id, as reported by a previous snapshot.
    Id(u64),
}

/// One query request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The window to query.
    pub window: Window,
    /// The registered aliases to select; empty selects all registered ones.
    pub aliases: Vec<String>,
    /// The layout layer to inspect.
    pub layer: Layer,
}

impl Request {
    /// A base-layer request for the given window selecting all aliases.
    pub fn new(window: Window) -> Self {
        Self {
            window,
            aliases: Vec::new(),
            layer: Layer::Base,
        }
    }

    /// Selects only these registered aliases.
    pub fn aliases(mut self, aliases: impl IntoIterator<Item = String>) -> Self {
        self.aliases = aliases.into_iter().collect();
        self
    }

    /// Queries this layout layer.
    pub fn layer(mut self, layer: Layer) -> Self {
        self.layer = layer;
        self
    }
}

/// The status of one requested alias in a [`Snapshot`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AliasStatus {
    /// Exactly one record was found.
    Found,
    /// No record was found; the widget may not be realised or visible.
    Missing,
    /// More than one record was found; all of them are reported.
    Ambiguous,
}

/// The per-alias outcome of a [`Snapshot`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasResult {
    /// The requested alias.
    pub alias: String,
    /// The outcome for that alias.
    pub status: AliasStatus,
}

/// One layout fact for one selected candidate.
#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    /// The registered alias of the candidate.
    pub alias: String,
    /// The kind of the candidate.
    pub kind: Kind,
    /// The raw layout rectangle of the candidate.
    pub layout_bounds: Rectangle,
    /// The visible rectangle of the candidate, in client logical
    /// coordinates, clipped by the actual client viewport and any drawing
    /// clip. It never proves pixels, occlusion or clickability.
    pub visible_bounds: Option<Rectangle>,
    /// The layout layer that answered the query.
    pub layer: Layer,
}

/// The report of one query.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    /// The actual runtime window id that answered the query.
    pub window_id: u64,
    /// The client logical size of the window.
    pub logical_size: Size,
    /// The layout sequence of the layout that answered the query. Layout
    /// evidence only, never a presentation or settings revision counter.
    pub layout_sequence: u64,
    /// The queried layout layer.
    pub layer: Layer,
    /// The selected records.
    pub records: Vec<Record>,
    /// The number of candidates visited during the traversal.
    pub visited: usize,
    /// Whether the traversal stopped early because a limit was reached.
    pub truncated: bool,
    /// The outcome for each requested alias, in request order.
    pub aliases: Vec<AliasResult>,
}

/// A failed registration, request or query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Another query is already in flight on the handle.
    Busy,
    /// The inspector was closed, or the reply channel was lost.
    Closed,
    /// The window has no live interface, or the requested overlay has not
    /// been laid out yet.
    NotReady,
    /// [`Window::Only`] was requested but there is more than one window.
    MultipleWindows,
    /// The requested window does not exist.
    WindowNotFound,
    /// The request selected an alias that is not registered.
    UnknownAlias(String),
    /// Two targets share the same alias.
    DuplicateAlias(String),
    /// Two targets could match the same candidate; their id and kind
    /// filters overlap, so the query would pick one accidentally.
    AmbiguousTarget { alias: String, other: String },
    /// More targets than the limit allows.
    TooManyAliases { limit: usize },
    /// An alias exceeds the byte limit.
    AliasTooLong {
        alias: String,
        bytes: usize,
        limit: usize,
    },
    /// A limit was set below its effective minimum.
    LimitsInvalid(&'static str),
}

impl From<select::QueryError> for Error {
    fn from(error: select::QueryError) -> Self {
        match error {
            select::QueryError::NotReady => Error::NotReady,
            select::QueryError::MultipleWindows => Error::MultipleWindows,
            select::QueryError::WindowNotFound => Error::WindowNotFound,
        }
    }
}

/// The query side of a registered inspector.
#[derive(Clone)]
pub struct Handle {
    inner: Arc<Inner>,
}

struct Inner {
    targets: Vec<Target>,
    limits: Limits,
    request_tx: Mutex<Option<mpsc::Sender<(Request, Reply)>>>,
    pending_reply: Mutex<Option<Reply>>,
}

type Reply = crate::message::Once<oneshot::Sender<Result<Snapshot, Error>>>;

impl Handle {
    /// Runs one query and waits for its [`Snapshot`]. One query is in flight
    /// per handle; a concurrent request returns [`Error::Busy`], and a
    /// closed inspector returns [`Error::Closed`].
    pub async fn query(&self, request: Request) -> Result<Snapshot, Error> {
        // A dropped query future releases the single pending slot, so a
        // cancelled query cannot wedge later ones.
        struct PendingGuard(Arc<Inner>, Reply);
        impl Drop for PendingGuard {
            fn drop(&mut self) {
                let mut pending = self.0.pending_reply.lock().unwrap();
                if pending.as_ref() == Some(&self.1) {
                    *pending = None;
                }
            }
        }
        let (reply_tx, reply_rx) = oneshot::channel();
        let reply_tx = Reply::new(reply_tx);
        {
            let mut pending = self.inner.pending_reply.lock().unwrap();
            if pending.is_some() {
                return Err(Error::Busy);
            }
            *pending = Some(reply_tx.clone());
        }
        let guard = PendingGuard(Arc::clone(&self.inner), reply_tx.clone());

        let sent = match self
            .inner
            .request_tx
            .lock()
            .unwrap()
            .as_mut()
            .map(|tx| tx.try_send((request, reply_tx)))
        {
            Some(Ok(())) => Ok(()),
            // Full: a cancelled earlier request is still being processed by
            // the task, so the slot is genuinely busy.
            Some(Err(error)) if error.is_full() => Err(Error::Busy),
            // Disconnected: the inspector was closed.
            Some(Err(_)) | None => Err(Error::Closed),
        };

        if let Err(error) = sent {
            drop(guard);
            return Err(error);
        }

        let result = reply_rx.await.map_err(|_| Error::Closed)??;
        drop(guard);
        Ok(result)
    }

    /// Closes the inspector: the request channel ends and a pending request
    /// resolves as [`Error::Closed`]. Safe to call more than once.
    pub fn close(&self) {
        *self.inner.request_tx.lock().unwrap() = None;
        if let Some(reply) = self
            .inner
            .pending_reply
            .lock()
            .unwrap()
            .take()
            .and_then(|reply| reply.take())
        {
            let _ = reply.send(Err(Error::Closed));
        }
    }
}

/// Registers inspection targets and returns the query [`Handle`] and the
/// bootstrap [`Task`] the application batches into its initial task exactly
/// once. The task never produces an application message.
pub fn channel<Message: Send + 'static>(
    targets: Vec<Target>,
    limits: Limits,
) -> Result<(Handle, Task<Message>), Error> {
    limits.validate()?;
    validate_targets(&targets, &limits)?;

    let inner = Arc::new(Inner {
        targets,
        limits,
        request_tx: Mutex::new(None),
        pending_reply: Mutex::new(None),
    });

    let (request_tx, request_rx) = mpsc::channel(1);
    *inner.request_tx.lock().unwrap() = Some(request_tx);

    let task_inner = Arc::clone(&inner);
    let task = Task::stream(request_rx)
        .then(move |(request, reply)| run_request(Arc::clone(&task_inner), request, reply));

    Ok((Handle { inner }, task))
}

fn validate_targets(targets: &[Target], limits: &Limits) -> Result<(), Error> {
    if targets.len() > limits.aliases {
        return Err(Error::TooManyAliases {
            limit: limits.aliases,
        });
    }

    for target in targets {
        let bytes = target.alias.len();
        if bytes > limits.alias_bytes {
            return Err(Error::AliasTooLong {
                alias: target.alias.clone(),
                bytes,
                limit: limits.alias_bytes,
            });
        }
    }

    for (index, target) in targets.iter().enumerate() {
        for other in &targets[..index] {
            if target.alias == other.alias {
                return Err(Error::DuplicateAlias(target.alias.clone()));
            }
            if target.id == other.id && kinds_overlap(target.kind, other.kind) {
                return Err(Error::AmbiguousTarget {
                    alias: target.alias.clone(),
                    other: other.alias.clone(),
                });
            }
        }
    }

    Ok(())
}

fn kinds_overlap(left: Option<Kind>, right: Option<Kind>) -> bool {
    match (left, right) {
        (None, _) | (_, None) => true,
        (Some(left), Some(right)) => left == right,
    }
}

fn select_aliases(inner: &Inner, request: &Request) -> Result<Vec<usize>, Error> {
    if request.aliases.is_empty() {
        return Ok((0..inner.targets.len()).collect());
    }

    request
        .aliases
        .iter()
        .map(|alias| {
            inner
                .targets
                .iter()
                .position(|target| &target.alias == alias)
                .ok_or_else(|| Error::UnknownAlias(alias.clone()))
        })
        .collect()
}

fn run_request<Message: Send + 'static>(
    inner: Arc<Inner>,
    request: Request,
    reply: Reply,
) -> Task<Message> {
    let selected = match select_aliases(&inner, &request) {
        Ok(selected) => selected,
        Err(error) => {
            if let Some(reply) = reply.take() {
                let _ = reply.send(Err(error));
            }
            return Task::none();
        }
    };

    let target = match request.window {
        Window::Only => QueryTarget::Only,
        Window::Id(id) => QueryTarget::Id(window::Id::from_raw(id)),
    };

    let make_selector = {
        let inner = Arc::clone(&inner);
        let selected = selected.clone();
        move || {
            let inner = Arc::clone(&inner);
            let selected = selected.clone();
            move |candidate: Candidate<'_>| -> Option<u8> {
                let id = candidate.id()?;
                for &index in &selected {
                    let target = &inner.targets[index];
                    if id == &target.id
                        && target.kind.is_none_or(|kind| Kind::of(&candidate) == kind)
                    {
                        return Some(index as u8);
                    }
                }
                None
            }
        }
    };

    let limits = select::Limits::new(inner.limits.visited, inner.limits.records);

    let query = select::query(make_selector(), target, request.layer, limits);

    query.then(move |result| {
        let Some(reply) = reply.take() else {
            return Task::none();
        };
        let report = match result {
            Ok(report) => report,
            Err(error) => {
                let _ = reply.send(Err(error.into()));
                return Task::none();
            }
        };

        let snapshot = assemble(&inner, &selected, report);
        let _ = reply.send(snapshot);
        Task::none()
    })
}

fn assemble(
    inner: &Inner,
    selected: &[usize],
    report: select::QueryReport,
) -> Result<Snapshot, Error> {
    let records = report
        .records
        .iter()
        .map(|record| Record {
            alias: inner.targets[record.alias as usize].alias.clone(),
            kind: record.kind,
            layout_bounds: record.layout_bounds,
            visible_bounds: record.visible_bounds,
            layer: report.layer,
        })
        .collect::<Vec<_>>();

    let aliases = selected
        .iter()
        .map(|&index| {
            let alias = inner.targets[index].alias.clone();
            let count = records
                .iter()
                .filter(|record| record.alias == alias)
                .count();
            AliasResult {
                alias,
                status: match count {
                    0 => AliasStatus::Missing,
                    1 => AliasStatus::Found,
                    _ => AliasStatus::Ambiguous,
                },
            }
        })
        .collect();

    Ok(Snapshot {
        window_id: report.window_id.raw(),
        logical_size: report.logical_size,
        layout_sequence: report.layout_sequence,
        layer: report.layer,
        records,
        visited: report.visited,
        truncated: report.truncated,
        aliases,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::Point;

    fn target(alias: &str) -> Target {
        Target::new(alias, widget::Id::new("probe"))
    }

    fn inner(targets: Vec<Target>) -> Inner {
        Inner {
            targets,
            limits: Limits::new(),
            request_tx: Mutex::new(None),
            pending_reply: Mutex::new(None),
        }
    }

    fn report() -> select::QueryReport {
        select::QueryReport {
            layer: Layer::Base,
            records: vec![select::Record {
                alias: 0,
                kind: Kind::Container,
                layout_bounds: Rectangle::new(Point::ORIGIN, Size::new(800.0, 600.0)),
                visible_bounds: Some(Rectangle::new(Point::ORIGIN, Size::new(800.0, 600.0))),
            }],
            visited: 7,
            truncated: false,
            layout_sequence: 3,
            logical_size: Size::new(800.0, 600.0),
            window_id: window::Id::from_raw(9),
        }
    }

    #[test]
    fn targets_are_validated_at_registration() {
        let limits = Limits::new();

        assert!(
            matches!(channel::<()>(vec![target("a"), target("a")], limits),
            Err(Error::DuplicateAlias(alias)) if alias == "a")
        );
        assert!(matches!(
            channel::<()>(
                vec![target("a"), Target::new("b", widget::Id::new("probe"))],
                limits
            ),
            Err(Error::AmbiguousTarget {alias, other}) if alias == "b" && other == "a"
        ));
        // Disjoint kind filters under one id are unambiguous.
        assert!(
            channel::<()>(
                vec![
                    Target::new("a", widget::Id::new("probe")).kind(Kind::Focusable),
                    Target::new("b", widget::Id::new("probe")).kind(Kind::TextInput),
                ],
                limits
            )
            .is_ok()
        );
        assert!(matches!(
            channel::<()>(vec![target("toolong")], limits.alias_bytes(4)),
            Err(Error::AliasTooLong { .. })
        ));
        assert!(matches!(
            channel::<()>(vec![target("a"), target("b")], limits.aliases(1)),
            Err(Error::TooManyAliases { .. })
        ));
    }

    #[test]
    fn limits_are_validated() {
        assert!(Limits::new().validate().is_ok());
        assert_eq!(
            Limits::new().aliases(0).validate(),
            Err(Error::LimitsInvalid("aliases"))
        );
        assert_eq!(
            Limits::new().records(0).validate(),
            Err(Error::LimitsInvalid("records"))
        );
    }

    #[test]
    fn snapshots_map_records_and_alias_outcomes() {
        let inner = inner(vec![
            target("probe"),
            Target::new("other", widget::Id::new("other")),
        ]);
        let snapshot = assemble(&inner, &[0, 1], report()).unwrap();

        assert_eq!(snapshot.window_id, 9);
        assert_eq!(snapshot.logical_size, Size::new(800.0, 600.0));
        assert_eq!(snapshot.layout_sequence, 3);
        assert_eq!(snapshot.layer, Layer::Base);
        assert_eq!(snapshot.visited, 7);
        assert!(!snapshot.truncated);
        assert_eq!(snapshot.records.len(), 1);
        assert_eq!(snapshot.records[0].alias, "probe");
        assert_eq!(
            snapshot.records[0].layout_bounds,
            Rectangle::new(Point::ORIGIN, Size::new(800.0, 600.0))
        );
        assert_eq!(
            snapshot.aliases,
            vec![
                AliasResult {
                    alias: "probe".to_owned(),
                    status: AliasStatus::Found,
                },
                AliasResult {
                    alias: "other".to_owned(),
                    status: AliasStatus::Missing,
                },
            ]
        );
    }

    #[test]
    fn duplicate_records_report_ambiguous() {
        let inner = inner(vec![target("probe")]);
        let mut report = report();
        report.records.push(report.records[0].clone());
        let snapshot = assemble(&inner, &[0], report).unwrap();

        assert_eq!(snapshot.records.len(), 2);
        assert_eq!(snapshot.aliases[0].status, AliasStatus::Ambiguous);
    }

    #[test]
    fn unknown_aliases_are_rejected_and_empty_selects_all() {
        let inner = inner(vec![
            target("probe"),
            Target::new("other", widget::Id::new("other")),
        ]);

        assert_eq!(
            select_aliases(
                &inner,
                &Request::new(Window::Only).aliases(["nope".to_owned()])
            ),
            Err(Error::UnknownAlias("nope".to_owned()))
        );
        assert_eq!(
            select_aliases(&inner, &Request::new(Window::Only)).unwrap(),
            vec![0, 1]
        );
    }
}

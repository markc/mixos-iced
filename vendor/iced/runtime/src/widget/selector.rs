//! Find and query widgets in your applications.
pub use iced_selector::{
    Bounded, Candidate, Kind, Limits, Record, Selector, Target, Text, Traversal, id, is_focused,
};

use crate::core::window;
use crate::core::widget::operation;
use crate::task;
use crate::Action;
use crate::Task;

use std::sync::{Arc, Mutex};

/// Finds a widget matching the given [`Selector`].
pub fn find<S>(selector: S) -> Task<Option<S::Output>>
where
    S: Selector + Send + 'static,
    S::Output: Send + Clone + 'static,
{
    task::widget(selector.find())
}

/// Finds all widgets matching the given [`Selector`].
pub fn find_all<S>(selector: S) -> Task<Vec<S::Output>>
where
    S: Selector + Send + 'static,
    S::Output: Send + Clone + 'static,
{
    task::widget(selector.find_all())
}

/// A layout layer of a window's user interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    /// The base widget tree, which is always laid out.
    Base,
    /// The overlay, which is only queryable once it has been laid out.
    Overlay,
}

/// The report of one read-only layout query.
#[derive(Debug, Clone, PartialEq)]
pub struct QueryReport {
    /// The layout layer that was queried.
    pub layer: Layer,
    /// The raw layout facts selected by the query.
    pub records: Vec<Record>,
    /// The number of candidates visited during the traversal.
    pub visited: usize,
    /// Whether the traversal stopped early because a limit was reached.
    pub truncated: bool,
    /// The layout sequence of the cached layout that answered the query.
    /// Layout evidence only, never a presentation revision counter.
    pub layout_sequence: u64,
    /// The client logical size of the window.
    pub logical_size: crate::core::Size,
    /// The runtime id of the window.
    pub window_id: window::Id,
}

/// Why a read-only query could not be answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryError {
    /// The window has no live interface, or the requested overlay has not
    /// been laid out yet.
    NotReady,
    /// [`QueryTarget::Only`] was requested but there is more than one window.
    MultipleWindows,
    /// The requested window does not exist.
    WindowNotFound,
}

/// Which window a read-only query targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryTarget {
    /// The single live window; fails when there is more than one.
    Only,
    /// An explicit runtime window id.
    Id(window::Id),
}

/// Runs a read-only [`Selector`] query on the given window and layer,
/// producing a [`QueryReport`] without requesting a redraw and without
/// producing an application message.
///
/// The selector returns the alias index (`u8`) of each matched candidate;
/// the traversal records raw layout facts only. It never runs a mutating
/// [`widget::Operation`](crate::core::widget::Operation).
pub fn query<S>(
    selector: S,
    target: QueryTarget,
    layer: Layer,
    limits: Limits,
) -> Task<Result<QueryReport, QueryError>>
where
    S: Selector<Output = u8> + Send + 'static,
{
    task::oneshot(|reply| {
        let traversal = Arc::new(Mutex::new(None));
        let output = Arc::clone(&traversal);

        // The widget tree operate surface is fixed to `Operation<()>`, so
        // the typed traversal output is captured through the established
        // `map` adapter when the runtime finishes the operation.
        let operation = operation::map(
            Box::new(iced_selector::query(selector).with_limits(limits)),
            move |traversal| {
                *output.lock().unwrap() = Some(traversal);
            },
        );

        Action::Query {
            target,
            layer,
            operation: Box::new(operation),
            traversal,
            reply,
        }
    })
}

use crate::Selector;
use crate::core::widget::operation::{Focusable, Outcome, Scrollable, TextInput};
use crate::core::widget::{Id, Operation};
use crate::core::{Rectangle, Vector};
use crate::target::Candidate;

use std::any::Any;

/// An [`Operation`] that runs the [`Selector`] and stops after
/// the first [`Output`](Selector::Output) is produced.
pub type Find<S> = Finder<One<S>>;

/// An [`Operation`] that runs the [`Selector`] for the entire
/// widget tree and aggregates all of its [`Output`](Selector::Output).
pub type FindAll<S> = Finder<All<S>>;

#[derive(Debug)]
pub struct One<S>
where
    S: Selector,
{
    selector: S,
    output: Option<S::Output>,
}

impl<S> One<S>
where
    S: Selector,
{
    pub fn new(selector: S) -> Self {
        Self {
            selector,
            output: None,
        }
    }
}

impl<S> Strategy for One<S>
where
    S: Selector,
    S::Output: Clone,
{
    type Output = Option<S::Output>;

    fn feed(&mut self, target: Candidate<'_>) {
        if let Some(output) = self.selector.select(target) {
            self.output = Some(output);
        }
    }

    fn is_done(&self) -> bool {
        self.output.is_some()
    }

    fn finish(&self) -> Self::Output {
        self.output.clone()
    }
}

#[derive(Debug)]
pub struct All<S>
where
    S: Selector,
{
    selector: S,
    outputs: Vec<S::Output>,
}

impl<S> All<S>
where
    S: Selector,
{
    pub fn new(selector: S) -> Self {
        Self {
            selector,
            outputs: Vec::new(),
        }
    }
}

impl<S> Strategy for All<S>
where
    S: Selector,
    S::Output: Clone,
{
    type Output = Vec<S::Output>;

    fn feed(&mut self, target: Candidate<'_>) {
        if let Some(output) = self.selector.select(target) {
            self.outputs.push(output);
        }
    }

    fn len(&self) -> usize {
        self.outputs.len()
    }

    fn is_done(&self) -> bool {
        false
    }

    fn finish(&self) -> Self::Output {
        self.outputs.clone()
    }
}

pub trait Strategy {
    type Output;

    fn feed(&mut self, target: Candidate<'_>);

    /// The number of outputs produced so far, used to bound the traversal.
    fn len(&self) -> usize {
        usize::from(self.is_done())
    }

    fn is_done(&self) -> bool;

    fn finish(&self) -> Self::Output;

    /// Assembles the final output together with the traversal bookkeeping.
    /// The default keeps the plain [`Strategy::finish`] output.
    fn finish_with(&self, _visited: usize, _truncated: bool) -> Self::Output {
        self.finish()
    }
}

/// Traversal limits for a bounded [`Finder`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    max_visited: usize,
    max_results: usize,
}

impl Limits {
    /// At most one visited candidate and one result is the effective
    /// minimum; smaller values are clamped.
    pub fn new(max_visited: usize, max_results: usize) -> Self {
        Self {
            max_visited: max_visited.max(1),
            max_results: max_results.max(1),
        }
    }

    /// The maximum number of visited candidates.
    pub fn max_visited(self) -> usize {
        self.max_visited
    }

    /// The maximum number of produced results.
    pub fn max_results(self) -> usize {
        self.max_results
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_visited: 8_192,
            max_results: 128,
        }
    }
}

#[derive(Debug)]
pub struct Finder<S> {
    strategy: S,
    stack: Vec<(Rectangle, Vector)>,
    viewport: Rectangle,
    translation: Vector,
    limits: Limits,
    visited: usize,
    truncated: bool,
}

impl<S> Finder<S> {
    pub fn new(strategy: S) -> Self {
        Self {
            strategy,
            stack: vec![(Rectangle::INFINITE, Vector::ZERO)],
            viewport: Rectangle::INFINITE,
            translation: Vector::ZERO,
            limits: Limits::default(),
            visited: 0,
            truncated: false,
        }
    }

    /// Sets the initial viewport of the [`Finder`]. The viewport is intersected
    /// with any drawing clip advertised during the traversal.
    pub fn with_viewport(mut self, viewport: Rectangle) -> Self {
        self.viewport = viewport;
        self
    }

    /// Sets the traversal limits of the [`Finder`].
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// The number of candidates visited so far.
    pub fn visited(&self) -> usize {
        self.visited
    }

    /// Whether the traversal stopped early because a limit was reached.
    pub fn truncated(&self) -> bool {
        self.truncated
    }
}

impl<S: Strategy> Finder<S> {
    fn is_done(&self) -> bool {
        self.truncated || self.strategy.is_done()
    }

    /// Feeds one candidate to the strategy, enforcing the limits.
    fn visit(&mut self, candidate: Candidate<'_>) {
        if self.is_done() {
            return;
        }

        self.visited += 1;
        if self.visited > self.limits.max_visited || self.strategy.len() >= self.limits.max_results
        {
            self.truncated = true;
            return;
        }

        self.strategy.feed(candidate);
    }
}

impl<S> Operation<S::Output> for Finder<S>
where
    S: Strategy + Send,
    S::Output: Send,
{
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<S::Output>)) {
        if self.is_done() {
            return;
        }

        self.stack.push((self.viewport, self.translation));
        operate(self);
        let _ = self.stack.pop();

        let (viewport, translation) = self.stack.last().unwrap();
        self.viewport = *viewport;
        self.translation = *translation;
    }

    fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
        self.visit(Candidate::Container {
            id,
            bounds,
            visible_bounds: self.viewport.intersection(&(bounds + self.translation)),
        });
    }

    fn clip(&mut self, bounds: Rectangle) {
        if self.is_done() {
            return;
        }

        self.viewport = self
            .viewport
            .intersection(&(bounds + self.translation))
            .unwrap_or_default();
    }

    fn focusable(&mut self, id: Option<&Id>, bounds: Rectangle, state: &mut dyn Focusable) {
        self.visit(Candidate::Focusable {
            id,
            bounds,
            visible_bounds: self.viewport.intersection(&(bounds + self.translation)),
            state,
        });
    }

    fn scrollable(
        &mut self,
        id: Option<&Id>,
        bounds: Rectangle,
        content_bounds: Rectangle,
        translation: Vector,
        state: &mut dyn Scrollable,
    ) {
        let visible_bounds = self.viewport.intersection(&(bounds + self.translation));

        self.visit(Candidate::Scrollable {
            id,
            bounds,
            visible_bounds,
            content_bounds,
            translation,
            state,
        });

        self.translation -= translation;
        self.viewport = visible_bounds.unwrap_or_default();
    }

    fn text_input(&mut self, id: Option<&Id>, bounds: Rectangle, state: &mut dyn TextInput) {
        self.visit(Candidate::TextInput {
            id,
            bounds,
            visible_bounds: self.viewport.intersection(&(bounds + self.translation)),
            state,
        });
    }

    fn text(&mut self, id: Option<&Id>, bounds: Rectangle, text: &str) {
        self.visit(Candidate::Text {
            id,
            bounds,
            visible_bounds: self.viewport.intersection(&(bounds + self.translation)),
            content: text,
        });
    }

    fn custom(&mut self, id: Option<&Id>, bounds: Rectangle, state: &mut dyn Any) {
        self.visit(Candidate::Custom {
            id,
            bounds,
            visible_bounds: self.viewport.intersection(&(bounds + self.translation)),
            state,
        });
    }

    fn finish(&self) -> Outcome<S::Output> {
        Outcome::Some(self.strategy.finish_with(self.visited, self.truncated))
    }
}

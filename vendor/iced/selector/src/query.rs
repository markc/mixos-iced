//! A bounded read-only traversal that records raw layout facts.
//!
//! The traversal is meant for layout inspection: it records an alias index,
//! the candidate kind and raw layout and clipped visible bounds, and never
//! text, editor state, unique id debug strings or reconstructed rectangles.

use crate::Selector;
use crate::core::Rectangle;
use crate::find::{Finder, Strategy};
use crate::target::Candidate;

/// The widget kind a [`Record`] describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A container widget.
    Container,
    /// A focusable widget.
    Focusable,
    /// A scrollable widget.
    Scrollable,
    /// A text input widget.
    TextInput,
    /// A text widget.
    Text,
    /// A custom widget.
    Custom,
}

impl Kind {
    /// The kind of a [`Candidate`].
    pub fn of(candidate: &Candidate<'_>) -> Self {
        match candidate {
            Candidate::Container { .. } => Kind::Container,
            Candidate::Focusable { .. } => Kind::Focusable,
            Candidate::Scrollable { .. } => Kind::Scrollable,
            Candidate::TextInput { .. } => Kind::TextInput,
            Candidate::Text { .. } => Kind::Text,
            Candidate::Custom { .. } => Kind::Custom,
        }
    }
}

/// A single layout fact for one selected candidate.
///
/// `alias` is the index the [`Selector`] returned, so the caller maps it
/// back to its own registration. No text or id is copied.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Record {
    /// The alias index selected by the [`Selector`].
    pub alias: u8,
    /// The kind of the candidate.
    pub kind: Kind,
    /// The raw layout rectangle of the candidate.
    pub layout_bounds: Rectangle,
    /// The visible rectangle of the candidate, clipped by the traversal
    /// viewport and any advertised drawing clip, in client logical
    /// coordinates.
    pub visible_bounds: Option<Rectangle>,
}

/// The result of one bounded traversal.
#[derive(Debug, Clone, PartialEq)]
pub struct Traversal {
    /// The recorded layout facts.
    pub records: Vec<Record>,
    /// The number of candidates visited.
    pub visited: usize,
    /// Whether the traversal stopped early because a limit was reached.
    pub truncated: bool,
}

/// A [`Strategy`] that records a [`Record`] for every selected candidate.
#[derive(Debug)]
pub struct Raw<S> {
    selector: S,
    records: Vec<Record>,
}

impl<S> Raw<S> {
    /// Creates a new [`Raw`] strategy.
    pub fn new(selector: S) -> Self {
        Self {
            selector,
            records: Vec::new(),
        }
    }
}

impl<S: Default> Default for Raw<S> {
    fn default() -> Self {
        Self::new(S::default())
    }
}

impl<S> Strategy for Raw<S>
where
    S: Selector<Output = u8>,
{
    type Output = Traversal;

    fn feed(&mut self, candidate: Candidate<'_>) {
        if let Some(alias) = self.selector.select(candidate.clone()) {
            self.records.push(Record {
                alias,
                kind: Kind::of(&candidate),
                layout_bounds: candidate.bounds(),
                visible_bounds: candidate.visible_bounds(),
            });
        }
    }

    fn len(&self) -> usize {
        self.records.len()
    }

    fn is_done(&self) -> bool {
        false
    }

    fn finish(&self) -> Self::Output {
        Traversal {
            records: self.records.clone(),
            visited: 0,
            truncated: false,
        }
    }

    fn finish_with(&self, visited: usize, truncated: bool) -> Self::Output {
        Traversal {
            records: self.records.clone(),
            visited,
            truncated,
        }
    }
}

/// Creates a bounded read-only [`Finder`] that records a [`Record`] for every
/// candidate selected by `selector`.
///
/// The selector returns the alias index (`u8`) of each matched candidate.
pub fn query<S>(selector: S) -> Finder<Raw<S>>
where
    S: Selector<Output = u8>,
{
    Finder::new(Raw::new(selector))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::widget::Id;
    use crate::core::{Point, Size};

    fn any() -> impl Selector<Output = u8> {
        |_: Candidate<'_>| Some(0)
    }

    #[test]
    fn clip_intersects_the_viewport_and_never_leaks_siblings() {
        let mut finder = query(any()).with_viewport(Rectangle::with_size(Size::new(100.0, 100.0)));

        // The parent container, which will restore its viewport when its
        // traversal scope ends.
        finder.container(None, Rectangle::new(Point::ORIGIN, Size::new(100.0, 100.0)));
        finder.traverse(&mut |operation| {
            // A clipped container inside the parent's scope: its children
            // see only the top-left quarter.
            operation.clip(Rectangle::new(Point::ORIGIN, Size::new(50.0, 50.0)));

            let inside = Id::unique();
            operation.container(
                Some(&inside),
                Rectangle::new(Point::new(10.0, 10.0), Size::new(30.0, 30.0)),
            );

            let below = Id::unique();
            operation.container(
                Some(&below),
                Rectangle::new(Point::new(0.0, 60.0), Size::new(10.0, 10.0)),
            );
        });

        // After the parent's scope ends, the clip is gone: a sibling is
        // measured against the full viewport again.
        let sibling = Id::unique();
        finder.container(
            Some(&sibling),
            Rectangle::new(Point::new(60.0, 0.0), Size::new(10.0, 10.0)),
        );

        let outcome = finder.finish();
        let crate::core::widget::operation::Outcome::Some(traversal) = outcome else {
            panic!("the traversal must finish with a report");
        };

        assert!(!traversal.truncated);
        assert_eq!(traversal.visited, 4);
        assert_eq!(traversal.records.len(), 4);

        let record = |index: usize| traversal.records[index];
        assert_eq!(
            record(1).visible_bounds,
            Some(Rectangle::new(
                Point::new(10.0, 10.0),
                Size::new(30.0, 30.0)
            ))
        );
        assert_eq!(record(2).kind, Kind::Container);
        assert_eq!(record(2).visible_bounds, None, "below the clip");
        assert_eq!(
            record(3).visible_bounds,
            Some(Rectangle::new(Point::new(60.0, 0.0), Size::new(10.0, 10.0))),
            "the clip does not leak into the next subtree"
        );
    }

    #[test]
    fn nested_clips_intersect_and_restore() {
        let mut finder = query(any()).with_viewport(Rectangle::with_size(Size::new(100.0, 100.0)));

        // Outer clip 0..80, inner clip 20..100 => 20..80.
        finder.clip(Rectangle::new(Point::ORIGIN, Size::new(80.0, 80.0)));
        finder.traverse(&mut |operation| {
            operation.clip(Rectangle::new(
                Point::new(20.0, 20.0),
                Size::new(80.0, 80.0),
            ));
            let nested = Id::unique();
            operation.container(
                Some(&nested),
                Rectangle::new(Point::ORIGIN, Size::new(100.0, 100.0)),
            );
        });

        // The outer scope is restored: 0..80.
        let after = Id::unique();
        finder.container(
            Some(&after),
            Rectangle::new(Point::new(70.0, 0.0), Size::new(30.0, 10.0)),
        );

        let outcome = finder.finish();
        let crate::core::widget::operation::Outcome::Some(traversal) = outcome else {
            panic!("the traversal must finish with a report");
        };

        assert_eq!(traversal.records.len(), 2);
        assert_eq!(
            traversal.records[0].visible_bounds,
            Some(Rectangle::new(
                Point::new(20.0, 20.0),
                Size::new(60.0, 60.0)
            ))
        );
        assert_eq!(
            traversal.records[1].visible_bounds,
            Some(Rectangle::new(Point::new(70.0, 0.0), Size::new(10.0, 10.0)))
        );
    }

    #[test]
    fn limits_truncate_the_traversal() {
        let mut finder = query(any()).with_limits(Limits::new(3, 2));

        for index in 0..5 {
            let id = Id::unique();
            finder.container(
                Some(&id),
                Rectangle::new(Point::new(index as f32, 0.0), Size::new(1.0, 1.0)),
            );
        }

        let outcome = finder.finish();
        let crate::core::widget::operation::Outcome::Some(traversal) = outcome else {
            panic!("the traversal must finish with a report");
        };

        assert!(traversal.truncated);
        assert_eq!(traversal.records.len(), 2);
        assert_eq!(traversal.visited, 3);
    }
}

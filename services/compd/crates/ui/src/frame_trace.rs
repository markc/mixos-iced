//! One aggregate iced span per output frame, including screen and world UIs.
use std::cell::RefCell;
use std::collections::BTreeSet;

#[derive(Default)]
struct Counts {
    rendered: BTreeSet<u64>,
    unchanged: BTreeSet<u64>,
}

thread_local! {
    static COUNTS: RefCell<Option<Counts>> = const { RefCell::new(None) };
}

pub struct Frame(Option<ledger::frame_trace::Span>);

/// Covers layout/tick and rasterisation of every compd-owned iced surface.
/// detail = surfaces rasterised, aux = clean surfaces reused without drawing.
pub fn frame(subject: u64) -> Frame {
    if !ledger::frame_trace::enabled() { return Frame(None); }
    COUNTS.with_borrow_mut(|counts| *counts = Some(Counts::default()));
    Frame(Some(ledger::frame_trace::span("kms_iced_render", subject)))
}

pub(crate) fn rendered(id: u64) {
    COUNTS.with_borrow_mut(|counts| {
        if let Some(counts) = counts {
            counts.rendered.insert(id);
            counts.unchanged.remove(&id);
        }
    });
}

pub(crate) fn unchanged(id: u64) {
    COUNTS.with_borrow_mut(|counts| {
        if let Some(counts) = counts && !counts.rendered.contains(&id) {
            counts.unchanged.insert(id);
        }
    });
}

impl Drop for Frame {
    fn drop(&mut self) {
        if let Some(mut span) = self.0.take()
            && let Some(counts) = COUNTS.with_borrow_mut(Option::take)
        {
            span.counters(counts.rendered.len() as u64, counts.unchanged.len() as u64);
        }
    }
}

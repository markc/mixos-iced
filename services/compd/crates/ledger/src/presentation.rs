// The feedback ledger, the content-source ledger, the renderer report types
// and the frame_trace discard codes, over engine-neutral types:
// `PresentedFrame` is generic over the engine's output type, `Refresh` and
// the kind flags are local, and `Feedback` names its output type. The
// `impl Feedback` for the Wayland callback and the engine glue (commit
// staging, frame reports, lifecycle discards) stay with the engine; the
// refused-content bookkeeping that glue needs is [`RefusedContent`].

//! `wp_presentation` bookkeeping: feedback taken at commit, resolved when the
//! renderer reports a presented frame, and the in-process content-source
//! ledger that is measured the same way.
//!
//! The ledgers are pure data structures, generic over [`Feedback`], so the
//! resolution rules are tested without a Wayland client. Every feedback that
//! enters a ledger leaves it exactly once, presented or discarded, including
//! when the ledger itself is dropped.
//!
//! Sequencing: a surface's `content_seq` advances only when the protocol
//! thread publishes a new buffer to the renderer, and the renderer reports
//! the `content_seq` each frame actually sampled. A commit's feedback is
//! presented only by a frame that sampled exactly that commit's content;
//! older commits it superseded are discarded.

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use crate::SurfaceId;
use crate::presentation_stats::{
    InputMark, PresentSample, PresentationLeaves, PresentationStats, Ring,
};

/// At most this many commits per surface wait for a presented frame. A
/// client that commits faster than frames are presented loses the oldest
/// (as `discarded`), never memory.
pub const MAX_PENDING_COMMITS: usize = 8;

/// The output's refresh as `wp_presentation` reports it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Refresh {
    #[default]
    Unknown,
    /// Variable refresh, with the minimum interval.
    Variable(Duration),
    Fixed(Duration),
}

impl Refresh {
    /// The fixed refresh in µs. A variable rate has no vblank grid to count
    /// misses against, so it reads as unknown.
    pub fn fixed_us(self) -> Option<u64> {
        match self {
            Self::Fixed(refresh) => u64::try_from(refresh.as_micros()).ok(),
            Self::Unknown | Self::Variable(_) => None,
        }
    }
}

/// `wp_presentation_feedback.kind` bits.
pub mod kind {
    pub const VSYNC: u32 = 0x1;
    pub const HW_CLOCK: u32 = 0x2;
    pub const HW_COMPLETION: u32 = 0x4;
    pub const ZERO_COPY: u32 = 0x8;
}

/// One `wp_presentation_feedback` object, or a test fake. Dropping one
/// without resolving it would leave a client waiting forever, so every
/// path out of the ledger calls exactly one of `presented`/`discarded`.
pub trait Feedback {
    /// The engine's output type (smithay `Output` in compd).
    type Output;
    /// Returns whether the feedback was actually sent as presented (a frame
    /// without a nameable output can only be sent as discarded).
    fn presented(self, frame: &PresentedFrame<Self::Output>) -> bool;
    fn discarded(self);
}

/// What a renderer proved about one presented frame on one output.
#[derive(Clone, Debug)]
pub struct PresentedFrame<O> {
    pub output: Option<O>,
    /// CLOCK_MONOTONIC.
    pub time: Duration,
    pub refresh: Refresh,
    pub seq: u64,
    /// [`kind`] bits.
    pub flags: u32,
}

impl<O> PresentedFrame<O> {
    /// The frame time in CLOCK_MONOTONIC µs (saturating).
    pub fn time_us(&self) -> u64 {
        u64::try_from(self.time.as_micros()).unwrap_or(u64::MAX)
    }
}

/// Per-surface presentation counters (the stats surface builds on these).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PresentationCounters {
    pub presented: u64,
    pub discarded: u64,
    pub last_presented_us: Option<u64>,
}

/// What one ledger operation resolved: `(commit seq, callbacks)`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Resolution {
    pub presented: Option<(u64, usize)>,
    pub discarded: Vec<(u64, usize)>,
}

struct PendingCommit<F> {
    seq: u64,
    callbacks: Vec<F>,
}

/// Feedback waiting for the frame that shows its commit.
pub struct PresentationLedger<F: Feedback> {
    pending: HashMap<SurfaceId, VecDeque<PendingCommit<F>>>,
    counters: HashMap<SurfaceId, PresentationCounters>,
}

impl<F: Feedback> Default for PresentationLedger<F> {
    fn default() -> Self {
        Self {
            pending: HashMap::new(),
            counters: HashMap::new(),
        }
    }
}

fn discard_entry<F: Feedback>(
    entry: PendingCommit<F>,
    counters: &mut PresentationCounters,
    resolution: &mut Resolution,
) {
    let count = entry.callbacks.len();
    for callback in entry.callbacks {
        callback.discarded();
    }
    counters.discarded += count as u64;
    if count > 0 {
        resolution.discarded.push((entry.seq, count));
    }
}

impl<F: Feedback> PresentationLedger<F> {
    /// Record the callbacks of one applied commit. `seq` is the surface's
    /// content sequence after the commit (a commit without a new buffer
    /// carries the sequence of the content it leaves on screen). Returns
    /// what the per-surface cap pushed out.
    pub fn take_on_commit(&mut self, id: SurfaceId, seq: u64, callbacks: Vec<F>) -> Resolution {
        let mut resolution = Resolution::default();
        if callbacks.is_empty() {
            return resolution;
        }
        let queue = self.pending.entry(id).or_default();
        match queue.back_mut() {
            Some(last) if last.seq == seq => last.callbacks.extend(callbacks),
            _ => queue.push_back(PendingCommit { seq, callbacks }),
        }
        let counters = self.counters.entry(id).or_default();
        while queue.len() > MAX_PENDING_COMMITS {
            if let Some(oldest) = queue.pop_front() {
                discard_entry(oldest, counters, &mut resolution);
            }
        }
        resolution
    }

    /// Apply one renderer report for one surface. With `shown`, commits
    /// below `commit_seq` were superseded before any frame showed them and
    /// are discarded, and the commit at exactly `commit_seq` is presented.
    /// Without it, everything at or below `commit_seq` was not seen.
    /// Commits above `commit_seq` keep waiting.
    pub fn resolve(
        &mut self,
        id: SurfaceId,
        commit_seq: u64,
        shown: bool,
        frame: &PresentedFrame<F::Output>,
    ) -> Resolution {
        let mut resolution = Resolution::default();
        let Some(queue) = self.pending.get_mut(&id) else {
            return resolution;
        };
        let ready = queue
            .iter()
            .take_while(|entry| entry.seq <= commit_seq)
            .count();
        if ready == 0 {
            return resolution;
        }
        let resolved = queue.drain(..ready).collect::<Vec<_>>();
        if queue.is_empty() {
            self.pending.remove(&id);
        }
        let counters = self.counters.entry(id).or_default();
        for entry in resolved {
            if !(shown && entry.seq == commit_seq) {
                discard_entry(entry, counters, &mut resolution);
                continue;
            }
            let count = entry.callbacks.len();
            let mut sent = 0;
            for callback in entry.callbacks {
                if callback.presented(frame) {
                    sent += 1;
                }
            }
            counters.presented += sent as u64;
            counters.discarded += (count - sent) as u64;
            if sent > 0 {
                counters.last_presented_us = u64::try_from(frame.time.as_micros()).ok();
                resolution.presented = Some((entry.seq, sent));
            }
            if sent < count {
                resolution.discarded.push((entry.seq, count - sent));
            }
        }
        resolution
    }

    /// Commits in `from..=to` will never be shown (a refused buffer, the
    /// bufferless commits that inherited its sequence, and requests it
    /// superseded while they were pending).
    pub fn discard_range(&mut self, id: SurfaceId, from: u64, to: u64) -> Resolution {
        let mut resolution = Resolution::default();
        let Some(queue) = self.pending.get_mut(&id) else {
            return resolution;
        };
        let counters = self.counters.entry(id).or_default();
        let (refused, kept): (VecDeque<_>, VecDeque<_>) = std::mem::take(queue)
            .into_iter()
            .partition(|entry| (from..=to).contains(&entry.seq));
        *queue = kept;
        for entry in refused {
            discard_entry(entry, counters, &mut resolution);
        }
        if queue.is_empty() {
            self.pending.remove(&id);
        }
        resolution
    }

    /// Unmap, minimise, destroy, role change: nothing pending for this
    /// surface can be shown as the content it was committed as.
    pub fn discard_surface(&mut self, id: SurfaceId) -> Resolution {
        let mut resolution = Resolution::default();
        let Some(queue) = self.pending.remove(&id) else {
            return resolution;
        };
        let counters = self.counters.entry(id).or_default();
        for entry in queue {
            discard_entry(entry, counters, &mut resolution);
        }
        resolution
    }

    /// The surface is gone for good: drop its counters too.
    pub fn forget_counters(&mut self, id: SurfaceId) {
        self.counters.remove(&id);
    }

    pub fn pending_surfaces(&self) -> Vec<SurfaceId> {
        self.pending.keys().copied().collect()
    }

    pub fn pending_count(&self, id: SurfaceId) -> usize {
        self.pending.get(&id).map_or(0, |queue| {
            queue.iter().map(|entry| entry.callbacks.len()).sum()
        })
    }

    pub fn counters(&self, id: SurfaceId) -> PresentationCounters {
        self.counters.get(&id).copied().unwrap_or_default()
    }
}

impl<F: Feedback> Drop for PresentationLedger<F> {
    fn drop(&mut self) {
        for (_, queue) in self.pending.drain() {
            for entry in queue {
                for callback in entry.callbacks {
                    callback.discarded();
                }
            }
        }
    }
}

/// Per surface, the newest content sequence the renderer refused. A
/// bufferless commit that still carries it can never be shown.
#[derive(Clone, Debug, Default)]
pub struct RefusedContent(HashMap<SurfaceId, u64>);

impl RefusedContent {
    /// The renderer refused content `seq` of `id`; `sampled` is the newest
    /// sequence it did sample, if any. Everything after `sampled` up to
    /// `seq` will never be shown.
    pub fn note_refused<F: Feedback>(
        &mut self,
        ledger: &mut PresentationLedger<F>,
        id: SurfaceId,
        sampled: Option<u64>,
        seq: u64,
    ) -> Resolution {
        let from = sampled.map_or(seq, |sampled| sampled.saturating_add(1));
        let resolution = ledger.discard_range(id, from, seq);
        trace_discards(id, &resolution, DiscardReason::Refused);
        let refused = self.0.entry(id).or_default();
        *refused = (*refused).max(seq);
        resolution
    }

    /// Whether feedback committed on content `seq` of `id` can still be
    /// presented: not when it sits on refused content. Newer content clears
    /// the refusal.
    pub fn admits(&mut self, id: SurfaceId, seq: u64) -> bool {
        match self.0.get(&id) {
            Some(refused) if *refused == seq => false,
            Some(refused) if *refused < seq => {
                self.0.remove(&id);
                true
            }
            _ => true,
        }
    }

    pub fn forget(&mut self, id: SurfaceId) {
        self.0.remove(&id);
    }
}

/// One content source's state in a renderer report.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FrameSource {
    pub id: String,
    pub revision: u64,
    pub shown: bool,
    pub upload_bytes: u64,
    pub damage_px: u64,
    pub consumed_input: Option<u64>,
    /// When the newest revision was taken (CLOCK_MONOTONIC µs), if this
    /// report carries a new one.
    pub revised_us: Option<u64>,
    /// When the oldest revision since the previous report was taken.
    pub first_revised_us: Option<u64>,
}

impl FrameSource {
    /// Forget the costs a report already carried.
    pub fn clear_costs(&mut self) {
        self.upload_bytes = 0;
        self.damage_px = 0;
        self.consumed_input = None;
        self.revised_us = None;
        self.first_revised_us = None;
    }
}

/// One client surface's state in a renderer report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameSurface {
    pub id: SurfaceId,
    /// With `shown`: the content sequence this frame sampled. Without it:
    /// everything at or below this sequence was not seen.
    pub commit_seq: u64,
    /// Visible, on the output, and sampling that commit's content.
    pub shown: bool,
    /// Not shown only because the newest content is not sampled yet (the
    /// surface is visible): the feedback ledger treats it as not shown, the
    /// statistics as a stall rather than a hide.
    pub waiting: bool,
}

/// What one presented frame contained.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FrameContent {
    pub surfaces: Vec<FrameSurface>,
    pub sources: Vec<FrameSource>,
}

/// Accounting for one registered content source.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceCounters {
    pub registration: u64,
    /// The output the plugin asked to be measured on (`None` = any).
    pub output: Option<String>,
    pub registered_at_us: u64,
    pub revision: u64,
    pub last_presented_revision: u64,
    pub stats: PresentationStats,
    pub upload_bytes_total: u64,
    pub damage_px_total: u64,
    /// Per reported frame, shown or not: the work was done either way.
    pub upload_bytes: Ring,
    pub damage_px: Ring,
    pub frames: u64,
    /// When the oldest revision not yet presented was written.
    pending_since_us: Option<u64>,
}

impl SourceCounters {
    fn reset(&mut self, now_us: u64) {
        *self = Self {
            registration: self.registration,
            output: self.output.take(),
            registered_at_us: self.registered_at_us,
            revision: self.revision,
            last_presented_revision: self.last_presented_revision,
            stats: PresentationStats::new(now_us),
            ..Self::default()
        };
    }

    /// The `sources.<id>.presentation.*` leaves.
    pub fn leaves(&self) -> SourcePresentationLeaves {
        let upload = self.upload_bytes.summary();
        let damage = self.damage_px.summary();
        SourcePresentationLeaves {
            common: self.stats.leaves(),
            upload_bytes_total: self.upload_bytes_total,
            damage_px_total: self.damage_px_total,
            upload_bytes_p50: upload.p50,
            upload_bytes_p99: upload.p99,
            damage_px_p50: damage.p50,
            damage_px_p99: damage.p99,
        }
    }

    /// The `comp.window.stats {source}` rings.
    pub fn samples(&self, count: usize) -> serde_json::Value {
        let mut samples = self.stats.samples(count);
        samples["upload_bytes"] = serde_json::json!(self.upload_bytes.newest(count));
        samples["damage_px"] = serde_json::json!(self.damage_px.newest(count));
        samples
    }
}

/// `sources.<id>.presentation.*`: the window leaves plus the costs.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct SourcePresentationLeaves {
    #[serde(flatten)]
    pub common: PresentationLeaves,
    pub upload_bytes_total: u64,
    pub damage_px_total: u64,
    pub upload_bytes_p50: Option<u64>,
    pub upload_bytes_p99: Option<u64>,
    pub damage_px_p50: Option<u64>,
    pub damage_px_p99: Option<u64>,
}

/// Content sources have no protocol callbacks; a revision counter stands in
/// for commits and the same presented/discarded rules apply.
#[derive(Default)]
pub struct SourceLedger {
    sources: HashMap<String, SourceCounters>,
    /// One counter for every registration of any id: a registration number
    /// never repeats, so it fences a stale request without per-id history.
    registrations: u64,
}

impl SourceLedger {
    /// Returns the new registration number.
    pub fn register(&mut self, id: &str, output: Option<String>, now_us: u64) -> u64 {
        self.registrations += 1;
        self.sources.insert(
            id.to_string(),
            SourceCounters {
                registration: self.registrations,
                output,
                registered_at_us: now_us,
                stats: PresentationStats::new(now_us),
                ..SourceCounters::default()
            },
        );
        self.registrations
    }

    /// `revision` is the newest revision the plugin wrote, reported or not;
    /// every revision after the last presented one counts as discarded.
    pub fn unregister(&mut self, id: &str, revision: u64) -> Option<SourceCounters> {
        let mut counters = self.sources.remove(id)?;
        counters.revision = counters.revision.max(revision);
        // Revisions are assumed to be +1 per update (not checked). No frame
        // time: an unregistration is not a frame report.
        counters.stats.record_discarded(
            counters
                .revision
                .saturating_sub(counters.last_presented_revision),
            None,
        );
        Some(counters)
    }

    /// One reported frame. `input_mark` finds when an injected input was
    /// delivered, for the update that says it answers it.
    pub fn resolve(
        &mut self,
        source: &FrameSource,
        tv_us: u64,
        refresh_us: Option<u64>,
        input_mark: impl Fn(u64, u64) -> Option<InputMark>,
    ) {
        let Some(counters) = self.sources.get_mut(&source.id) else {
            return;
        };
        counters.frames = counters.frames.saturating_add(1);
        counters.upload_bytes_total = counters
            .upload_bytes_total
            .saturating_add(source.upload_bytes);
        counters.damage_px_total = counters.damage_px_total.saturating_add(source.damage_px);
        counters.upload_bytes.push(source.upload_bytes);
        counters.damage_px.push(source.damage_px);
        counters.revision = counters.revision.max(source.revision);
        let unpresented = source.revision > counters.last_presented_revision;
        if unpresented && let Some(first) = source.first_revised_us {
            counters.pending_since_us = Some(
                counters
                    .pending_since_us
                    .map_or(first, |since| since.min(first)),
            );
        }
        if source.shown && unpresented {
            counters.stats.record_present(PresentSample {
                tv_us,
                refresh_us,
                discarded: source.revision - counters.last_presented_revision - 1,
                committed_us: source.revised_us,
                pending_since_us: counters.pending_since_us.take(),
                answered_input_us: source
                    .consumed_input
                    .and_then(|input_seq| input_mark(input_seq, tv_us))
                    .map(|mark| mark.injected_at_us),
            });
            counters.last_presented_revision = source.revision;
        } else if !source.shown {
            counters.stats.hidden();
        }
    }

    pub fn get(&self, id: &str) -> Option<&SourceCounters> {
        self.sources.get(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &SourceCounters)> {
        self.sources.iter()
    }

    pub fn reset(&mut self, id: &str, now_us: u64) -> bool {
        self.sources
            .get_mut(id)
            .map(|counters| counters.reset(now_us))
            .is_some()
    }

    pub fn reset_all(&mut self, now_us: u64) {
        for counters in self.sources.values_mut() {
            counters.reset(now_us);
        }
    }

    pub fn len(&self) -> usize {
        self.sources.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }
}

/// `frame_trace` reason codes (the `aux` field of
/// `comp_presentation_discarded`).
#[derive(Clone, Copy, Debug)]
pub enum DiscardReason {
    Unmap = 1,
    Minimize = 2,
    Destroy = 3,
    Role = 4,
    Superseded = 5,
    NotPresentable = 6,
    Refused = 7,
    Overflow = 8,
    NoFrame = 9,
    /// The surface left the current workspace (a switch or a move).
    Workspace = 10,
}

/// One `comp_presentation_discarded` trace event per discarded commit.
pub fn trace_discards(id: SurfaceId, resolution: &Resolution, reason: DiscardReason) {
    for (seq, _) in &resolution.discarded {
        crate::frame_trace::event("comp_presentation_discarded", || {
            (id.0, *seq, reason as u64)
        });
    }
}

#[cfg(test)]
#[path = "presentation_tests.rs"]
mod ledger_tests;

// The trace event it emits uses the frame_trace record format.

//! Why compd drew a frame. Every redraw request names a [`RedrawReason`];
//! the [`FrameLedger`] counts requests per reason and attributes each
//! rendered frame to the reasons that were pending on that pipe when it
//! rendered. A frame nobody asked for is *unattributed*: on a static screen
//! the ledger should show zero frames, and any that remain name their cause
//! (or, unattributed, a self-scheduling path still to be found).
//!
//! The ledger mirrors the scheduler's `Schedule`: a request is global (every pipe goes
//! stale at once), a render is per pipe. Pipes are keyed by the output key the
//! scheduler already uses; a pipe exists from the first report on it.

use std::collections::BTreeMap;

use serde::Serialize;

/// Why a redraw was requested. The variants cover the `Schedule::request*`
/// call sites and the protocol handlers that redraw unconditionally; a new
/// caller picks the closest one or adds its own, never `Unattributed`.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum RedrawReason {
    /// A client committed new content (wl_surface.commit with damage).
    Commit,
    /// A surface mapped.
    Map,
    /// A surface unmapped or was destroyed.
    Unmap,
    /// A configure / window-state change (maximise, fullscreen, resize, move).
    WindowState,
    /// Stacking changed (raise, band).
    Stack,
    /// Keyboard focus or activation changed (decorations follow focus).
    Focus,
    /// Pointer motion or cursor image change.
    Cursor,
    /// Any other human input event that changes what is drawn.
    Input,
    /// Injected agent input (`comp.input.*`).
    AgentInput,
    /// A workspace switch or a window moved between workspaces.
    Workspace,
    /// An output appeared, left, or changed mode, scale or transform.
    Output,
    /// Layer-shell surface or exclusive-zone change.
    Layer,
    /// A popup opened, moved or closed.
    Popup,
    /// A compositor animation (camera ease, window open/close effect).
    Animation,
    /// A renderer-neutral effect asked for its next frame.
    Effect,
    /// In-compositor iced UI (furniture, overlays) is dirty or animating.
    Iced,
    /// Screencopy or capture needs a fresh frame.
    Capture,
    /// Session lock state or lock-screen fade.
    Lock,
    /// A `comp.*` Bus control changed the scene.
    Bus,
    /// A Mix Scenes request (`shell.scene.*`) changed what a scene draws.
    Publish,
    /// The session resumed (VT switch back, DPMS on, GPU reset).
    Resume,
    /// A pipe's very first frame (no request can precede it).
    FirstFrame,
    /// An idle rescue: the loop forced a redraw to recover from a stall.
    Rescue,
    /// A watchdog or pacing-floor poll forced a redraw.
    Watchdog,
    /// The post-activation settle redraws.
    Settle,
    /// An unconditional re-arm from inside a render (to be removed).
    Rearm,
    /// Background or wallpaper animation.
    Background,
    /// A request through the scheduler's reason-less entry points
    /// (`Schedule::request`, `request_silent`, `force`): a call site that has
    /// not been migrated to name its reason yet. Not the same as an
    /// unattributed FRAME (one no request asked for at all), which the
    /// snapshot counts as `unattributed_frames`.
    Unattributed,
}

impl RedrawReason {
    pub const ALL: [Self; 28] = [
        Self::Commit,
        Self::Map,
        Self::Unmap,
        Self::WindowState,
        Self::Stack,
        Self::Focus,
        Self::Cursor,
        Self::Input,
        Self::AgentInput,
        Self::Workspace,
        Self::Output,
        Self::Layer,
        Self::Popup,
        Self::Animation,
        Self::Effect,
        Self::Iced,
        Self::Capture,
        Self::Lock,
        Self::Bus,
        Self::Publish,
        Self::Resume,
        Self::FirstFrame,
        Self::Rescue,
        Self::Watchdog,
        Self::Settle,
        Self::Rearm,
        Self::Background,
        Self::Unattributed,
    ];
    pub const COUNT: usize = Self::ALL.len();

    pub const fn index(self) -> usize {
        self as usize
    }

    const fn bit(self) -> u64 {
        1 << self.index()
    }

    /// The ledger key (`frames.reasons.<name>`).
    pub const fn name(self) -> &'static str {
        match self {
            Self::Commit => "commit",
            Self::Map => "map",
            Self::Unmap => "unmap",
            Self::WindowState => "window_state",
            Self::Stack => "stack",
            Self::Focus => "focus",
            Self::Cursor => "cursor",
            Self::Input => "input",
            Self::AgentInput => "agent_input",
            Self::Workspace => "workspace",
            Self::Output => "output",
            Self::Layer => "layer",
            Self::Popup => "popup",
            Self::Animation => "animation",
            Self::Effect => "effect",
            Self::Iced => "iced",
            Self::Capture => "capture",
            Self::Lock => "lock",
            Self::Bus => "bus",
            Self::Publish => "publish",
            Self::Resume => "resume",
            Self::FirstFrame => "first_frame",
            Self::Rescue => "rescue",
            Self::Watchdog => "watchdog",
            Self::Settle => "settle",
            Self::Rearm => "rearm",
            Self::Background => "background",
            Self::Unattributed => "unattributed",
        }
    }

    /// Reasons that exist to keep the loop turning rather than because
    /// something changed: on a static screen these must count zero.
    pub const fn is_continuation(self) -> bool {
        matches!(
            self,
            Self::Rescue | Self::Watchdog | Self::Settle | Self::Rearm | Self::Background
        )
    }
}

// Every reason fits the pending bitmask.
const _: () = assert!(RedrawReason::COUNT <= 64);

/// How the request reached the scheduler (`Schedule::request`,
/// `request_silent`, `force`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum RequestKind {
    /// Wakes the loop iff a pipe can act on it now.
    Wake = 0,
    /// No wake: the caller runs the executor itself or is inside a render.
    Silent = 1,
    /// Wakes unconditionally.
    Force = 2,
}

/// A set of reasons (the reasons a frame answered).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReasonSet(u64);

impl ReasonSet {
    pub fn contains(self, reason: RedrawReason) -> bool {
        self.0 & reason.bit() != 0
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn insert(&mut self, reason: RedrawReason) {
        self.0 |= reason.bit();
    }

    pub fn iter(self) -> impl Iterator<Item = RedrawReason> {
        RedrawReason::ALL
            .into_iter()
            .filter(move |reason| self.contains(*reason))
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Counts {
    requests: u64,
    silent: u64,
    forced: u64,
    frames: u64,
}

#[derive(Clone, Debug)]
struct Pipe {
    key: String,
    pending: ReasonSet,
    frames: u64,
    empty_frames: u64,
}

/// Per-reason redraw accounting.
#[derive(Clone, Debug)]
pub struct FrameLedger {
    counts: [Counts; RedrawReason::COUNT],
    pipes: Vec<Pipe>,
    frames: u64,
    empty_frames: u64,
    unattributed_frames: u64,
    since_us: u64,
}

impl FrameLedger {
    pub fn new(since_us: u64) -> Self {
        Self {
            counts: [Counts::default(); RedrawReason::COUNT],
            pipes: Vec::new(),
            frames: 0,
            empty_frames: 0,
            unattributed_frames: 0,
            since_us,
        }
    }

    fn pipe(&mut self, key: &str) -> &mut Pipe {
        let index = match self.pipes.iter().position(|pipe| pipe.key == key) {
            Some(index) => index,
            None => {
                // A pipe's first report: nothing could have asked for it yet.
                let mut pending = ReasonSet::default();
                pending.insert(RedrawReason::FirstFrame);
                self.pipes.push(Pipe {
                    key: key.to_string(),
                    pending,
                    frames: 0,
                    empty_frames: 0,
                });
                self.pipes.len() - 1
            }
        };
        &mut self.pipes[index]
    }

    /// A redraw request. Global, like the scheduler's: every known pipe
    /// owes a frame for `reason`.
    pub fn note_request(&mut self, reason: RedrawReason, kind: RequestKind) {
        let counts = &mut self.counts[reason.index()];
        counts.requests = counts.requests.saturating_add(1);
        match kind {
            RequestKind::Wake => {}
            RequestKind::Silent => counts.silent = counts.silent.saturating_add(1),
            RequestKind::Force => counts.forced = counts.forced.saturating_add(1),
        }
        for pipe in &mut self.pipes {
            pipe.pending.insert(reason);
        }
        crate::frame_trace::event("comp_redraw_request", || {
            (reason.index() as u64, kind as u64, 0)
        });
    }

    /// A redraw request for ONE pipe (compd: `Schedule::request_pipe_silent`):
    /// counted like any request, but owed only by `pipe`.
    pub fn note_pipe_request(&mut self, pipe: &str, reason: RedrawReason, kind: RequestKind) {
        let counts = &mut self.counts[reason.index()];
        counts.requests = counts.requests.saturating_add(1);
        match kind {
            RequestKind::Wake => {}
            RequestKind::Silent => counts.silent = counts.silent.saturating_add(1),
            RequestKind::Force => counts.forced = counts.forced.saturating_add(1),
        }
        self.pipe(pipe).pending.insert(reason);
        crate::frame_trace::event("comp_redraw_request", || {
            (reason.index() as u64, kind as u64, 0)
        });
    }

    /// `pipe` rendered a frame; `empty` when it carried no damage (a frame
    /// that should not have been drawn). Returns the reasons it answered,
    /// which are no longer pending on that pipe; an empty set is an
    /// unattributed frame.
    pub fn note_frame(&mut self, pipe: &str, empty: bool) -> ReasonSet {
        let entry = self.pipe(pipe);
        let reasons = std::mem::take(&mut entry.pending);
        entry.frames = entry.frames.saturating_add(1);
        if empty {
            entry.empty_frames = entry.empty_frames.saturating_add(1);
        }
        self.frames = self.frames.saturating_add(1);
        if empty {
            self.empty_frames = self.empty_frames.saturating_add(1);
        }
        if reasons.is_empty() {
            self.unattributed_frames = self.unattributed_frames.saturating_add(1);
        }
        for reason in reasons.iter() {
            let counts = &mut self.counts[reason.index()];
            counts.frames = counts.frames.saturating_add(1);
        }
        crate::frame_trace::event("comp_frame_reasons", || {
            (reasons.0, u64::from(empty), 0)
        });
        reasons
    }

    /// The pipe is gone (pruned output); a later report recreates it.
    pub fn remove_pipe(&mut self, pipe: &str) {
        self.pipes.retain(|entry| entry.key != pipe);
    }

    /// The reasons `pipe` still owes a frame for.
    pub fn pending(&self, pipe: &str) -> ReasonSet {
        self.pipes
            .iter()
            .find(|entry| entry.key == pipe)
            .map_or_else(ReasonSet::default, |entry| entry.pending)
    }

    /// Zero every counter; counting restarts at `now_us`. Pending reasons
    /// stay pending: a reset does not answer a request.
    pub fn reset(&mut self, now_us: u64) {
        self.counts = [Counts::default(); RedrawReason::COUNT];
        self.frames = 0;
        self.empty_frames = 0;
        self.unattributed_frames = 0;
        self.since_us = now_us;
        for pipe in &mut self.pipes {
            pipe.frames = 0;
            pipe.empty_frames = 0;
        }
    }

    pub fn snapshot(&self) -> FrameLedgerSnapshot {
        FrameLedgerSnapshot {
            since_us: self.since_us,
            frames: self.frames,
            empty_frames: self.empty_frames,
            unattributed_frames: self.unattributed_frames,
            continuation_frames: RedrawReason::ALL
                .iter()
                .filter(|reason| reason.is_continuation())
                .map(|reason| self.counts[reason.index()].frames)
                .fold(0u64, u64::saturating_add),
            reasons: RedrawReason::ALL
                .iter()
                .map(|reason| {
                    let counts = self.counts[reason.index()];
                    (
                        reason.name(),
                        ReasonCounts {
                            requests: counts.requests,
                            silent: counts.silent,
                            forced: counts.forced,
                            frames: counts.frames,
                        },
                    )
                })
                .collect(),
            pipes: self
                .pipes
                .iter()
                .map(|pipe| {
                    (
                        pipe.key.clone(),
                        PipeCounts {
                            frames: pipe.frames,
                            empty_frames: pipe.empty_frames,
                            pending: pipe.pending.iter().map(RedrawReason::name).collect(),
                        },
                    )
                })
                .collect(),
        }
    }
}

/// One reason's counters.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct ReasonCounts {
    pub requests: u64,
    pub silent: u64,
    pub forced: u64,
    /// Frames rendered while this reason was pending (a frame answering
    /// several reasons counts once for each).
    pub frames: u64,
}

/// One pipe's counters.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct PipeCounts {
    pub frames: u64,
    pub empty_frames: u64,
    pub pending: Vec<&'static str>,
}

/// A consistent copy of the ledger (the future `frames.*` props subtree).
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct FrameLedgerSnapshot {
    pub since_us: u64,
    pub frames: u64,
    pub empty_frames: u64,
    /// Frames no request asked for.
    pub unattributed_frames: u64,
    /// Frames answering a continuation reason (rescue, watchdog, settle,
    /// re-arm, background): zero on a static screen once the self-scheduling
    /// paths are gone.
    pub continuation_frames: u64,
    pub reasons: BTreeMap<&'static str, ReasonCounts>,
    pub pipes: BTreeMap<String, PipeCounts>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasons_have_unique_indices_and_names() {
        for (index, reason) in RedrawReason::ALL.iter().enumerate() {
            assert_eq!(reason.index(), index, "{reason:?} is in declaration order");
        }
        let names = RedrawReason::ALL
            .iter()
            .map(|reason| reason.name())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(names.len(), RedrawReason::COUNT);
        assert!(names.iter().all(|name| name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')));
    }

    #[test]
    fn a_frame_answers_every_reason_pending_on_its_pipe_once() {
        let mut ledger = FrameLedger::new(10);
        // First frames: nothing could have asked for them.
        assert_eq!(
            ledger.note_frame("o_dp_1", false).iter().collect::<Vec<_>>(),
            [RedrawReason::FirstFrame]
        );
        ledger.note_frame("o_dp_2", false);
        ledger.note_request(RedrawReason::Commit, RequestKind::Wake);
        ledger.note_request(RedrawReason::Cursor, RequestKind::Wake);
        ledger.note_request(RedrawReason::Commit, RequestKind::Silent);
        let answered = ledger.note_frame("o_dp_1", false);
        assert_eq!(
            answered.iter().collect::<Vec<_>>(),
            [RedrawReason::Commit, RedrawReason::Cursor]
        );
        assert!(ledger.note_frame("o_dp_1", true).is_empty(), "answered once");
        // The other pipe still owes the same frame.
        assert!(ledger.pending("o_dp_2").contains(RedrawReason::Commit));
        ledger.note_frame("o_dp_2", false);
        let snapshot = ledger.snapshot();
        assert_eq!(snapshot.since_us, 10);
        assert_eq!(snapshot.frames, 5);
        assert_eq!(snapshot.empty_frames, 1);
        assert_eq!(snapshot.unattributed_frames, 1);
        assert_eq!(
            snapshot.reasons["commit"],
            ReasonCounts {
                requests: 2,
                silent: 1,
                forced: 0,
                frames: 2,
            }
        );
        assert_eq!(snapshot.reasons["cursor"].frames, 2);
        assert_eq!(snapshot.reasons["first_frame"].frames, 2);
        assert_eq!(snapshot.reasons.len(), RedrawReason::COUNT, "every reason is listed");
        assert_eq!(snapshot.pipes["o_dp_1"].frames, 3);
        assert_eq!(snapshot.pipes["o_dp_1"].empty_frames, 1);
        assert!(snapshot.pipes["o_dp_2"].pending.is_empty());
    }

    #[test]
    fn continuation_frames_are_counted_apart() {
        let mut ledger = FrameLedger::new(0);
        ledger.note_frame("o_a", false);
        ledger.note_request(RedrawReason::Rearm, RequestKind::Force);
        ledger.note_frame("o_a", true);
        ledger.note_request(RedrawReason::Watchdog, RequestKind::Force);
        ledger.note_request(RedrawReason::Commit, RequestKind::Wake);
        ledger.note_frame("o_a", false);
        let snapshot = ledger.snapshot();
        assert_eq!(snapshot.continuation_frames, 2);
        assert_eq!(snapshot.reasons["rearm"].forced, 1);
        assert!(RedrawReason::Rescue.is_continuation());
        assert!(!RedrawReason::Commit.is_continuation());
    }

    #[test]
    fn reset_zeroes_counts_but_keeps_what_is_still_owed() {
        let mut ledger = FrameLedger::new(0);
        ledger.note_frame("o_a", false);
        ledger.note_request(RedrawReason::Output, RequestKind::Wake);
        ledger.reset(500);
        let snapshot = ledger.snapshot();
        assert_eq!((snapshot.since_us, snapshot.frames), (500, 0));
        assert_eq!(snapshot.reasons["output"], ReasonCounts::default());
        assert_eq!(snapshot.pipes["o_a"].pending, ["output"]);
        assert!(ledger.note_frame("o_a", false).contains(RedrawReason::Output));
        ledger.remove_pipe("o_a");
        assert!(ledger.pending("o_a").is_empty());
        assert!(
            ledger.note_frame("o_a", false).contains(RedrawReason::FirstFrame),
            "a pruned pipe comes back as new"
        );
    }

    #[test]
    fn snapshot_serialises_with_named_reasons() {
        let mut ledger = FrameLedger::new(0);
        ledger.note_frame("o_a", false);
        let json = serde_json::to_value(ledger.snapshot()).unwrap();
        assert_eq!(json["reasons"]["first_frame"]["frames"], 1);
        assert_eq!(json["pipes"]["o_a"]["frames"], 1);
        assert_eq!(json["unattributed_frames"], 0);
    }
}

//! Per-pipe redraw scheduling: what each output has been asked to render, what
//! it has rendered, and whether a flip of its own is in flight.
//!
//! A redraw REQUEST is global — every monitor renders the same world, so a commit
//! or an input event makes every pipe stale at once. Everything after that is per
//! pipe: a pipe renders when it lags the request epoch, is skipped while its flip
//! is in flight (its own vblank re-renders it), and the loop is woken only when
//! some pipe is idle and stale. There is no latch: a ping is a stale signal by
//! nature ("a request happened since the last drain"), so the executor decides
//! from the epoch alone whether a wake has work behind it. The single-`bool`
//! latch and in-flight gate this replaces were written for one output and could
//! not say WHICH pipe a wake was for.
//!
//! Incremental by design: nothing is registered. A pipe exists from the first
//! time the kernel reports on it, an unknown pipe reads as stale and idle, and
//! `clear` is safe at any point — the price is at most one redundant render per
//! pipe. Keyed by the output key the kernel already uses (`tearing.liveness`
//! keys by it too); this layer knows nothing about CRTCs.
//!
//! Every request names a [`RedrawReason`] and
//! every frame a backend puts out is reported through [`Schedule::frame`], so the
//! [`FrameLedger`] can say why each frame was drawn. With `COMPD_FRAME_TRACE=1`
//! the ledger writes `comp_redraw_request` and `comp_frame_reasons` records to
//! the frame-trace sink (`COMPD_FRAME_TRACE_FILE`), which is what the idle probe
//! counts. The reason-less entry points stay for call sites not migrated yet and
//! record [`RedrawReason::Unattributed`].

use ledger::frame_trace::monotonic_us;
use ledger::redraw::{FrameLedger, ReasonSet, RequestKind};
pub use ledger::redraw::RedrawReason;
use smithay::reexports::calloop::ping::Ping;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::calloop::{LoopHandle, RegistrationToken};
use std::time::Instant;

/// `flips` counts real page-flip completions ([`Schedule::vblank`]): per-pipe
/// proof of progress for the stall rescue and the settle deadline, which a
/// sibling pipe's flips must not stand in for.
struct Pipe { key: String, rendered: u64, in_flight: bool, flips: u64 }

/// A frame wanted at an instant rather than now: one
/// one-shot timer per kind, keeping the earliest. `None` is a bare wake for a
/// request already pending; `Some(reason)` is a new request when it fires.
struct Deadline { kind: Option<RedrawReason>, at: Instant, token: RegistrationToken }

/// `rendering_now` / `deferred_wake`: while a backend renders, a request (from a
/// continuation source called inside the render) moves the epoch but does NOT
/// ping — see [`Schedule::begin_render`].
pub struct Schedule {
    epoch: u64,
    pipes: Vec<Pipe>,
    ping: Option<Ping>,
    ledger: FrameLedger,
    deadlines: Vec<Deadline>,
    rendering_now: bool,
    deferred_wake: bool,
}

impl Schedule {
    /// Epoch 1 with every pipe first seen at 0: the first render is always due.
    pub fn new() -> Self {
        Self {
            epoch: 1,
            pipes: Vec::new(),
            ping: None,
            ledger: FrameLedger::new(monotonic_us()),
            deadlines: Vec::new(),
            rendering_now: false,
            deferred_wake: false,
        }
    }

    /// A backend starts rendering (round-1 finding B). Until [`Self::end_render`],
    /// requests still move the epoch and reach the ledger, but their wake is
    /// HELD: a source that asks for a frame from inside every render (capture
    /// polls, effects, iced) would otherwise ping at once and, when the frame
    /// comes out empty, render again at CPU rate with no vblank to pace it. The
    /// backend decides after the render: wake now when a flip is coming or a
    /// sibling pipe can act, or `wake_at` the estimated vblank when the frame was
    /// empty.
    pub fn begin_render(&mut self) {
        self.rendering_now = true;
        self.deferred_wake = false;
    }

    /// The render is over. Returns whether a wake was held during it.
    pub fn end_render(&mut self) -> bool {
        self.rendering_now = false;
        std::mem::take(&mut self.deferred_wake)
    }

    /// Register a pipe that has just come live (round-1 finding E) so that
    /// [`Self::pending`] sees it — a pipe the schedule has never heard of is
    /// invisible to `pending`, and a forced ping would find nothing to do.
    pub fn register(&mut self, key: &str) { self.entry(key); }

    /// A real page flip completed on `key`: not in flight, and one more flip of
    /// progress ([`Self::flips`]).
    pub fn vblank(&mut self, key: &str) {
        let pipe = self.entry(key);
        pipe.in_flight = false;
        pipe.flips = pipe.flips.wrapping_add(1);
    }

    /// Page flips completed on `key`; `None` for a pipe the schedule does not know
    /// (never seen, or removed).
    pub fn flips(&self, key: &str) -> Option<u64> { self.get(key).map(|p| p.flips) }

    /// Request a frame for `reason` at `at` — a producer that knows WHEN it next
    /// needs one (an iced animation's `RedrawRequest::At`) instead of asking
    /// every frame until then. One timer per reason: an earlier `at` replaces a
    /// later one, a later one is dropped. `schedule` finds this `Schedule` in the
    /// loop data, where the timer callback runs.
    pub fn request_at<D: 'static>(
        &mut self,
        handle: &LoopHandle<'static, D>,
        at: Instant,
        reason: RedrawReason,
        schedule: fn(&mut D) -> &mut Schedule,
    ) {
        self.arm_deadline(handle, at, Some(reason), schedule);
    }

    /// Wake the loop at `at` for a request that is ALREADY pending: the
    /// continuation of an empty frame that left its pipe behind the epoch (a
    /// silent request made during the render). One refresh later rather than
    /// now, so a source that asks every frame cannot spin the loop.
    pub fn wake_at<D: 'static>(
        &mut self,
        handle: &LoopHandle<'static, D>,
        at: Instant,
        schedule: fn(&mut D) -> &mut Schedule,
    ) {
        self.arm_deadline(handle, at, None, schedule);
    }

    /// Whether a deadline of this kind is armed (`None`: the wake).
    pub fn deadline_armed(&self, kind: Option<RedrawReason>) -> Option<Instant> {
        self.deadlines.iter().find(|d| d.kind == kind).map(|d| d.at)
    }

    fn arm_deadline<D: 'static>(
        &mut self,
        handle: &LoopHandle<'static, D>,
        at: Instant,
        kind: Option<RedrawReason>,
        schedule: fn(&mut D) -> &mut Schedule,
    ) {
        if let Some(i) = self.deadlines.iter().position(|d| d.kind == kind) {
            if self.deadlines[i].at <= at {
                return;
            }
            let old = self.deadlines.remove(i);
            handle.remove(old.token);
        }
        // A concrete deadline is owed work, including iced/model changes on a
        // static desktop. It must survive exclusive pacing: the paced client
        // may never commit again. Only continuous frame-driven producers use
        // request_gated; this one-shot retires after delivering its request.
        let token = handle.insert_source(Timer::from_deadline(at), move |_, _, data: &mut D| {
            let s = schedule(data);
            s.deadlines.retain(|d| d.kind != kind);
            match kind {
                Some(reason) => s.request_for(reason),
                None => s.wake(),
            }
            TimeoutAction::Drop
        });
        match token {
            Ok(token) => self.deadlines.push(Deadline { kind, at, token }),
            // Without the timer the frame would never come: ask now instead.
            Err(_) => match kind {
                Some(reason) => self.request_for(reason),
                None => self.wake(),
            },
        }
    }
    pub fn set_ping(&mut self, ping: Ping) { self.ping = Some(ping); }
    pub fn epoch(&self) -> u64 { self.epoch }

    /// Forget a pruned pipe — optional hygiene, a stale entry never blocks anything.
    pub fn remove(&mut self, key: &str) {
        self.pipes.retain(|p| p.key != key);
        self.ledger.remove_pipe(key);
    }
    /// Forget everything: every pipe is unknown again, i.e. stale and idle. The
    /// ledger keeps its pipes: a clear does not answer what they owe.
    pub fn clear(&mut self) { self.pipes.clear(); }

    /// A redraw request: every pipe is stale. Wakes the loop iff a pipe can act on
    /// it now; the rest are in flight and their own vblank renders them. (No pipes
    /// known yet — boot — wakes too: the first render is what creates them.)
    pub fn request_for(&mut self, reason: RedrawReason) {
        self.ledger.note_request(reason, RequestKind::Wake);
        self.epoch = self.epoch.wrapping_add(1);
        if self.pipes.is_empty() || self.pipes.iter().any(|p| !p.in_flight) {
            self.wake();
        }
    }
    /// [`Self::request_for`] unless exclusive pacing holds the cadence (the gate
    /// `Dispatch::schedule_redraw` applies), for backend call sites that do not
    /// go through `Dispatch`.
    pub fn request_gated(&mut self, reason: RedrawReason) {
        if crate::tearing::gate::gate::engaged() { return; }
        self.request_for(reason);
    }
    /// A request without a wake: for callers that run the executor themselves,
    /// and for continuation sources called from INSIDE a render (the parallax),
    /// whose pipe's vblank finds the epoch.
    pub fn request_silent_for(&mut self, reason: RedrawReason) {
        self.ledger.note_request(reason, RequestKind::Silent);
        self.epoch = self.epoch.wrapping_add(1);
    }
    /// A request owed by ONE pipe, without a wake (round-2 finding 4): leaves
    /// every other pipe current. For work a pipe owes itself that its own vblank
    /// path renders — the connector setup frame after its first flip. A global
    /// epoch move there would leave idle siblings behind with nothing to wake
    /// them.
    pub fn request_pipe_silent(&mut self, key: &str, reason: RedrawReason) {
        self.ledger.note_pipe_request(key, reason, RequestKind::Silent);
        let epoch = self.epoch;
        let pipe = self.entry(key);
        if pipe.rendered == epoch {
            pipe.rendered = epoch.wrapping_sub(1);
        }
    }
    /// A request that wakes unconditionally — rescues and re-arms, where an idle
    /// cycle must restart whatever the bookkeeping says.
    pub fn force_for(&mut self, reason: RedrawReason) {
        self.ledger.note_request(reason, RequestKind::Force);
        self.epoch = self.epoch.wrapping_add(1);
        self.wake();
    }
    /// Wake the loop without asking for anything: a deferred render (the rate cap)
    /// resuming a request that is already pending. No epoch move, no ledger entry.
    /// Held while a render is in progress ([`Self::begin_render`]).
    pub fn wake(&mut self) {
        if self.rendering_now {
            self.deferred_wake = true;
            return;
        }
        if let Some(p) = &self.ping { p.ping(); }
    }

    /// Reason-less forms, kept for call sites that have not named a reason yet.
    pub fn request(&mut self) { self.request_for(RedrawReason::Unattributed); }
    pub fn request_silent(&mut self) { self.request_silent_for(RedrawReason::Unattributed); }
    pub fn force(&mut self) { self.force_for(RedrawReason::Unattributed); }

    /// Is there a pipe a wake can render right now — idle and behind the epoch?
    pub fn pending(&self) -> bool {
        self.pipes.iter().any(|p| !p.in_flight && p.rendered != self.epoch)
    }
    /// Unknown pipes need rendering: a first frame is never gated on bookkeeping.
    pub fn needs(&self, key: &str) -> bool {
        self.get(key).is_none_or(|p| p.rendered != self.epoch)
    }
    pub fn in_flight(&self, key: &str) -> bool { self.get(key).is_some_and(|p| p.in_flight) }

    /// A render begins: the pipe is current as of NOW. A request that arrives
    /// while it renders moves the epoch past this stamp and is serviced next time.
    pub fn rendering(&mut self, key: &str) { let e = self.epoch; self.entry(key).rendered = e; }
    /// The frame was queued; only this pipe's own vblank ends the flight.
    pub fn queued(&mut self, key: &str) { self.entry(key).in_flight = true; }
    /// The flip completed — or never can (rebuilt pipe, session resume).
    pub fn completed(&mut self, key: &str) { self.entry(key).in_flight = false; }

    /// `key` put a frame out: submitted to the display (`empty == false`), or
    /// rendered with nothing to submit (`empty == true`). Returns the reasons
    /// it answered; empty means nobody asked for it.
    pub fn frame(&mut self, key: &str, empty: bool) -> ReasonSet { self.ledger.note_frame(key, empty) }
    /// The per-reason frame ledger.
    pub fn ledger(&self) -> &FrameLedger { &self.ledger }
    pub fn ledger_mut(&mut self) -> &mut FrameLedger { &mut self.ledger }

    fn get(&self, key: &str) -> Option<&Pipe> { self.pipes.iter().find(|p| p.key == key) }
    /// Upsert: the first report on a pipe is what creates it.
    fn entry(&mut self, key: &str) -> &mut Pipe {
        let i = match self.pipes.iter().position(|p| p.key == key) {
            Some(i) => i,
            None => {
                self.pipes.push(Pipe { key: key.to_string(), rendered: 0, in_flight: false, flips: 0 });
                self.pipes.len() - 1
            }
        };
        &mut self.pipes[i]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::reexports::calloop::EventLoop;
    use std::time::Duration;

    struct Data { schedule: Schedule }
    fn schedule_of(data: &mut Data) -> &mut Schedule { &mut data.schedule }

    #[test]
    fn reasons_reach_the_ledger_and_frames_answer_them() {
        let mut s = Schedule::new();
        s.frame("o", false); // the pipe's first frame creates it in the ledger
        s.request_for(RedrawReason::Commit);
        s.request(); // reason-less: recorded as Unattributed
        let answered = s.frame("o", false);
        assert!(answered.contains(RedrawReason::Commit));
        assert!(answered.contains(RedrawReason::Unattributed));
        assert!(s.frame("o", true).is_empty(), "a frame nobody asked for is unattributed");
        assert_eq!(s.ledger().snapshot().unattributed_frames, 1);
    }

    #[test]
    fn request_at_fires_once_and_keeps_the_earliest() {
        let mut event_loop: EventLoop<'static, Data> = EventLoop::try_new().unwrap();
        let handle = event_loop.handle();
        let mut data = Data { schedule: Schedule::new() };
        let epoch = data.schedule.epoch();
        let now = Instant::now();
        data.schedule.request_at(&handle, now + Duration::from_millis(40), RedrawReason::Iced, schedule_of);
        // Earlier replaces later; a later one after that is dropped.
        data.schedule.request_at(&handle, now + Duration::from_millis(5), RedrawReason::Iced, schedule_of);
        data.schedule.request_at(&handle, now + Duration::from_millis(30), RedrawReason::Iced, schedule_of);
        assert_eq!(
            data.schedule.deadline_armed(Some(RedrawReason::Iced)),
            Some(now + Duration::from_millis(5))
        );
        let until = Instant::now() + Duration::from_millis(100);
        while Instant::now() < until {
            event_loop.dispatch(Some(Duration::from_millis(10)), &mut data).unwrap();
        }
        assert_eq!(data.schedule.epoch(), epoch + 1, "exactly one request");
        assert_eq!(data.schedule.ledger().snapshot().reasons["iced"].requests, 1);
        assert_eq!(data.schedule.deadline_armed(Some(RedrawReason::Iced)), None);
    }

    /// Data with a counted ping, so a test can see whether a request woke the loop.
    struct Pinged { schedule: Schedule, pings: u32 }

    fn pinged_loop() -> (EventLoop<'static, Pinged>, Pinged) {
        let event_loop: EventLoop<'static, Pinged> = EventLoop::try_new().unwrap();
        let (ping, source) = smithay::reexports::calloop::ping::make_ping().unwrap();
        event_loop
            .handle()
            .insert_source(source, |_, _, data: &mut Pinged| data.pings += 1)
            .unwrap();
        let mut schedule = Schedule::new();
        schedule.set_ping(ping);
        (event_loop, Pinged { schedule, pings: 0 })
    }

    fn settle(event_loop: &mut EventLoop<'static, Pinged>, data: &mut Pinged) {
        for _ in 0..3 {
            event_loop.dispatch(Some(Duration::from_millis(5)), data).unwrap();
        }
    }

    /// Finding B: a request from inside a render moves the epoch but holds its
    /// wake for the backend to decide on.
    #[test]
    fn requests_during_a_render_hold_their_wake() {
        let (mut event_loop, mut data) = pinged_loop();
        data.schedule.begin_render();
        let epoch = data.schedule.epoch();
        data.schedule.request_for(RedrawReason::Capture);
        data.schedule.force_for(RedrawReason::Effect);
        assert_eq!(data.schedule.epoch(), epoch + 2, "the requests still count");
        settle(&mut event_loop, &mut data);
        assert_eq!(data.pings, 0, "no wake while rendering");
        assert!(data.schedule.end_render(), "the held wake is reported");
        assert!(!data.schedule.end_render(), "and reported once");
        // Outside a render a request wakes as before.
        data.schedule.request_for(RedrawReason::Commit);
        settle(&mut event_loop, &mut data);
        assert_eq!(data.pings, 1);
    }

    /// Finding E: a freshly registered pipe is pending, so a forced ping has
    /// something to render even while every other pipe is in flight.
    #[test]
    fn a_registered_pipe_is_pending_beside_one_in_flight() {
        let mut s = Schedule::new();
        s.rendering("primary");
        s.queued("primary");
        assert!(!s.pending(), "the only known pipe is in flight");
        s.register("secondary");
        assert!(s.pending(), "the new pipe owes its first frame");
        assert!(s.needs("secondary"));
    }

    /// Finding A: flips are counted per pipe and only by a real vblank.
    #[test]
    fn flips_are_per_pipe_and_vblank_only() {
        let mut s = Schedule::new();
        assert_eq!(s.flips("a"), None);
        s.queued("a");
        s.queued("b");
        s.vblank("b");
        assert_eq!(s.flips("a"), Some(0), "a sibling's flip is not this pipe's");
        assert_eq!(s.flips("b"), Some(1));
        assert!(s.in_flight("a"));
        s.completed("a"); // a rebuilt pipe: no longer in flight, but no flip
        assert!(!s.in_flight("a"));
        assert_eq!(s.flips("a"), Some(0));
    }

    /// Round-2 finding 4: a per-pipe request owes only that pipe.
    #[test]
    fn a_pipe_request_leaves_its_siblings_current() {
        let mut s = Schedule::new();
        s.rendering("a");
        s.rendering("b");
        s.frame("a", false);
        s.frame("b", false);
        let epoch = s.epoch();
        s.request_pipe_silent("a", RedrawReason::Output);
        assert_eq!(s.epoch(), epoch, "no global epoch move");
        assert!(s.needs("a"));
        assert!(!s.needs("b"), "the sibling is not left behind");
        assert!(s.ledger().pending("a").contains(RedrawReason::Output));
        assert!(!s.ledger().pending("b").contains(RedrawReason::Output));
        s.rendering("a");
        assert!(!s.needs("a"), "rendering answers it");
    }

    #[test]
    fn wake_at_moves_no_epoch() {
        let mut event_loop: EventLoop<'static, Data> = EventLoop::try_new().unwrap();
        let handle = event_loop.handle();
        let mut data = Data { schedule: Schedule::new() };
        let epoch = data.schedule.epoch();
        data.schedule.wake_at(&handle, Instant::now() + Duration::from_millis(5), schedule_of);
        let until = Instant::now() + Duration::from_millis(50);
        while Instant::now() < until {
            event_loop.dispatch(Some(Duration::from_millis(10)), &mut data).unwrap();
        }
        assert_eq!(data.schedule.epoch(), epoch, "a wake answers an existing request");
        assert_eq!(data.schedule.deadline_armed(None), None);
    }

    #[test]
    fn content_changes_during_a_pending_flip_render_on_its_completion() {
        let (mut event_loop, mut data) = pinged_loop();
        data.schedule.rendering("kms");
        data.schedule.queued("kms");
        // Submission reports the first frame and registers the ledger pipe,
        // just as the native backend does before waiting for its page flip.
        assert!(data.schedule.frame("kms", false).contains(RedrawReason::FirstFrame));
        for reason in [RedrawReason::Publish, RedrawReason::Workspace, RedrawReason::Iced] {
            data.schedule.request_for(reason);
        }
        settle(&mut event_loop, &mut data);
        assert_eq!(data.pings, 0, "the pending flip is the wake-up");
        assert!(data.schedule.needs("kms"));
        assert!(!data.schedule.pending(), "the pipe cannot render until its flip completes");
        for reason in [RedrawReason::Publish, RedrawReason::Workspace, RedrawReason::Iced] {
            assert!(data.schedule.ledger().pending("kms").contains(reason));
        }
        data.schedule.vblank("kms");
        assert!(data.schedule.pending(), "render immediately, without another vblank");
        data.schedule.rendering("kms");
        let reasons = data.schedule.frame("kms", false);
        for reason in [RedrawReason::Publish, RedrawReason::Workspace, RedrawReason::Iced] {
            assert!(reasons.contains(reason));
        }
        assert!(!data.schedule.needs("kms"));
        assert!(data.schedule.ledger().pending("kms").is_empty(), "the next frame answers the reasons");
    }
}

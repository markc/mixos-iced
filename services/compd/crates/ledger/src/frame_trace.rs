// The record line format and the COMPD_FRAME_TRACE* environment names are
// fixed so the existing trace readers keep working; the `component=comp`
// field is the trace's component name, not the binary's.

//! Opt-in, bounded frame diagnostics. No frame-thread I/O or blocking sends.
use std::{
    io::Write,
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
        mpsc::{SyncSender, sync_channel},
    },
};

const LIMIT: u64 = 65_536;
struct Recorder {
    sender: SyncSender<Record>,
    sequence: AtomicU64,
    dropped: AtomicU64,
    // Runtime record ceiling. 0 == unbounded (file-sink capture); otherwise the
    // recorder stops after `limit` records and emits a single `trace_limit`
    // sentinel, so a stderr sink can never be flooded without bound.
    limit: u64,
}
struct Record {
    sequence: u64,
    stage: &'static str,
    subject: u64,
    detail: u64,
    aux: u64,
    tid: u32,
    start_us: u64,
    end_us: u64,
    cpu_us: u64,
    dropped: u64,
}
static RECORDER: OnceLock<Option<Recorder>> = OnceLock::new();

/// CLOCK_MONOTONIC in µs, the clock trace records and presentation times
/// share.
pub fn monotonic_us() -> u64 {
    clock_us(libc::CLOCK_MONOTONIC)
}

fn clock_us(clock: libc::clockid_t) -> u64 {
    let mut value = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: value points to a valid, writable timespec.
    if unsafe { libc::clock_gettime(clock, &mut value) } != 0 {
        return 0;
    }
    (value.tv_sec as u64)
        .saturating_mul(1_000_000)
        .saturating_add(value.tv_nsec as u64 / 1_000)
}

/// Resolve the record ceiling. `COMPD_FRAME_TRACE_LIMIT` overrides the default
/// (0 == unbounded, for a file sink capturing a long session); an unset or
/// unparsable value keeps the stderr-safe default so the fast path is unchanged.
fn resolve_limit() -> u64 {
    match std::env::var("COMPD_FRAME_TRACE_LIMIT") {
        Ok(raw) => raw.trim().parse::<u64>().unwrap_or(LIMIT),
        Err(_) => LIMIT,
    }
}

/// A line sink for the trace thread: a file when `COMPD_FRAME_TRACE_FILE` names
/// a writable path, else stderr. The file is truncated on open so each capture
/// starts clean, and a path that cannot be opened falls back to stderr rather
/// than losing the trace.
enum Sink {
    File(std::fs::File),
    Stderr(std::io::Stderr),
}

impl Sink {
    fn open() -> Self {
        if let Ok(path) = std::env::var("COMPD_FRAME_TRACE_FILE")
            && !path.is_empty()
        {
            match std::fs::File::create(&path) {
                Ok(file) => return Sink::File(file),
                Err(error) => {
                    eprintln!("FRAME_TRACE sink open failed path={path} error={error}");
                }
            }
        }
        Sink::Stderr(std::io::stderr())
    }

    fn write_line(&mut self, line: &str) {
        match self {
            Sink::File(file) => {
                let _ = file.write_all(line.as_bytes());
            }
            Sink::Stderr(stderr) => {
                let _ = write!(stderr.lock(), "{line}");
            }
        }
    }
}

fn recorder() -> Option<&'static Recorder> {
    RECORDER.get_or_init(|| {
        if std::env::var("COMPD_FRAME_TRACE").as_deref() != Ok("1") { return None; }
        let limit = resolve_limit();
        // A file sink can absorb a long capture, so it gets a deeper backlog;
        // stderr keeps the tight channel that never lets tracing dominate a run.
        let file_sink = std::env::var("COMPD_FRAME_TRACE_FILE").map(|p| !p.is_empty()).unwrap_or(false);
        let depth = if file_sink { 65_536 } else { 4096 };
        let (sender, receiver) = sync_channel::<Record>(depth);
        std::thread::Builder::new().name("frame-trace".into()).spawn(move || {
            let mut sink = Sink::open();
            for r in receiver {
                sink.write_line(&format!(
                    "FRAME_TRACE component=comp pid={} sequence={} stage={} subject={} detail={} aux={} tid={} start_us={} end_us={} duration_us={} cpu_us={} dropped={} limit={}\n",
                    std::process::id(), r.sequence, r.stage, r.subject, r.detail, r.aux, r.tid, r.start_us,
                    r.end_us, r.end_us.saturating_sub(r.start_us), r.cpu_us, r.dropped, limit));
            }
        }).ok()?;
        Some(Recorder { sender, sequence: AtomicU64::new(0), dropped: AtomicU64::new(0), limit })
    }).as_ref()
}

/// A timed span: records its stage when dropped. Inert when tracing is off.
pub struct Span(Option<(&'static Recorder, Record, u64)>);

/// A submitted frame waiting for a kernel event. Cancellation (pause, output
/// rebuild or watchdog recovery) must not masquerade as a completed flip.
pub struct WaitSpan(Option<Span>);

pub fn wait_span(stage: &'static str, subject: u64) -> WaitSpan {
    WaitSpan(Some(span(stage, subject)))
}

impl WaitSpan {
    pub fn finish(mut self) {
        if let Some(span) = self.0.take() { span.finish_wait(); }
    }
}

impl Drop for WaitSpan {
    fn drop(&mut self) {
        if let Some(span) = self.0.as_mut() { span.0 = None; }
    }
}

/// Also usable on the protocol thread before render observer installation.
pub fn enabled() -> bool {
    recorder().is_some()
}

fn thread_id() -> u32 {
    // SAFETY: gettid has no arguments or pointers and cannot change state.
    unsafe { libc::gettid() as u32 }
}

/// A protocol observation, not a duration or proof of client receipt. Resolve
/// identities only when tracing is enabled and still within the record budget.
pub fn event(stage: &'static str, fields: impl FnOnce() -> (u64, u64, u64)) {
    if let Some(recorder) = recorder() {
        event_to(recorder, stage, fields);
    }
}

fn event_to(recorder: &Recorder, stage: &'static str, fields: impl FnOnce() -> (u64, u64, u64)) {
    let sequence = recorder.sequence.fetch_add(1, Ordering::Relaxed);
    let bounded = recorder.limit != 0;
    if bounded && sequence > recorder.limit {
        return;
    }
    let at_limit = bounded && sequence == recorder.limit;
    let (subject, detail, aux) = if at_limit { (0, 0, 0) } else { fields() };
    let now = clock_us(libc::CLOCK_MONOTONIC);
    send_record(
        recorder,
        Record {
            sequence,
            stage: if at_limit { "trace_limit" } else { stage },
            subject,
            detail,
            aux,
            tid: thread_id(),
            start_us: now,
            end_us: now,
            cpu_us: 0,
            dropped: 0,
        },
    );
}

fn send_record(recorder: &Recorder, mut record: Record) {
    record.dropped = recorder.dropped.load(Ordering::Relaxed);
    if recorder.sender.try_send(record).is_err() {
        recorder.dropped.fetch_add(1, Ordering::Relaxed);
    }
}

pub fn span(stage: &'static str, subject: u64) -> Span {
    span_with_detail(stage, subject, 0)
}

pub fn span_with_detail(stage: &'static str, subject: u64, detail: u64) -> Span {
    let Some(recorder) = recorder() else {
        return Span(None);
    };
    let sequence = recorder.sequence.fetch_add(1, Ordering::Relaxed);
    let bounded = recorder.limit != 0;
    if bounded && sequence > recorder.limit {
        return Span(None);
    }
    let stage = if bounded && sequence == recorder.limit {
        "trace_limit"
    } else {
        stage
    };
    Span(Some((
        recorder,
        Record {
            sequence,
            stage,
            subject,
            detail,
            aux: 0,
            tid: thread_id(),
            start_us: clock_us(libc::CLOCK_MONOTONIC),
            end_us: 0,
            cpu_us: 0,
            dropped: 0,
        },
        clock_us(libc::CLOCK_THREAD_CPUTIME_ID),
    )))
}

impl Span {
    /// Fill counters discovered during the measured work.
    pub fn counters(&mut self, detail: u64, aux: u64) {
        if let Some((_, record, _)) = self.0.as_mut() {
            record.detail = detail;
            record.aux = aux;
        }
    }

    /// Complete a wait across event-loop callbacks. Thread CPU time during
    /// the wait belongs to other work, not to this submitted frame.
    pub fn finish_wait(mut self) {
        if let Some((recorder, mut record, _)) = self.0.take() {
            record.end_us = monotonic_us();
            record.cpu_us = 0;
            send_record(recorder, record);
        }
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        let Some((recorder, mut record, cpu_start)) = self.0.take() else {
            return;
        };
        record.end_us = clock_us(libc::CLOCK_MONOTONIC);
        record.cpu_us = clock_us(libc::CLOCK_THREAD_CPUTIME_ID).saturating_sub(cpu_start);
        send_record(recorder, record);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_events_are_bounded_and_keep_identity_and_thread() {
        let (sender, receiver) = sync_channel(1);
        let recorder = Recorder {
            sender,
            sequence: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            limit: LIMIT,
        };
        event_to(&recorder, "test_release", || (17, 23, 2));
        // A full queue drops instead of blocking the protocol thread.
        event_to(&recorder, "test_full", || (0, 0, 0));
        assert_eq!(recorder.dropped.load(Ordering::Relaxed), 1);
        let record = receiver.try_recv().unwrap();
        assert_eq!((record.subject, record.detail, record.aux), (17, 23, 2));
        assert_eq!(record.tid, thread_id());
        assert_eq!(record.start_us, record.end_us);
        assert!(record.start_us > 0);
        assert_eq!(record.cpu_us, 0);
        recorder.sequence.store(LIMIT, Ordering::Relaxed);
        event_to(&recorder, "test_limit", || {
            panic!("limit must not resolve identities")
        });
        assert_eq!(receiver.try_recv().unwrap().stage, "trace_limit");
        event_to(&recorder, "test_past_limit", || {
            panic!("exhausted recorder must not resolve identities")
        });
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn unbounded_limit_keeps_recording_past_the_default_cap() {
        // A file-sink capture (limit == 0) must never stop or emit the sentinel,
        // even after passing the bounded default — otherwise a long mouse-move
        // trace would silently truncate exactly where the interesting data is.
        let (sender, receiver) = sync_channel(4);
        let recorder = Recorder {
            sender,
            sequence: AtomicU64::new(LIMIT),
            dropped: AtomicU64::new(0),
            limit: 0,
        };
        event_to(&recorder, "past_cap", || (9, 8, 7));
        let record = receiver.try_recv().unwrap();
        assert_eq!(record.sequence, LIMIT);
        assert_eq!(record.stage, "past_cap");
        assert_eq!((record.subject, record.detail, record.aux), (9, 8, 7));
    }

    // The clock the ledgers share never runs backwards.
    #[test]
    fn monotonic_clock_is_nonzero_and_never_decreases() {
        let first = monotonic_us();
        let second = monotonic_us();
        assert!(first > 0);
        assert!(second >= first);
    }
}

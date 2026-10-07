// SPDX-License-Identifier: MIT OR Apache-2.0
//! `<AppDirs ced>/state/session.json` (ced E1 plan §4.7) — the format is
//! frozen in Stage S; load/save land in Stage E1d. Written atomically (temp,
//! fsync, rename) 1 s after the last change (armed by the change) and on exit.

use serde::{Deserialize, Serialize};

pub const VERSION: u32 = 1;
/// At most this many recent files are kept.
pub const RECENT_MAX: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub version: u32,
    /// Index into `tabs`.
    pub active: Option<usize>,
    pub tabs: Vec<SessionTab>,
    pub recent: Vec<String>,
}

/// A path tab reattaches by `path`; a scratch tab by `recovery_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionTab {
    pub path: Option<String>,
    pub recovery_id: Option<String>,
    /// View byte offset of the caret.
    pub caret: usize,
    /// 1-based first visible line.
    pub first_line: usize,
}

impl Default for Session {
    fn default() -> Self {
        Self {
            version: VERSION,
            active: None,
            tabs: Vec::new(),
            recent: Vec::new(),
        }
    }
}

/// Read the session; a missing, unreadable, malformed or newer-version file
/// is an empty session (ced never refuses to start over its own state).
pub fn load(path: &std::path::Path) -> Session {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Session::default();
    };
    match serde_json::from_str::<Session>(&text) {
        Ok(mut s) if s.version == VERSION => {
            s.recent.truncate(RECENT_MAX);
            if s.active.is_some_and(|a| a >= s.tabs.len()) {
                s.active = None;
            }
            s
        }
        _ => Session::default(),
    }
}

/// Write atomically: a sibling temp file, fsync, rename over, fsync the
/// directory. The parent directory is created if missing.
pub fn save(path: &std::path::Path, session: &Session) -> std::io::Result<()> {
    use std::io::Write;
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("session path has no parent"))?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".session.json.{}.tmp", std::process::id()));
    let body = serde_json::to_vec_pretty(session).map_err(std::io::Error::other)?;
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&body)?;
        f.sync_all()?;
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::File::open(dir)?.sync_all()
}

/// Session writes off the UI thread: [`save`] fsyncs twice (~5 ms, over the
/// frame budget — the nested gate's slow `bus.timer` updates were this).
/// One writer thread, in order; a burst collapses to its newest session.
/// [`SessionWriter::flush`] waits for the last write (exit).
pub struct SessionWriter {
    tx: Option<std::sync::mpsc::Sender<(std::path::PathBuf, Session)>>,
    thread: Option<std::thread::JoinHandle<()>>,
    completion: std::sync::mpsc::Receiver<bool>,
}

impl Default for SessionWriter {
    fn default() -> Self {
        Self::spawn()
    }
}

impl SessionWriter {
    pub fn spawn() -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<(std::path::PathBuf, Session)>();
        let (completed, completion) = std::sync::mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("ced-session".into())
            .spawn(move || {
                let mut saved = true;
                while let Ok(mut job) = rx.recv() {
                    while let Ok(newer) = rx.try_recv() {
                        job = newer;
                    }
                    if let Err(e) = save(&job.0, &job.1) {
                        saved = false;
                        tracing::warn!("ced: saving {}: {e}", job.0.display());
                    } else {
                        saved = true;
                    }
                }
                let _ = completed.send(saved);
            })
            .ok();
        Self {
            tx: thread.as_ref().map(|_| tx),
            thread,
            completion,
        }
    }

    /// Queue a write (synchronous if the thread could not start).
    pub fn save(&self, path: std::path::PathBuf, session: Session) {
        match &self.tx {
            Some(tx) => {
                let _ = tx.send((path, session));
            }
            None => {
                if let Err(e) = save(&path, &session) {
                    tracing::warn!("ced: saving {}: {e}", path.display());
                }
            }
        }
    }

    /// GUI queueing never falls back to filesystem work on the event loop.
    pub fn queue(&self, path: std::path::PathBuf, session: Session) -> Result<(), String> {
        self.tx
            .as_ref()
            .ok_or("session writer unavailable")?
            .send((path, session))
            .map_err(|_| "session writer stopped".to_owned())
    }

    /// Wait until every queued write is on disk; later saves write inline.
    pub fn flush(&mut self) {
        self.tx = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }

    /// Drain on the host's existing worker, with a bounded receipt. Closing
    /// the queue lets its one writer finish; timeout detaches that writer and
    /// never claims its queued state was persisted.
    pub fn flush_for(&mut self, budget: std::time::Duration) -> bool {
        self.tx = None;
        let completed = self.completion.recv_timeout(budget).unwrap_or(false);
        if completed {
            if let Some(thread) = self.thread.take() {
                return thread.join().is_ok();
            }
        }
        self.thread.take();
        completed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_writer_receipt_reports_success_failure_and_unavailable_queue() {
        let directory = tempfile::tempdir().unwrap();
        let mut writer = SessionWriter::spawn();
        let path = directory.path().join("session.json");
        writer.queue(path.clone(), Session::default()).unwrap();
        assert!(writer.flush_for(std::time::Duration::from_secs(2)));
        assert!(path.is_file());
        assert!(writer.queue(path.clone(), Session::default()).is_err());
        let mut writer = SessionWriter::spawn();
        let obstruction = directory.path().join("file");
        std::fs::write(&obstruction, "obstruction").unwrap();
        writer
            .queue(obstruction.join("session.json"), Session::default())
            .unwrap();
        assert!(!writer.flush_for(std::time::Duration::from_secs(2)));
        assert_eq!(std::fs::read_to_string(obstruction).unwrap(), "obstruction");
    }

    #[test]
    fn the_writer_keeps_the_newest_session_and_flushes() {
        let dir = std::env::temp_dir().join(format!("ced-session-writer-{}", std::process::id()));
        let path = dir.join("session.json");
        let mut w = SessionWriter::spawn();
        for k in 0..50 {
            let s = Session {
                recent: vec![format!("/f{k}")],
                ..Session::default()
            };
            w.save(path.clone(), s);
        }
        w.flush();
        assert_eq!(
            load(&path).recent,
            vec!["/f49".to_string()],
            "the last save wins, and flush waited for it"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_frozen_example_parses() {
        let s: Session = serde_json::from_str(
            r#"{"version":1,"active":1,"tabs":[{"path":"/home/u/x.mix","recovery_id":null,"caret":1204,"first_line":40},{"path":null,"recovery_id":"5f0c2a9e1b7d4c33","caret":0,"first_line":1}],"recent":["/home/u/x.mix"]}"#,
        )
        .unwrap();
        assert_eq!(s.tabs.len(), 2);
        assert_eq!(
            serde_json::from_str::<Session>(&serde_json::to_string(&s).unwrap()).unwrap(),
            s
        );
    }

    #[test]
    fn save_then_load_round_trips_and_bad_files_are_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/session.json");
        assert_eq!(load(&path), Session::default());
        let s = Session {
            version: VERSION,
            active: Some(0),
            tabs: vec![SessionTab {
                path: Some("/tmp/x.mix".into()),
                recovery_id: None,
                caret: 3,
                first_line: 1,
            }],
            recent: vec!["/tmp/x.mix".into()],
        };
        save(&path, &s).unwrap();
        assert_eq!(load(&path), s);
        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(load(&path), Session::default());
        std::fs::write(
            &path,
            r#"{"version":99,"active":null,"tabs":[],"recent":[]}"#,
        )
        .unwrap();
        assert_eq!(load(&path), Session::default());
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap()).unwrap().collect();
        assert_eq!(leftovers.len(), 1, "no temp file left behind");
    }
}

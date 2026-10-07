// SPDX-License-Identifier: MIT OR Apache-2.0
//! Cache state belongs to the host's existing serial blocking lane.
use super::{Diagnostic, Running};
use settings::cache::{Save, Target, WriteOutcome, Writer};
use std::path::PathBuf;

pub(super) struct Result {
    pub writer: Option<Writer>,
    pub outcome: std::result::Result<WriteOutcome, Diagnostic>,
}

pub(super) struct Lane {
    directory: PathBuf,
    target: Option<Target>,
    writer: Option<Writer>,
    last: Option<Save>,
    queued: Option<Save>,
    retry: u64,
    #[cfg(test)]
    write_gate: Option<WriteGate>,
}

#[cfg(test)]
struct WriteGate {
    entered: tokio::sync::oneshot::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
}

impl Lane {
    pub fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            target: None,
            writer: None,
            last: None,
            queued: None,
            retry: 0,
            #[cfg(test)]
            write_gate: None,
        }
    }

    /// Hold one physical task after opening the real writer. Dropping the
    /// release sender also unblocks it, including during a test panic.
    #[cfg(test)]
    pub fn hold_next_write(&mut self) -> (tokio::sync::oneshot::Receiver<()>, std::sync::mpsc::Sender<()>) {
        assert!(self.write_gate.is_none());
        let (entered, notification) = tokio::sync::oneshot::channel();
        let (release, gate) = std::sync::mpsc::channel();
        self.write_gate = Some(WriteGate { entered, release: gate });
        (notification, release)
    }

    pub fn replace(&mut self, target: &Target, save: Option<&Save>, retry: u64) {
        if self.target.as_ref() != Some(target) {
            self.writer = None;
            self.last = None;
            self.queued = None;
            self.retry = retry;
        }
        self.target = Some(target.clone());
        if let Some(save) = save
            && (self.retry != retry
                || self
                    .last
                    .as_ref()
                    .is_none_or(|last| !last.same_capture(save)))
        {
            self.last = Some(save.clone());
            self.queued = Some(save.clone());
        }
        self.retry = retry;
    }

    pub fn loader(&self) -> Option<(PathBuf, Target)> {
        Some((self.directory.clone(), self.target.clone()?))
    }

    pub fn start<T, C>(&mut self) -> Option<Running<T, C>> {
        let save = self.queued.take()?;
        let target = self.target.clone().expect("captured target");
        let writer = self.writer.take();
        let directory = self.directory.clone();
        let capture = save.clone();
        let open_target = target.clone();
        #[cfg(test)]
        let write_gate = self.write_gate.take();
        let task = tokio::task::spawn_blocking(move || {
            // Provision only on the blocking lane, through the shared
            // no-follow filesystem owner. A read never creates paths.
            let mut writer = match writer {
                Some(writer) => writer,
                None => match config::atomic::create_directory(&directory)
                    .map_err(|error| {
                        Diagnostic::new("cache_provision_failed", "cache", error.to_string())
                    })
                    .and_then(|_| Writer::open_for(&directory, &open_target))
                {
                    Ok(writer) => writer,
                    Err(error) => {
                        return Result {
                            writer: None,
                            outcome: Err(error),
                        };
                    }
                },
            };
            #[cfg(test)]
            if let Some(gate) = write_gate {
                let _ = gate.entered.send(());
                let _ = gate.release.recv();
            }
            let outcome = writer.write(&save);
            // Keep the lock and attempted-serial fence even on an ambiguous
            // or failed write. Only producer retirement releases them.
            Result {
                writer: Some(writer),
                outcome,
            }
        });
        Some(Running::Save {
            save: capture,
            target,
            task,
        })
    }

    pub fn finish(&mut self, target: &Target, writer: Option<Writer>) {
        if self.target.as_ref() == Some(target) {
            self.writer = writer;
        }
    }

    pub fn pending(&self) -> bool {
        self.queued.is_some()
    }
}

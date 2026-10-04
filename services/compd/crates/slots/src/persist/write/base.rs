use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::thread::JoinHandle;
use uuid::Uuid;

/// A write request handed to the worker. Fully owned (no borrows), so the frame
/// thread is free the instant it sends one. `world`+`key` identify the ledger slot.
pub struct WriteJob {
    pub world: Uuid,
    pub key: &'static str,
    pub path: PathBuf,
    pub bytes: Vec<u8>,
    pub epoch: u64,
}

/// The worker's reply, drained on the frame thread to advance the ledger.
pub struct WriteDone {
    pub world: Uuid,
    pub key: &'static str,
    pub epoch: u64,
    pub ok: bool,
}

/// Write `bytes` to `path` atomically: create the dir, write a sibling `.tmp`,
/// fsync it, rename over the target, then fsync the directory so the rename is
/// durable. A reader never observes a partial file.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = tmp_sibling(path);
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    if let Some(dir) = path.parent() {
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}

fn tmp_sibling(path: &Path) -> PathBuf {
    let mut tmp = path.to_path_buf();
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".tmp");
    tmp.set_file_name(name);
    tmp
}

/// Spawn the long-lived persistence writer. It serializes one write at a time
/// (the frame thread already coalesces per key) and reports each result back.
pub fn spawn_worker(jobs: Receiver<WriteJob>, done: Sender<WriteDone>) -> JoinHandle<()> {
    std::thread::Builder::new()
        .name("compd-persist".to_string())
        .spawn(move || {
            while let Ok(job) = jobs.recv() {
                let ok = match atomic_write(&job.path, &job.bytes) {
                    Ok(()) => {
                        trace!("persist: wrote {}", job.path.display());
                        true
                    }
                    Err(e) => {
                        warn!("persist: write {} failed: {e}", job.path.display());
                        false
                    }
                };
                // Receiver gone ⇒ shutting down; stop quietly.
                if done.send(WriteDone { world: job.world, key: job.key, epoch: job.epoch, ok }).is_err() {
                    break;
                }
            }
        })
        .expect("spawn compd-persist worker thread")
}

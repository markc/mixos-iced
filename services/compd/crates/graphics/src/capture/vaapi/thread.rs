//! Off-thread NVENC encoding.
//!
//! The NVENC path reads back BGRA frames from the GPU on the render thread, but
//! the *encode* (the per-frame `memcpy` into the AVFrame + `avcodec_send_frame`
//! + mux write) used to run inline there too — stalling the compositor's frame
//! loop (visible as a sluggish desktop while recording). [`EncoderThread`] moves
//! all of that onto a dedicated worker: the render thread only does the wgpu
//! readback and hands the owned BGRA buffer off over an **unbounded** channel,
//! so no frame is ever dropped under back-pressure (the queue grows instead).
//!
//! The `NvencEncoder` (raw libav pointers, not `Send`) lives entirely on the
//! worker thread; only the `Sender` and the `JoinHandle` cross threads.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;

use crate::capture::vaapi::nvenc::NvencEncoder;

/// A unit of work for the encoder thread.
enum EncMsg {
    /// One captured frame: tightly-packed BGRA (`w*h*4` bytes). `flip` requests
    /// a vertical row-reversal first (winit/nested GLES framebuffers are
    /// bottom-up) — done on the worker so the render thread pays nothing for it.
    Frame {
        bgra: Vec<u8>,
        w: u32,
        h: u32,
        flip: bool,
        /// Capture-time PTS (encoder timebase 1/fps).
        pts: i64,
    },
    /// Flush + finalize the mp4; the worker returns the temp path.
    Finish,
    /// Drop the encode and delete the temp file.
    Discard,
}

/// Handle to an NVENC encode running on its own thread. Frames are queued via
/// [`send`](Self::send); [`finish`](Self::finish)/[`discard`](Self::discard)
/// join the worker and reclaim the result.
pub struct EncoderThread {
    tx: Sender<EncMsg>,
    join: Option<JoinHandle<Option<PathBuf>>>,
}

impl EncoderThread {
    /// Spawn the worker and initialize an `NvencEncoder` on it. Returns `None`
    /// if NVENC is unavailable / the encoder fails to open (reported back from
    /// the worker before the first frame, so callers behave exactly like the
    /// old inline `NvencEncoder::start` returning `None`).
    pub fn spawn_nvenc(width: u32, height: u32, fps: u32, cq: u32) -> Option<Self> {
        let (tx, rx) = channel::<EncMsg>();
        let (init_tx, init_rx) = channel::<bool>();
        let join = std::thread::Builder::new()
            .name("compd-nvenc-encode".into())
            .spawn(move || worker(width, height, fps, cq, rx, init_tx))
            .ok()?;
        match init_rx.recv() {
            Ok(true) => Some(EncoderThread {
                tx,
                join: Some(join),
            }),
            _ => {
                let _ = join.join();
                None
            }
        }
    }

    /// Queue one BGRA frame for encoding. Never blocks the caller and never
    /// drops the frame (unbounded queue); a no-op if the worker has exited.
    pub fn send(&self, bgra: Vec<u8>, w: u32, h: u32, flip: bool, pts: i64) {
        let _ = self.tx.send(EncMsg::Frame {
            bgra,
            w,
            h,
            flip,
            pts,
        });
    }

    /// Flush the encoder, finalize the mp4, and return the temp path.
    pub fn finish(mut self) -> Option<PathBuf> {
        let _ = self.tx.send(EncMsg::Finish);
        self.join.take()?.join().ok().flatten()
    }

    /// Discard the encode and delete the temp file.
    pub fn discard(mut self) {
        let _ = self.tx.send(EncMsg::Discard);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

fn worker(
    width: u32,
    height: u32,
    fps: u32,
    cq: u32,
    rx: Receiver<EncMsg>,
    init_tx: Sender<bool>,
) -> Option<PathBuf> {
    let mut enc = match NvencEncoder::start(width, height, fps, cq) {
        Some(e) => {
            let _ = init_tx.send(true);
            e
        }
        None => {
            let _ = init_tx.send(false);
            return None;
        }
    };
    for msg in rx {
        match msg {
            EncMsg::Frame {
                mut bgra,
                w,
                h,
                flip,
                pts,
            } => {
                if flip {
                    flip_vertical(&mut bgra, w, h);
                }
                enc.push(&bgra, w, h, pts);
            }
            EncMsg::Finish => return enc.finish(),
            EncMsg::Discard => {
                enc.discard();
                return None;
            }
        }
    }
    // Senders all dropped without an explicit Finish/Discard: treat as discard
    // so we don't leave a temp file behind.
    enc.discard();
    None
}

/// Flip a tightly-packed BGRA buffer vertically in place (mirrors
/// `Frame::flip_vertical`, kept here so this crate doesn't depend on
/// `capture.encode`).
fn flip_vertical(bgra: &mut [u8], w: u32, h: u32) {
    let stride = (w * 4) as usize;
    if stride == 0 {
        return;
    }
    let h = h as usize;
    for row in 0..h / 2 {
        let top = row * stride;
        let bot = (h - 1 - row) * stride;
        let (a, b) = bgra.split_at_mut(bot);
        a[top..top + stride].swap_with_slice(&mut b[..stride]);
    }
}

//! Zero-copy NVENC encoder: capture **dmabuf** → EGLImage → CUDA → `*_nvenc`.
//!
//! Unlike [`crate::capture::vaapi::nvenc`] (which reads the frame back to system memory and
//! re-uploads), this path never touches the CPU: the capture dmabuf is imported
//! as an `EGLImage`, registered with CUDA, and each tick `cuMemcpy2D`'d (GPU→GPU)
//! into an `AV_PIX_FMT_CUDA` frame that NVENC encodes directly. The encoder owns
//! all EGL/CUDA/libav state and runs on its own thread; the render thread only
//! signals "a fresh frame is in the dmabuf" via [`tick`](NvencCudaEncoder::tick).
//!
//! The whole chain is validated on the hardware; it fails soft (returns `None`)
//! if libcuda/libEGL or NVENC are unavailable, so callers fall back to the
//! readback encoder. The CUDA frame is BGR0 — NVENC converts to NV12 internally
//! (no `scale_cuda` filter needed).

use std::ffi::{CStr, c_int, c_void};
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::PathBuf;
use std::ptr;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;

use smithay::backend::allocator::Buffer;
use smithay::backend::allocator::dmabuf::Dmabuf;

use crate::capture::vaapi::common::{Muxer, averr};
use crate::capture::vaapi::cuda::{CUmemorytype, Cuda, CudaMemcpy2D, CU_EGL_FRAME_TYPE_ARRAY};
use crate::capture::vaapi::egl::Egl;
use crate::capture::vaapi::ffi;

/// NVENC output codec. All three accept CUDA/BGRA input and mux into mp4.
#[derive(Clone, Copy, Debug)]
pub enum Codec {
    H264,
    Hevc,
    Av1,
}

impl Codec {
    fn encoder_name(self) -> &'static CStr {
        match self {
            Codec::H264 => c"h264_nvenc",
            Codec::Hevc => c"hevc_nvenc",
            Codec::Av1 => c"av1_nvenc",
        }
    }
    /// Codec-specific profile (NVENC private opt), or `None` to leave default.
    fn profile(self) -> Option<&'static CStr> {
        match self {
            Codec::H264 => Some(c"high"),
            Codec::Hevc => Some(c"main"),
            Codec::Av1 => None,
        }
    }
    fn b_frames(self) -> c_int {
        match self {
            Codec::H264 | Codec::Hevc => 2,
            Codec::Av1 => 0,
        }
    }
}

// --- Per-frame pipeline knobs (compile-time). Defaults are byte-identical to the
//     stable path: a synchronous copy followed by a full-context sync. ----------
/// Keep the per-frame `cuCtxSynchronize` after a *synchronous* copy. The copy is
/// already host-synchronous, so this extra whole-context barrier is largely
/// redundant and serializes the copy against in-flight NVENC work. `true` =
/// stable; set `false` to drop it (lets NVENC overlap → higher throughput).
/// Ignored when [`ASYNC_COPY`] is on. Bench before flipping.
const PER_FRAME_CTX_SYNC: bool = true;
/// Copy the dmabuf into the NVENC input frame asynchronously on the ffmpeg CUDA
/// device's stream (no host sync — NVENC reads on the same stream, so the copy
/// is ordered before the encode). `false` = stable (sync copy). `true` removes
/// the per-frame host stall entirely; relies on ffmpeg's nvenc using the device
/// stream for input, so verify output integrity when enabling.
const ASYNC_COPY: bool = false;

/// Trade max quality for throughput on the *live* encode: drop NVENC from `p7`
/// (slowest/best) to `p4`, and force split-frame encoding across the Ada NVENC
/// units (~2× on av1/hevc; h264 has no split and ignores it). Ideal for the
/// auto-re-encode workflow where the live capture is a throwaway intermediate.
/// `false` = stable (p7, split-encode left on `auto`). Bench p4+split vs p7.
const SPLIT_ENCODE: bool = true;

// --- Encoder tuning (shared with crate::capture::vaapi::nvenc; see CAPTURE.md). --------------
const BITRATE: i64 = 40_000_000;
const MAXRATE: i64 = 60_000_000;
const BUFSIZE: c_int = 120_000_000;
const GOP_SECONDS: u32 = 2;

/// The dmabuf description handed to the worker (plain, `Send` data + the owned
/// plane-0 fd). Single-plane ARGB/XRGB only (the capture entries are).
struct DmabufInfo {
    width: i32,
    height: i32,
    fourcc: u32,
    modifier: u64,
    offset: u32,
    stride: u32,
    fd: OwnedFd,
}

enum Msg {
    /// A fresh frame is in the dmabuf — encode the latest one. PTS is derived on
    /// the worker at encode time (not carried here), so coalesced ticks still get
    /// correct, real-time playback timing.
    Tick,
    Finish,
    Discard,
}

/// Handle to a zero-copy NVENC encode running on its own thread.
pub struct NvencCudaEncoder {
    tx: Sender<Msg>,
    join: Option<JoinHandle<Option<PathBuf>>>,
}

impl NvencCudaEncoder {
    /// Set up the zero-copy pipeline for `dmabuf`. Returns `None` if libEGL/
    /// libcuda/NVENC are unavailable or any stage fails (→ caller falls back to
    /// the readback path).
    pub fn start(dmabuf: &Dmabuf, fps: u32, codec: Codec, cq: u32) -> Option<Self> {
        let width = dmabuf.width() as i32;
        let height = dmabuf.height() as i32;
        if width < 2 || height < 2 {
            return None;
        }
        let fd = dmabuf.handles().next()?.try_clone_to_owned().ok()?;
        let info = DmabufInfo {
            width,
            height,
            fourcc: dmabuf.format().code as u32,
            modifier: dmabuf.format().modifier.into(),
            offset: dmabuf.offsets().next().unwrap_or(0),
            stride: dmabuf.strides().next().unwrap_or(0),
            fd,
        };
        let fps = fps.max(1);

        let (tx, rx) = channel::<Msg>();
        let (init_tx, init_rx) = channel::<bool>();
        let join = std::thread::Builder::new()
            .name("compd-nvenc-cuda".into())
            .spawn(move || worker(info, fps, codec, cq, rx, init_tx))
            .ok()?;
        match init_rx.recv() {
            Ok(true) => Some(NvencCudaEncoder {
                tx,
                join: Some(join),
            }),
            _ => {
                let _ = join.join();
                None
            }
        }
    }

    /// Signal that the dmabuf holds a fresh frame. Non-blocking. Extra signals
    /// while the worker is busy coalesce — the dmabuf only ever holds the latest
    /// frame, so queuing them would only let the worker fall behind (sped-up
    /// playback) and pile up a backlog that stalls Stop.
    pub fn tick(&self) {
        let _ = self.tx.send(Msg::Tick);
    }

    pub fn finish(mut self) -> Option<PathBuf> {
        let _ = self.tx.send(Msg::Finish);
        self.join.take()?.join().ok().flatten()
    }

    pub fn discard(mut self) {
        let _ = self.tx.send(Msg::Discard);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// ffmpeg's `AVCUDADeviceContext` — we only read the first field (`cuda_ctx`).
/// `hwcontext_cuda.h` isn't in the bindgen wrapper, so it's hand-declared.
#[repr(C)]
struct AvcudaDeviceContextMin {
    cuda_ctx: crate::capture::vaapi::cuda::CUcontext,
    stream: *mut c_void,
    internal: *mut c_void,
}

/// All EGL/CUDA/libav resources, owned and used on the worker thread.
struct Session {
    cuda: Cuda,
    egl: Egl,
    // `image` borrows `egl`; kept in a box behind a raw pointer would be
    // self-referential, so we keep the EGLImage as a raw handle + destroy it via
    // egl in Drop instead of the guard. Simpler: store the raw image.
    image: crate::capture::vaapi::egl::EGLImage,
    cures: crate::capture::vaapi::cuda::CUgraphicsResource,
    hwdev: *mut ffi::AVBufferRef,
    frames: *mut ffi::AVBufferRef,
    codec_ctx: *mut ffi::AVCodecContext,
    /// The ffmpeg CUDA device's stream (used for the async-copy knob).
    stream: crate::capture::vaapi::cuda::CUstream,
    mux: Muxer,
    enc_w: c_int,
    enc_h: c_int,
    pts: i64,
    temp: PathBuf,
}

fn worker(
    info: DmabufInfo,
    fps: u32,
    codec: Codec,
    cq: u32,
    rx: Receiver<Msg>,
    init_tx: Sender<bool>,
) -> Option<PathBuf> {
    let mut sess = match unsafe { Session::init(&info, fps, codec, cq) } {
        Some(s) => {
            let _ = init_tx.send(true);
            s
        }
        None => {
            let _ = init_tx.send(false);
            return None;
        }
    };
    // PTS anchor: real elapsed time is measured HERE (at encode time), so a
    // frame's PTS always matches the dmabuf content the worker actually reads —
    // even if the worker can't keep up with the capture rate. This is what keeps
    // playback at the right speed regardless of throughput.
    let mut anchor: Option<std::time::Instant> = None;
    loop {
        let msg = match rx.recv() {
            Ok(m) => m,
            Err(_) => break, // senders dropped without Finish/Discard → discard
        };
        match msg {
            Msg::Tick => {
                // Coalesce any queued ticks; the dmabuf only holds the latest
                // frame, so we encode it exactly once per drain.
                let mut terminal = None;
                loop {
                    match rx.try_recv() {
                        Ok(Msg::Tick) => {}
                        Ok(Msg::Finish) => {
                            terminal = Some(true);
                            break;
                        }
                        Ok(Msg::Discard) => {
                            terminal = Some(false);
                            break;
                        }
                        Err(_) => break,
                    }
                }
                let a = *anchor.get_or_insert_with(std::time::Instant::now);
                let pts = (a.elapsed().as_secs_f64() * fps as f64).round() as i64;
                unsafe { sess.encode_one(pts) };
                match terminal {
                    Some(true) => return Some(unsafe { sess.finish() }),
                    Some(false) => {
                        let temp = sess.temp.clone();
                        drop(sess);
                        let _ = std::fs::remove_file(temp);
                        return None;
                    }
                    None => {}
                }
            }
            Msg::Finish => {
                let temp = unsafe { sess.finish() };
                return Some(temp);
            }
            Msg::Discard => {
                let temp = sess.temp.clone();
                drop(sess);
                let _ = std::fs::remove_file(temp);
                return None;
            }
        }
    }
    // Senders dropped without Finish/Discard → discard.
    let temp = sess.temp.clone();
    drop(sess);
    let _ = std::fs::remove_file(temp);
    None
}

impl Session {
    unsafe fn init(info: &DmabufInfo, fps: u32, codec: Codec, cq: u32) -> Option<Session> {
        let cuda = Cuda::load()?;
        if unsafe { cuda.init() } != 0 {
            warn!("nvenc-cuda: cuInit failed");
            return None;
        }
        let egl = Egl::open_device_display()?;
        // Import the dmabuf; we pull the raw image out of the guard and manage
        // its lifetime ourselves (see `Drop`).
        let image = {
            let guard = egl.import_dmabuf(
                info.width,
                info.height,
                info.fourcc,
                info.fd.as_raw_fd(),
                info.offset,
                info.stride,
                info.modifier,
            )?;
            let raw = guard.raw();
            std::mem::forget(guard); // don't destroy yet; Session::drop does it
            raw
        };

        // ffmpeg CUDA device — owns the CUDA context we borrow for interop.
        let mut hwdev: *mut ffi::AVBufferRef = ptr::null_mut();
        let r = unsafe {
            ffi::av_hwdevice_ctx_create(
                &mut hwdev,
                ffi::AV_HWDEVICE_TYPE_CUDA,
                ptr::null(),
                ptr::null_mut(),
                0,
            )
        };
        if r < 0 {
            warn!("nvenc-cuda: av_hwdevice_ctx_create(CUDA): {}", averr(r));
            unsafe { egl.destroy_raw(image) };
            return None;
        }
        let (cuda_ctx, stream) = unsafe {
            let dctx = (*hwdev).data as *mut ffi::AVHWDeviceContext;
            let hwctx = (*dctx).hwctx as *mut AvcudaDeviceContextMin;
            ((*hwctx).cuda_ctx, (*hwctx).stream as crate::capture::vaapi::cuda::CUstream)
        };
        unsafe { cuda.set_current(cuda_ctx) };

        let cures = match unsafe { cuda.register_egl(image) } {
            Ok(r) => r,
            Err(e) => {
                warn!("nvenc-cuda: cuGraphicsEGLRegisterImage failed: {e}");
                unsafe {
                    ffi::av_buffer_unref(&mut hwdev);
                    egl.destroy_raw(image);
                }
                return None;
            }
        };

        let enc_w = info.width & !1;
        let enc_h = info.height & !1;

        // CUDA frames context (BGR0 linear NVENC inputs).
        let mut frames = unsafe { ffi::av_hwframe_ctx_alloc(hwdev) };
        if frames.is_null() {
            warn!("nvenc-cuda: av_hwframe_ctx_alloc failed");
            unsafe { cleanup(&cuda, cures, &mut hwdev, &egl, image) };
            return None;
        }
        unsafe {
            let fctx = (*frames).data as *mut ffi::AVHWFramesContext;
            (*fctx).format = ffi::AV_PIX_FMT_CUDA;
            (*fctx).sw_format = ffi::AV_PIX_FMT_BGR0;
            (*fctx).width = enc_w;
            (*fctx).height = enc_h;
            let r = ffi::av_hwframe_ctx_init(frames);
            if r < 0 {
                warn!("nvenc-cuda: av_hwframe_ctx_init: {}", averr(r));
                ffi::av_buffer_unref(&mut frames);
                cleanup(&cuda, cures, &mut hwdev, &egl, image);
                return None;
            }
        }

        // NVENC encoder on CUDA frames.
        let codec_ctx = match unsafe { build_encoder(codec, frames, enc_w, enc_h, fps, cq) } {
            Some(c) => c,
            None => {
                unsafe {
                    ffi::av_buffer_unref(&mut frames);
                    cleanup(&cuda, cures, &mut hwdev, &egl, image);
                }
                return None;
            }
        };

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let temp = std::env::temp_dir().join(format!("compd-capture-{nanos}.mp4"));
        let mux = match unsafe { Muxer::new(&temp, codec_ctx) } {
            Ok(m) => m,
            Err(e) => {
                warn!("nvenc-cuda: muxer: {e}");
                unsafe {
                    let mut cc = codec_ctx;
                    ffi::avcodec_free_context(&mut cc);
                    ffi::av_buffer_unref(&mut frames);
                    cleanup(&cuda, cures, &mut hwdev, &egl, image);
                }
                return None;
            }
        };

        info!("nvenc-cuda: zero-copy encoder ready ({enc_w}x{enc_h} {codec:?})");
        Some(Session {
            cuda,
            egl,
            image,
            cures,
            hwdev,
            frames,
            codec_ctx,
            stream,
            mux,
            enc_w,
            enc_h,
            pts: -1,
            temp,
        })
    }

    /// Map the dmabuf via CUDA, copy into a fresh CUDA frame, and encode it.
    unsafe fn encode_one(&mut self, pts: i64) {
        let ef = match unsafe { self.cuda.mapped_egl_frame(self.cures) } {
            Ok(f) => f,
            Err(e) => {
                warn!("nvenc-cuda: map egl frame failed: {e}");
                return;
            }
        };
        let cf = unsafe { ffi::av_frame_alloc() };
        if cf.is_null() {
            return;
        }
        unsafe {
            (*cf).format = ffi::AV_PIX_FMT_CUDA;
            (*cf).width = self.enc_w;
            (*cf).height = self.enc_h;
            let r = ffi::av_hwframe_get_buffer(self.frames, cf, 0);
            if r < 0 {
                warn!("nvenc-cuda: av_hwframe_get_buffer: {}", averr(r));
                ffi::av_frame_free(&mut (cf as *mut _));
                return;
            }

            let mut m = CudaMemcpy2D::default();
            if ef.frame_type == CU_EGL_FRAME_TYPE_ARRAY {
                m.src_memory_type = CUmemorytype::Array;
                m.src_array = ef.plane[0];
            } else {
                m.src_memory_type = CUmemorytype::Device;
                m.src_device = ef.plane[0] as crate::capture::vaapi::cuda::CUdeviceptr;
                m.src_pitch = ef.pitch as usize;
            }
            m.dst_memory_type = CUmemorytype::Device;
            m.dst_device = (*cf).data[0] as crate::capture::vaapi::cuda::CUdeviceptr;
            m.dst_pitch = (*cf).linesize[0] as usize;
            m.width_in_bytes = self.enc_w as usize * 4;
            m.height = self.enc_h as usize;
            let r = if ASYNC_COPY {
                // Async on the device stream; NVENC reads on the same stream, so
                // the copy is ordered before the encode — no host sync.
                self.cuda.memcpy2d_async(&m, self.stream)
            } else {
                self.cuda.memcpy2d(&m)
            };
            if r != 0 {
                warn!("nvenc-cuda: cuMemcpy2D failed: {r}");
                ffi::av_frame_free(&mut (cf as *mut _));
                return;
            }
            // The synchronous copy is already host-synced; the extra full-context
            // barrier is optional (and skipped entirely for the async path).
            if !ASYNC_COPY && PER_FRAME_CTX_SYNC {
                self.cuda.synchronize();
            }

            // Keep PTS strictly monotonic (libav rejects non-increasing PTS);
            // the throttle already guarantees ≥1 spacing, this is just a guard.
            let pts = pts.max(self.pts + 1);
            self.pts = pts;
            (*cf).pts = pts;
            self.mux.pump(self.codec_ctx, cf);
            ffi::av_frame_free(&mut (cf as *mut _));
        }
    }

    unsafe fn finish(&mut self) -> PathBuf {
        unsafe { self.mux.finish(self.codec_ctx) };
        self.temp.clone()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        unsafe {
            if !self.codec_ctx.is_null() {
                ffi::avcodec_free_context(&mut self.codec_ctx);
            }
            if !self.frames.is_null() {
                ffi::av_buffer_unref(&mut self.frames);
            }
            cleanup(&self.cuda, self.cures, &mut self.hwdev, &self.egl, self.image);
        }
    }
}

/// Unregister the CUDA resource, free the ffmpeg CUDA device, destroy the
/// EGLImage. Order matters: CUDA resource before its EGLImage/device.
unsafe fn cleanup(
    cuda: &Cuda,
    cures: crate::capture::vaapi::cuda::CUgraphicsResource,
    hwdev: *mut *mut ffi::AVBufferRef,
    egl: &Egl,
    image: crate::capture::vaapi::egl::EGLImage,
) {
    unsafe {
        if !cures.is_null() {
            cuda.unregister(cures);
        }
        if !(*hwdev).is_null() {
            ffi::av_buffer_unref(hwdev);
        }
        if !image.is_null() {
            egl.destroy_raw(image);
        }
    }
}

unsafe fn build_encoder(
    codec: Codec,
    frames: *mut ffi::AVBufferRef,
    w: c_int,
    h: c_int,
    fps: u32,
    cq: u32,
) -> Option<*mut ffi::AVCodecContext> {
    unsafe {
        let enc = ffi::avcodec_find_encoder_by_name(codec.encoder_name().as_ptr());
        if enc.is_null() {
            warn!("nvenc-cuda: encoder not found");
            return None;
        }
        let ctx = ffi::avcodec_alloc_context3(enc);
        if ctx.is_null() {
            return None;
        }
        let c = &mut *ctx;
        c.width = w;
        c.height = h;
        c.time_base = ffi::AVRational {
            num: 1,
            den: fps as c_int,
        };
        c.framerate = ffi::AVRational {
            num: fps as c_int,
            den: 1,
        };
        c.pix_fmt = ffi::AV_PIX_FMT_CUDA;
        c.hw_frames_ctx = ffi::av_buffer_ref(frames);
        c.bit_rate = BITRATE;
        c.rc_max_rate = MAXRATE;
        c.rc_buffer_size = BUFSIZE;
        c.gop_size = (fps * GOP_SECONDS) as c_int;
        c.max_b_frames = codec.b_frames();
        let p = c.priv_data;
        // SPLIT_ENCODE trades the slowest/best preset for throughput: p4 instead
        // of p7, plus split-frame encoding forced on across the NVENC units.
        let preset: &CStr = if SPLIT_ENCODE { c"p4" } else { c"p7" };
        ffi::av_opt_set(p, c"preset".as_ptr(), preset.as_ptr(), 0);
        ffi::av_opt_set(p, c"tune".as_ptr(), c"hq".as_ptr(), 0);
        ffi::av_opt_set(p, c"rc".as_ptr(), c"vbr".as_ptr(), 0);
        let cq_str = std::ffi::CString::new(cq.to_string()).unwrap();
        ffi::av_opt_set(p, c"cq".as_ptr(), cq_str.as_ptr(), 0);
        if SPLIT_ENCODE {
            // `1` = "forced": the driver picks the strip count for the number of
            // NVENC units (2 on Ada). Only av1_nvenc/hevc_nvenc have this option;
            // h264_nvenc ignores it (av_opt_set fails soft, which we don't check).
            ffi::av_opt_set(p, c"split_encode_mode".as_ptr(), c"1".as_ptr(), 0);
        }
        if let Some(profile) = codec.profile() {
            ffi::av_opt_set(p, c"profile".as_ptr(), profile.as_ptr(), 0);
        }
        let r = ffi::avcodec_open2(ctx, enc, ptr::null_mut());
        if r < 0 {
            warn!("nvenc-cuda: avcodec_open2: {}", averr(r));
            let mut cc = ctx;
            ffi::avcodec_free_context(&mut cc);
            return None;
        }
        Some(ctx)
    }
}

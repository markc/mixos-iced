//! VAAPI hardware H.264 encoder (in-process libav / rusty_ffmpeg).
//!
//! Zero-copy: the capture entry **dmabuf** is wrapped as a persistent
//! `AV_PIX_FMT_DRM_PRIME` frame and pushed each tick through an avfilter graph
//! that hardware-maps it to VAAPI and converts BGRA→NV12 (`hwmap=derive_device=vaapi,
//! scale_vaapi=format=nv12`), then into `h264_vaapi`, muxed to mp4. Encoding
//! runs inline on the render thread — VAAPI is fast enough that this doesn't
//! stall like the software path did.
//!
//! All FFI; freed in `Drop`. Untestable without a GPU — designed to fail soft
//! (returns `None`/logs) rather than panic.

use std::ffi::{CStr, CString, c_int, c_void};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::ptr;

use smithay::backend::allocator::Buffer;
use smithay::backend::allocator::dmabuf::Dmabuf;

use crate::capture::vaapi::common::{AVERROR, AVERROR_EOF, EAGAIN, averr, compd_avfmt_get_pb, compd_avfmt_set_pb};
use crate::capture::vaapi::ffi;
use crate::capture::vaapi::nvenc_cuda::Codec;

const MAX_PLANES: usize = 4;

// `lseek(fd, 0, SEEK_END)` reports a dmabuf's size (needed for the DRM object
// descriptor). In libc, always linked — no extra crate dependency.
unsafe extern "C" {
    fn lseek(fd: c_int, offset: i64, whence: c_int) -> i64;
}
const SEEK_END: c_int = 2;

/// No-op `AVBuffer` free callback: the wrapped DRM descriptor is owned by the
/// encoder's `Box`, not by libav, so dropping the buffer must not free it.
unsafe extern "C" fn noop_buffer_free(_opaque: *mut c_void, _data: *mut u8) {}

// --- Encoder tuning knobs (VAAPI). All compile-time constants; see CAPTURE.md. ---
// Kept conservative: VAAPI drivers vary, and unsupported options fail soft
// (av_opt_set is ignored), but rejected B-frames/profiles can fail encoder open.
/// Target bitrate (bits/s). Sized for up to 4K@60.
const VAAPI_BITRATE: i64 = 40_000_000;
/// VBR ceiling bitrate (bits/s).
const VAAPI_MAXRATE: i64 = 60_000_000;
/// Rate-control buffer (bits) — ~2s of MAXRATE.
const VAAPI_BUFSIZE: c_int = 120_000_000;
/// Keyframe (IDR) interval, in seconds (`gop_size = fps * GOP_SECONDS`).
const GOP_SECONDS: u32 = 2;
/// Consecutive B-frames. 0 = safest across VAAPI drivers (many reject B-frames).
const VAAPI_BFRAMES: c_int = 0;
/// Rate control mode (`CQP` | `VBR` | `CBR`), set as the `rc_mode` priv option.
const VAAPI_RC_MODE: &CStr = c"VBR";
/// H.264 profile.
const VAAPI_PROFILE: &CStr = c"high";

/// A live VAAPI encode session writing to a temp mp4.
pub struct VaapiEncoder {
    drm_device: *mut ffi::AVBufferRef,
    drm_frames: *mut ffi::AVBufferRef,
    codec_ctx: *mut ffi::AVCodecContext,
    fmt_ctx: *mut ffi::AVFormatContext,
    graph: *mut ffi::AVFilterGraph,
    src: *mut ffi::AVFilterContext,
    sink: *mut ffi::AVFilterContext,
    drm_frame: *mut ffi::AVFrame,
    packet: *mut ffi::AVPacket,
    stream: *mut ffi::AVStream,
    // The descriptor `drm_frame.data[0]` points at; must outlive the frame.
    desc: Box<ffi::AVDRMFrameDescriptor>,
    stream_index: c_int,
    pts: i64,
    fps: u32,
    codec: Codec,
    width: u32,
    height: u32,
    temp: PathBuf,
    started: bool,
}

impl VaapiEncoder {
    /// Set up the pipeline for the given capture dmabuf (BGRA), encoding with
    /// `codec`'s VAAPI encoder. Returns `None` on any failure (no VAAPI, codec
    /// unsupported on this GPU, etc.) — the caller walks a fallback list
    /// (`av1 → hevc → h264`), so a GPU without AV1/HEVC encode lands on H.264.
    pub fn start(dmabuf: &Dmabuf, fps: u32, codec: Codec) -> Option<Self> {
        let width = dmabuf.width();
        let height = dmabuf.height();
        let w = (width & !1) as i32;
        let h = (height & !1) as i32;
        if w == 0 || h == 0 {
            return None;
        }

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let temp = std::env::temp_dir().join(format!("compd-capture-{nanos}.mp4"));

        let mut enc = VaapiEncoder {
            drm_device: ptr::null_mut(),
            drm_frames: ptr::null_mut(),
            codec_ctx: ptr::null_mut(),
            fmt_ctx: ptr::null_mut(),
            graph: ptr::null_mut(),
            src: ptr::null_mut(),
            sink: ptr::null_mut(),
            drm_frame: ptr::null_mut(),
            packet: ptr::null_mut(),
            stream: ptr::null_mut(),
            desc: Box::new(unsafe { std::mem::zeroed() }),
            stream_index: 0,
            pts: -1,
            fps: fps.max(1),
            codec,
            width: w as u32,
            height: h as u32,
            temp: temp.clone(),
            started: false,
        };

        match unsafe { enc.init(dmabuf, w, h) } {
            Ok(()) => {
                enc.started = true;
                Some(enc)
            }
            Err(e) => {
                warn!("vaapi encoder init failed ({e}); video unavailable");
                None
            }
        }
    }

    pub fn dims(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    unsafe fn init(&mut self, dmabuf: &Dmabuf, w: c_int, h: c_int) -> Result<(), String> {
        // 1. DRM device on the render node. The capture dmabuf is wrapped as a
        //    DRM_PRIME hw frame, and a DRM_PRIME frames context can ONLY live on a
        //    DRM-type device — allocating it on a VAAPI device makes
        //    `av_hwframe_ctx_init` return ENOSYS ("function not implemented").
        //    The VAAPI device is derived later by `hwmap=derive_device=vaapi`.
        let node = CString::new(crate::capture::vaapi::capture_render_node()).unwrap();
        let r = ffi::av_hwdevice_ctx_create(
            &mut self.drm_device,
            ffi::AV_HWDEVICE_TYPE_DRM,
            node.as_ptr(),
            ptr::null_mut(),
            0,
        );
        if r < 0 {
            return Err(format!("av_hwdevice_ctx_create(drm): {}", averr(r)));
        }

        // 2. Fill the persistent DRM_PRIME descriptor from the dmabuf.
        self.fill_drm_descriptor(dmabuf)?;

        // 3. Persistent DRM_PRIME AVFrame referencing the descriptor.
        self.drm_frame = ffi::av_frame_alloc();
        if self.drm_frame.is_null() {
            return Err("av_frame_alloc failed".into());
        }
        (*self.drm_frame).format = ffi::AV_PIX_FMT_DRM_PRIME;
        (*self.drm_frame).width = w;
        (*self.drm_frame).height = h;
        (*self.drm_frame).data[0] = (&mut *self.desc) as *mut _ as *mut u8;
        // Make the frame REFERENCE-COUNTED by wrapping the (persistent) descriptor
        // in an AVBuffer with a no-op free (the Box<AVDRMFrameDescriptor> owns the
        // memory; this buffer must not free it). Without a `buf[0]`, libav treats
        // the frame as non-refcounted, so `av_buffersrc_add_frame(KEEP_REF)`
        // allocate-and-copies every frame — churning the DRM hwframe pool until it
        // ENOMEMs ("Cannot allocate memory"). A real `buf[0]` makes KEEP_REF a
        // cheap new reference.
        (*self.drm_frame).buf[0] = ffi::av_buffer_create(
            (&mut *self.desc) as *mut _ as *mut u8,
            std::mem::size_of::<ffi::AVDRMFrameDescriptor>(),
            Some(noop_buffer_free),
            ptr::null_mut(),
            0,
        );
        if (*self.drm_frame).buf[0].is_null() {
            return Err("av_buffer_create(drm descriptor) failed".into());
        }

        // 4. Filter graph: buffer(drm_prime) → hwmap→vaapi → nv12 → buffersink.
        self.build_filtergraph(w, h)?;

        // 5. Encoder (h264_vaapi), input = the buffersink's VAAPI NV12 frames.
        self.build_encoder(w, h)?;

        // 6. mp4 muxer.
        self.build_muxer()?;

        self.packet = ffi::av_packet_alloc();
        if self.packet.is_null() {
            return Err("av_packet_alloc failed".into());
        }
        Ok(())
    }

    unsafe fn fill_drm_descriptor(&mut self, dmabuf: &Dmabuf) -> Result<(), String> {
        let nplanes = dmabuf.num_planes().min(MAX_PLANES);
        if nplanes == 0 {
            return Err("dmabuf has no planes".into());
        }
        let fds: Vec<c_int> = dmabuf.handles().map(|fd| fd.as_raw_fd()).collect();
        let offsets: Vec<u32> = dmabuf.offsets().collect();
        let strides: Vec<u32> = dmabuf.strides().collect();
        let format = dmabuf.format();
        let fourcc: u32 = format.code as u32;
        let modifier: u64 = format.modifier.into();

        let height = dmabuf.height();
        let desc = &mut *self.desc;
        // One object per fd (planes may share an fd; deduping is optional — VAAPI
        // import accepts one object per plane fd).
        desc.nb_objects = nplanes as c_int;
        for i in 0..nplanes {
            let fd = fds.get(i).copied().unwrap_or(fds[0]);
            desc.objects[i].fd = fd;
            // The object size MUST be the real dmabuf size. NVIDIA never reached
            // this path, but MESA's VAAPI dmabuf import rejects `size = 0` with
            // ENOMEM ("Cannot allocate memory") when the frame flows through
            // `hwmap`. `lseek(SEEK_END)` returns a dmabuf's size; fall back to
            // stride*height if that ever fails.
            let sz = lseek(fd, 0, SEEK_END);
            desc.objects[i].size = if sz > 0 {
                sz as _
            } else {
                (strides.get(i).copied().unwrap_or(0) as u64 * height as u64) as _
            };
            desc.objects[i].format_modifier = modifier;
        }
        desc.nb_layers = 1;
        desc.layers[0].format = fourcc;
        desc.layers[0].nb_planes = nplanes as c_int;
        for i in 0..nplanes {
            desc.layers[0].planes[i].object_index = i as c_int;
            desc.layers[0].planes[i].offset = offsets.get(i).copied().unwrap_or(0) as isize as _;
            desc.layers[0].planes[i].pitch = strides.get(i).copied().unwrap_or(0) as isize as _;
        }
        Ok(())
    }

    unsafe fn build_filtergraph(&mut self, w: c_int, h: c_int) -> Result<(), String> {
        self.graph = ffi::avfilter_graph_alloc();
        if self.graph.is_null() {
            return Err("avfilter_graph_alloc failed".into());
        }

        // buffersrc with DRM_PRIME params + the DRM frames context.
        let buffersrc = ffi::avfilter_get_by_name(c"buffer".as_ptr());
        let buffersink = ffi::avfilter_get_by_name(c"buffersink".as_ptr());
        if buffersrc.is_null() || buffersink.is_null() {
            return Err("buffer/buffersink filter missing".into());
        }

        // We need a hw_frames_ctx (DRM) so the buffersrc knows it's a hw frame.
        let drm_dev_for_frames = ffi::av_buffer_ref(self.drm_device);
        self.drm_frames = ffi::av_hwframe_ctx_alloc(drm_dev_for_frames);
        if self.drm_frames.is_null() {
            return Err("av_hwframe_ctx_alloc(drm) failed".into());
        }
        {
            let fctx = (*self.drm_frames).data as *mut ffi::AVHWFramesContext;
            (*fctx).format = ffi::AV_PIX_FMT_DRM_PRIME;
            (*fctx).sw_format = ffi::AV_PIX_FMT_BGR0; // BGRA-ish; driver maps via modifier/fourcc
            (*fctx).width = w;
            (*fctx).height = h;
            let r = ffi::av_hwframe_ctx_init(self.drm_frames);
            if r < 0 {
                return Err(format!("av_hwframe_ctx_init(drm): {}", averr(r)));
            }
        }
        (*self.drm_frame).hw_frames_ctx = ffi::av_buffer_ref(self.drm_frames);

        // A buffersrc carrying a HW pixel format must have its `hw_frames_ctx`
        // set BEFORE the filter is initialized. So: alloc (uninitialized) →
        // set parameters (format/size/time_base + the DRM frames ctx) → init.
        // (`avfilter_graph_create_filter` initializes immediately, which errors
        // with "Setting BufferSourceContext.pix_fmt to a HW format requires
        // hw_frames_ctx to be non-NULL".)
        self.src = ffi::avfilter_graph_alloc_filter(self.graph, buffersrc, c"in".as_ptr());
        if self.src.is_null() {
            return Err("avfilter_graph_alloc_filter(buffersrc) failed".into());
        }
        let par = ffi::av_buffersrc_parameters_alloc();
        if par.is_null() {
            return Err("av_buffersrc_parameters_alloc failed".into());
        }
        (*par).format = ffi::AV_PIX_FMT_DRM_PRIME as c_int;
        (*par).width = w;
        (*par).height = h;
        (*par).time_base = ffi::AVRational {
            num: 1,
            den: self.fps as c_int,
        };
        (*par).hw_frames_ctx = ffi::av_buffer_ref(self.drm_frames);
        let r = ffi::av_buffersrc_parameters_set(self.src, par);
        ffi::av_free(par as *mut c_void);
        if r < 0 {
            return Err(format!("av_buffersrc_parameters_set: {}", averr(r)));
        }
        let r = ffi::avfilter_init_str(self.src, ptr::null());
        if r < 0 {
            return Err(format!("avfilter_init_str(buffersrc): {}", averr(r)));
        }

        let name_sink = c"out";
        let r = ffi::avfilter_graph_create_filter(
            &mut self.sink,
            buffersink,
            name_sink.as_ptr(),
            ptr::null(),
            ptr::null_mut(),
            self.graph,
        );
        if r < 0 {
            return Err(format!("create buffersink: {}", averr(r)));
        }

        // Parse the conversion chain between in → out.
        let mut inputs = ffi::avfilter_inout_alloc();
        let mut outputs = ffi::avfilter_inout_alloc();
        if inputs.is_null() || outputs.is_null() {
            return Err("avfilter_inout_alloc failed".into());
        }
        (*outputs).name = ffi::av_strdup(c"in".as_ptr());
        (*outputs).filter_ctx = self.src;
        (*outputs).pad_idx = 0;
        (*outputs).next = ptr::null_mut();
        (*inputs).name = ffi::av_strdup(c"out".as_ptr());
        (*inputs).filter_ctx = self.sink;
        (*inputs).pad_idx = 0;
        (*inputs).next = ptr::null_mut();

        let spec = c"hwmap=derive_device=vaapi,scale_vaapi=format=nv12";
        let r = ffi::avfilter_graph_parse_ptr(
            self.graph,
            spec.as_ptr(),
            &mut inputs,
            &mut outputs,
            ptr::null_mut(),
        );
        ffi::avfilter_inout_free(&mut inputs);
        ffi::avfilter_inout_free(&mut outputs);
        if r < 0 {
            return Err(format!("graph parse: {}", averr(r)));
        }
        let r = ffi::avfilter_graph_config(self.graph, ptr::null_mut());
        if r < 0 {
            return Err(format!("graph config: {}", averr(r)));
        }
        Ok(())
    }

    unsafe fn build_encoder(&mut self, w: c_int, h: c_int) -> Result<(), String> {
        // Codec → VAAPI encoder + profile. Not every GPU exposes every encoder
        // (e.g. Kaby Lake has no AV1/VP9 encode), so a missing encoder or a failed
        // open returns Err and the caller falls back to the next codec.
        let (name, profile): (&CStr, Option<&CStr>) = match self.codec {
            Codec::H264 => (c"h264_vaapi", Some(VAAPI_PROFILE)),
            Codec::Hevc => (c"hevc_vaapi", Some(c"main")),
            Codec::Av1 => (c"av1_vaapi", None),
        };
        let codec = ffi::avcodec_find_encoder_by_name(name.as_ptr());
        if codec.is_null() {
            return Err(format!("vaapi encoder {name:?} not found"));
        }
        self.codec_ctx = ffi::avcodec_alloc_context3(codec);
        if self.codec_ctx.is_null() {
            return Err("avcodec_alloc_context3 failed".into());
        }
        let c = &mut *self.codec_ctx;
        c.width = w;
        c.height = h;
        c.time_base = ffi::AVRational {
            num: 1,
            den: self.fps as c_int,
        };
        c.framerate = ffi::AVRational {
            num: self.fps as c_int,
            den: 1,
        };
        c.pix_fmt = ffi::AV_PIX_FMT_VAAPI;
        // The encoder's input frames context = the buffersink's output frames
        // context (VAAPI NV12), produced by the filter graph.
        let sink_frames = ffi::av_buffersink_get_hw_frames_ctx(self.sink);
        if sink_frames.is_null() {
            return Err("buffersink has no hw_frames_ctx".into());
        }
        c.hw_frames_ctx = ffi::av_buffer_ref(sink_frames);

        // Rate control + GOP (generic AVCodecContext fields).
        c.bit_rate = VAAPI_BITRATE;
        c.rc_max_rate = VAAPI_MAXRATE;
        c.rc_buffer_size = VAAPI_BUFSIZE;
        c.gop_size = (self.fps * GOP_SECONDS) as c_int;
        c.max_b_frames = VAAPI_BFRAMES;
        // h264_vaapi-private options (rc mode + profile); ignored if unsupported.
        let p = c.priv_data;
        ffi::av_opt_set(p, c"rc_mode".as_ptr(), VAAPI_RC_MODE.as_ptr(), 0);
        if let Some(prof) = profile {
            ffi::av_opt_set(p, c"profile".as_ptr(), prof.as_ptr(), 0);
        }

        let r = ffi::avcodec_open2(self.codec_ctx, codec, ptr::null_mut());
        if r < 0 {
            return Err(format!("avcodec_open2({name:?}): {}", averr(r)));
        }
        Ok(())
    }

    unsafe fn build_muxer(&mut self) -> Result<(), String> {
        let path = CString::new(self.temp.to_string_lossy().as_bytes()).unwrap();
        let r = ffi::avformat_alloc_output_context2(
            &mut self.fmt_ctx,
            ptr::null_mut(),
            c"mp4".as_ptr(),
            path.as_ptr(),
        );
        if r < 0 || self.fmt_ctx.is_null() {
            return Err(format!("avformat_alloc_output_context2: {}", averr(r)));
        }
        let stream = ffi::avformat_new_stream(self.fmt_ctx, ptr::null());
        if stream.is_null() {
            return Err("avformat_new_stream failed".into());
        }
        self.stream = stream;
        self.stream_index = (*stream).index;
        (*stream).time_base = (*self.codec_ctx).time_base;
        let r = ffi::avcodec_parameters_from_context((*stream).codecpar, self.codec_ctx);
        if r < 0 {
            return Err(format!("avcodec_parameters_from_context: {}", averr(r)));
        }
        // Open the output file and attach it via the C shim (pb is bindgen-opaque).
        let mut pb: *mut ffi::AVIOContext = ptr::null_mut();
        let r = ffi::avio_open(&mut pb, path.as_ptr(), ffi::AVIO_FLAG_WRITE as c_int);
        if r < 0 {
            return Err(format!("avio_open: {}", averr(r)));
        }
        compd_avfmt_set_pb(self.fmt_ctx, pb);
        let r = ffi::avformat_write_header(self.fmt_ctx, ptr::null_mut());
        if r < 0 {
            return Err(format!("avformat_write_header: {}", averr(r)));
        }
        Ok(())
    }

    /// Encode the current contents of the capture dmabuf as one frame. Call
    /// after the per-frame render into the dmabuf has completed (synced).
    pub fn encode(&mut self, pts: i64) {
        if !self.started {
            return;
        }
        unsafe {
            let pts = pts.max(self.pts + 1); // strictly monotonic guard
            self.pts = pts;
            (*self.drm_frame).pts = pts;
            // Push the (persistent) DRM frame into the graph (its dmabuf content
            // changed since last frame). KEEP_REF so the graph doesn't take it.
            let r = ffi::av_buffersrc_add_frame_flags(
                self.src,
                self.drm_frame,
                ffi::AV_BUFFERSRC_FLAG_KEEP_REF as c_int,
            );
            if r < 0 {
                warn!("vaapi: buffersrc add frame: {}", averr(r));
                return;
            }
            let nv12 = ffi::av_frame_alloc();
            loop {
                let r = ffi::av_buffersink_get_frame(self.sink, nv12);
                if r == AVERROR(EAGAIN) || r == AVERROR_EOF {
                    break;
                }
                if r < 0 {
                    warn!("vaapi: buffersink get frame: {}", averr(r));
                    break;
                }
                self.send_and_mux(nv12);
                ffi::av_frame_unref(nv12);
            }
            ffi::av_frame_free(&mut (nv12 as *mut _));
        }
    }

    unsafe fn send_and_mux(&mut self, frame: *mut ffi::AVFrame) {
        let r = ffi::avcodec_send_frame(self.codec_ctx, frame);
        if r < 0 {
            warn!("vaapi: send_frame: {}", averr(r));
            return;
        }
        self.drain_packets();
    }

    unsafe fn drain_packets(&mut self) {
        loop {
            let r = ffi::avcodec_receive_packet(self.codec_ctx, self.packet);
            if r == AVERROR(EAGAIN) || r == AVERROR_EOF {
                break;
            }
            if r < 0 {
                warn!("vaapi: receive_packet: {}", averr(r));
                break;
            }
            (*self.packet).stream_index = self.stream_index;
            ffi::av_packet_rescale_ts(
                self.packet,
                (*self.codec_ctx).time_base,
                (*self.stream).time_base,
            );
            let r = ffi::av_interleaved_write_frame(self.fmt_ctx, self.packet);
            if r < 0 {
                warn!("vaapi: write_frame: {}", averr(r));
            }
            ffi::av_packet_unref(self.packet);
        }
    }

    /// Flush the encoder, finalize the mp4, and return the temp path.
    pub fn finish(mut self) -> Option<PathBuf> {
        if !self.started {
            return None;
        }
        unsafe {
            // Flush the filter graph (EOF) then the encoder.
            let _ = ffi::av_buffersrc_add_frame_flags(self.src, ptr::null_mut(), 0);
            let nv12 = ffi::av_frame_alloc();
            loop {
                let r = ffi::av_buffersink_get_frame(self.sink, nv12);
                if r < 0 {
                    break;
                }
                self.send_and_mux(nv12);
                ffi::av_frame_unref(nv12);
            }
            ffi::av_frame_free(&mut (nv12 as *mut _));
            // Drain the encoder.
            let _ = ffi::avcodec_send_frame(self.codec_ctx, ptr::null_mut());
            self.drain_packets();
            ffi::av_write_trailer(self.fmt_ctx);
        }
        let path = self.temp.clone();
        self.started = false; // Drop frees contexts; don't double-finalize.
        Some(path)
    }

    /// Drop the encoder and delete the temp file (discard path).
    pub fn discard(self) {
        let temp = self.temp.clone();
        drop(self);
        let _ = std::fs::remove_file(temp);
    }
}

impl Drop for VaapiEncoder {
    fn drop(&mut self) {
        unsafe {
            if !self.packet.is_null() {
                ffi::av_packet_free(&mut self.packet);
            }
            if !self.drm_frame.is_null() {
                ffi::av_frame_free(&mut self.drm_frame);
            }
            if !self.graph.is_null() {
                ffi::avfilter_graph_free(&mut self.graph);
            }
            if !self.codec_ctx.is_null() {
                ffi::avcodec_free_context(&mut self.codec_ctx);
            }
            if !self.fmt_ctx.is_null() {
                let mut pb = compd_avfmt_get_pb(self.fmt_ctx);
                if !pb.is_null() {
                    ffi::avio_closep(&mut pb);
                    compd_avfmt_set_pb(self.fmt_ctx, ptr::null_mut());
                }
                ffi::avformat_free_context(self.fmt_ctx);
                self.fmt_ctx = ptr::null_mut();
            }
            if !self.drm_frames.is_null() {
                ffi::av_buffer_unref(&mut self.drm_frames);
            }
            if !self.drm_device.is_null() {
                ffi::av_buffer_unref(&mut self.drm_device);
            }
        }
    }
}

/// Convenience: extract just the temp-path helper for callers.
pub fn is_render_node(_p: &Path) -> bool {
    true
}

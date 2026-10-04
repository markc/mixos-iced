//! The hardware video encoders when compd is built WITHOUT `capture-ffmpeg`.
//!
//! compd (Q-K / D11): the libav link is opt-in. Without the feature, graphics links
//! no ffmpeg at all and these stand-ins keep the encoder API callers compile
//! against. Every `start`/`spawn_*` logs once why and returns `None`, which the
//! capture driver already treats as "video unavailable". Screenshots (the GL
//! snapshot + readback path in `capture::encode`) are unaffected. The encoder
//! types are uninhabited, so the remaining methods can never run.

use smithay::backend::allocator::dmabuf::Dmabuf;
use std::convert::Infallible;
use std::path::PathBuf;

/// Why every video encoder is unavailable in this build.
pub const NOT_BUILT: &str =
    "video capture needs the capture-ffmpeg feature (this compd was built without the libav link)";

fn refuse() {
    error!("{NOT_BUILT}");
}

/// Output codec (the same three as the libav build).
#[derive(Clone, Copy, Debug)]
pub enum Codec {
    H264,
    Hevc,
    Av1,
}

/// VA-API zero-copy encoder; unavailable without `capture-ffmpeg`.
pub struct VaapiEncoder(Infallible);

impl VaapiEncoder {
    pub fn start(_dmabuf: &Dmabuf, _fps: u32, _codec: Codec) -> Option<Self> {
        refuse();
        None
    }
    pub fn dims(&self) -> (u32, u32) {
        match self.0 {}
    }
    pub fn encode(&mut self, _pts: i64) {
        match self.0 {}
    }
    pub fn finish(self) -> Option<PathBuf> {
        match self.0 {}
    }
    pub fn discard(self) {
        match self.0 {}
    }
}

/// NVENC encoder fed by CPU readback; unavailable without `capture-ffmpeg`.
pub struct NvencEncoder(Infallible);

impl NvencEncoder {
    pub fn start(_width: u32, _height: u32, _fps: u32, _cq: u32) -> Option<Self> {
        refuse();
        None
    }
    pub fn dims(&self) -> (u32, u32) {
        match self.0 {}
    }
    pub fn push(&mut self, _bgra: &[u8], _w: u32, _h: u32, _pts: i64) {
        match self.0 {}
    }
    pub fn finish(self) -> Option<PathBuf> {
        match self.0 {}
    }
    pub fn discard(self) {
        match self.0 {}
    }
}

/// NVENC zero-copy encoder; unavailable without `capture-ffmpeg`.
pub struct NvencCudaEncoder(Infallible);

impl NvencCudaEncoder {
    pub fn start(_dmabuf: &Dmabuf, _fps: u32, _codec: Codec, _cq: u32) -> Option<Self> {
        refuse();
        None
    }
    pub fn tick(&self) {
        match self.0 {}
    }
    pub fn finish(self) -> Option<PathBuf> {
        match self.0 {}
    }
    pub fn discard(self) {
        match self.0 {}
    }
}

/// The readback NVENC encode thread; unavailable without `capture-ffmpeg`.
pub struct EncoderThread(Infallible);

impl EncoderThread {
    pub fn spawn_nvenc(_width: u32, _height: u32, _fps: u32, _cq: u32) -> Option<Self> {
        refuse();
        None
    }
    pub fn send(&self, _bgra: Vec<u8>, _w: u32, _h: u32, _flip: bool, _pts: i64) {
        match self.0 {}
    }
    pub fn finish(self) -> Option<PathBuf> {
        match self.0 {}
    }
    pub fn discard(self) {
        match self.0 {}
    }
}

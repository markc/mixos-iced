//! Minimal CUDA driver-API FFI, loaded from `libcuda.so.1` at runtime.
//!
//! Hand-declared (no CUDA toolkit / `cuda.h` required — the driver library ships
//! with the NVIDIA driver). Only the entry points the zero-copy NVENC path uses
//! are bound. Struct layouts match the stable driver ABI (verified against a
//! working PoC on the hardware).

use std::ffi::{c_uint, c_void};

use crate::capture::vaapi::dynload::Lib;
use crate::load_sym;

pub type CUresult = i32;
pub type CUcontext = *mut c_void;
pub type CUstream = *mut c_void;
pub type CUarray = *mut c_void;
pub type CUgraphicsResource = *mut c_void;
pub type CUdeviceptr = u64;
/// EGLImage handle (opaque) as the driver sees it.
pub type EglImage = *mut c_void;

pub const CUDA_SUCCESS: CUresult = 0;

#[allow(dead_code)]
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CUmemorytype {
    Host = 1,
    Device = 2,
    Array = 3,
    Unified = 4,
}

/// Mirrors the driver's `CUDA_MEMCPY2D` (v2). Field order/types are ABI-fixed.
#[repr(C)]
pub struct CudaMemcpy2D {
    pub src_x_in_bytes: usize,
    pub src_y: usize,
    pub src_memory_type: CUmemorytype,
    pub src_host: *const c_void,
    pub src_device: CUdeviceptr,
    pub src_array: CUarray,
    pub src_pitch: usize,
    pub dst_x_in_bytes: usize,
    pub dst_y: usize,
    pub dst_memory_type: CUmemorytype,
    pub dst_host: *mut c_void,
    pub dst_device: CUdeviceptr,
    pub dst_array: CUarray,
    pub dst_pitch: usize,
    pub width_in_bytes: usize,
    pub height: usize,
}

impl Default for CudaMemcpy2D {
    fn default() -> Self {
        // NB: can't `mem::zeroed()` — `CUmemorytype` has no 0 discriminant, so
        // all-zero bytes are an invalid enum value (Rust panics). Callers always
        // overwrite both memory-type fields before use; `Host` is just a
        // placeholder.
        CudaMemcpy2D {
            src_x_in_bytes: 0,
            src_y: 0,
            src_memory_type: CUmemorytype::Host,
            src_host: std::ptr::null(),
            src_device: 0,
            src_array: std::ptr::null_mut(),
            src_pitch: 0,
            dst_x_in_bytes: 0,
            dst_y: 0,
            dst_memory_type: CUmemorytype::Host,
            dst_host: std::ptr::null_mut(),
            dst_device: 0,
            dst_array: std::ptr::null_mut(),
            dst_pitch: 0,
            width_in_bytes: 0,
            height: 0,
        }
    }
}

pub const CU_EGL_FRAME_TYPE_ARRAY: c_uint = 0;
pub const CU_EGL_FRAME_TYPE_PITCH: c_uint = 1;

/// Mirrors the driver's `CUeglFrame`: a union of 3 array/pointer planes followed
/// by six `unsigned int` and three enum (`int`) fields.
#[repr(C)]
pub struct CUeglFrame {
    /// `union { CUarray pArray[3]; void* pPitch[3]; }` — three pointers.
    pub plane: [*mut c_void; 3],
    pub width: c_uint,
    pub height: c_uint,
    pub depth: c_uint,
    pub pitch: c_uint,
    pub plane_count: c_uint,
    pub num_channels: c_uint,
    pub frame_type: c_uint,
    pub egl_color_format: c_uint,
    pub cu_format: i32,
}

impl Default for CUeglFrame {
    fn default() -> Self {
        unsafe { std::mem::zeroed() }
    }
}

type FnInit = unsafe extern "C" fn(c_uint) -> CUresult;
type FnCtxSetCurrent = unsafe extern "C" fn(CUcontext) -> CUresult;
type FnCtxSync = unsafe extern "C" fn() -> CUresult;
type FnEglRegister =
    unsafe extern "C" fn(*mut CUgraphicsResource, EglImage, c_uint) -> CUresult;
type FnGetEglFrame =
    unsafe extern "C" fn(*mut CUeglFrame, CUgraphicsResource, c_uint, c_uint) -> CUresult;
type FnMemcpy2D = unsafe extern "C" fn(*const CudaMemcpy2D) -> CUresult;
type FnMemcpy2DAsync = unsafe extern "C" fn(*const CudaMemcpy2D, CUstream) -> CUresult;
type FnUnregister = unsafe extern "C" fn(CUgraphicsResource) -> CUresult;

/// Resolved CUDA driver entry points.
pub struct Cuda {
    _lib: Lib,
    init: FnInit,
    ctx_set_current: FnCtxSetCurrent,
    ctx_sync: FnCtxSync,
    egl_register: FnEglRegister,
    get_egl_frame: FnGetEglFrame,
    memcpy2d: FnMemcpy2D,
    memcpy2d_async: FnMemcpy2DAsync,
    unregister: FnUnregister,
}

impl Cuda {
    /// Load `libcuda.so.1` and resolve symbols. `None` if absent (non-NVIDIA).
    pub fn load() -> Option<Cuda> {
        let lib = Lib::open("libcuda.so.1").or_else(|| Lib::open("libcuda.so"))?;
        let cuda = Cuda {
            init: load_sym!(lib, "cuInit", FnInit),
            ctx_set_current: load_sym!(lib, "cuCtxSetCurrent", FnCtxSetCurrent),
            ctx_sync: load_sym!(lib, "cuCtxSynchronize", FnCtxSync),
            egl_register: load_sym!(lib, "cuGraphicsEGLRegisterImage", FnEglRegister),
            get_egl_frame: load_sym!(lib, "cuGraphicsResourceGetMappedEglFrame", FnGetEglFrame),
            memcpy2d: load_sym!(lib, "cuMemcpy2D_v2", FnMemcpy2D),
            memcpy2d_async: load_sym!(lib, "cuMemcpy2DAsync_v2", FnMemcpy2DAsync),
            unregister: load_sym!(lib, "cuGraphicsUnregisterResource", FnUnregister),
            _lib: lib,
        };
        Some(cuda)
    }

    pub unsafe fn init(&self) -> CUresult {
        unsafe { (self.init)(0) }
    }
    pub unsafe fn set_current(&self, ctx: CUcontext) -> CUresult {
        unsafe { (self.ctx_set_current)(ctx) }
    }
    pub unsafe fn synchronize(&self) -> CUresult {
        unsafe { (self.ctx_sync)() }
    }
    pub unsafe fn register_egl(&self, img: EglImage) -> Result<CUgraphicsResource, CUresult> {
        let mut res: CUgraphicsResource = std::ptr::null_mut();
        let r = unsafe { (self.egl_register)(&mut res, img, 0) };
        if r == CUDA_SUCCESS { Ok(res) } else { Err(r) }
    }
    pub unsafe fn mapped_egl_frame(&self, res: CUgraphicsResource) -> Result<CUeglFrame, CUresult> {
        let mut f = CUeglFrame::default();
        let r = unsafe { (self.get_egl_frame)(&mut f, res, 0, 0) };
        if r == CUDA_SUCCESS { Ok(f) } else { Err(r) }
    }
    pub unsafe fn memcpy2d(&self, m: &CudaMemcpy2D) -> CUresult {
        unsafe { (self.memcpy2d)(m) }
    }
    /// Async 2D copy on `stream`. NVENC reads the frame on the ffmpeg CUDA
    /// device's stream, so issuing the copy on that same stream orders it before
    /// the encode without a host-side sync.
    pub unsafe fn memcpy2d_async(&self, m: &CudaMemcpy2D, stream: CUstream) -> CUresult {
        unsafe { (self.memcpy2d_async)(m, stream) }
    }
    pub unsafe fn unregister(&self, res: CUgraphicsResource) {
        unsafe {
            (self.unregister)(res);
        }
    }
}

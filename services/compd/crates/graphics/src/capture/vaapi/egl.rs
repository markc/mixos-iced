//! Minimal EGL FFI, loaded from `libEGL.so.1` at runtime.
//!
//! Just enough to: open a headless **device-platform** display (NVIDIA's EGL has
//! no GBM platform), and import a capture `Dmabuf` as an `EGLImage` for CUDA to
//! map. No GL context / rendering — the compositor renders into the dmabuf
//! itself; we only read it. Hand-declared (no `epoxy`/`khronos-egl` dep).

use std::ffi::{CString, c_char, c_int, c_void};

use crate::capture::vaapi::dynload::Lib;
use crate::load_sym;

pub type EGLDisplay = *mut c_void;
pub type EGLDeviceEXT = *mut c_void;
pub type EGLImage = *mut c_void;
pub type EGLAttrib = isize;
pub type EGLint = i32;
pub type EGLBoolean = u32;

const EGL_TRUE: EGLBoolean = 1;
const EGL_NONE: EGLAttrib = 0x3038;
const EGL_PLATFORM_DEVICE_EXT: u32 = 0x313F;
const EGL_EXTENSIONS: EGLint = 0x3055;
const EGL_LINUX_DMA_BUF_EXT: u32 = 0x3270;
const EGL_WIDTH: EGLAttrib = 0x3057;
const EGL_HEIGHT: EGLAttrib = 0x3056;
const EGL_LINUX_DRM_FOURCC_EXT: EGLAttrib = 0x3271;
const EGL_DMA_BUF_PLANE0_FD_EXT: EGLAttrib = 0x3272;
const EGL_DMA_BUF_PLANE0_OFFSET_EXT: EGLAttrib = 0x3273;
const EGL_DMA_BUF_PLANE0_PITCH_EXT: EGLAttrib = 0x3274;
const EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT: EGLAttrib = 0x3443;
const EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT: EGLAttrib = 0x3444;

type FnGetProcAddress = unsafe extern "C" fn(*const c_char) -> *mut c_void;
type FnQueryDevices =
    unsafe extern "C" fn(EGLint, *mut EGLDeviceEXT, *mut EGLint) -> EGLBoolean;
type FnGetPlatformDisplay =
    unsafe extern "C" fn(u32, *mut c_void, *const EGLAttrib) -> EGLDisplay;
type FnInitialize = unsafe extern "C" fn(EGLDisplay, *mut EGLint, *mut EGLint) -> EGLBoolean;
type FnQueryString = unsafe extern "C" fn(EGLDisplay, EGLint) -> *const c_char;
type FnCreateImage = unsafe extern "C" fn(
    EGLDisplay,
    *mut c_void, // EGLContext (EGL_NO_CONTEXT)
    u32,         // target
    *mut c_void, // EGLClientBuffer
    *const EGLAttrib,
) -> EGLImage;
type FnDestroyImage = unsafe extern "C" fn(EGLDisplay, EGLImage) -> EGLBoolean;

/// A headless EGL device display plus the entry points we use.
pub struct Egl {
    _lib: Lib,
    dpy: EGLDisplay,
    create_image: FnCreateImage,
    destroy_image: FnDestroyImage,
}

// Created and used on the encoder worker thread; the display handle is process
// global and the resolved fns are read-only.
unsafe impl Send for Egl {}

impl Egl {
    /// Load libEGL, enumerate the first EGL device, and open + initialize a
    /// device-platform display that supports dma_buf import. `None` on any
    /// failure (non-NVIDIA / missing extensions).
    pub fn open_device_display() -> Option<Egl> {
        let lib = Lib::open("libEGL.so.1").or_else(|| Lib::open("libEGL.so"))?;
        let get_proc: FnGetProcAddress = load_sym!(lib, "eglGetProcAddress", FnGetProcAddress);

        // Extension entry points come through eglGetProcAddress.
        let query_devices: FnQueryDevices = match sym(get_proc, "eglQueryDevicesEXT") {
            Some(p) => unsafe { std::mem::transmute::<*mut c_void, FnQueryDevices>(p) },
            None => {
                warn!("egl: no eglQueryDevicesEXT");
                return None;
            }
        };
        let get_platform_display: FnGetPlatformDisplay =
            match sym(get_proc, "eglGetPlatformDisplayEXT") {
                Some(p) => unsafe {
                    std::mem::transmute::<*mut c_void, FnGetPlatformDisplay>(p)
                },
                None => {
                    warn!("egl: no eglGetPlatformDisplayEXT");
                    return None;
                }
            };
        let initialize: FnInitialize = load_sym!(lib, "eglInitialize", FnInitialize);
        let query_string: FnQueryString = load_sym!(lib, "eglQueryString", FnQueryString);
        let create_image: FnCreateImage = load_sym!(lib, "eglCreateImage", FnCreateImage);
        let destroy_image: FnDestroyImage = load_sym!(lib, "eglDestroyImage", FnDestroyImage);

        let mut dev: EGLDeviceEXT = std::ptr::null_mut();
        let mut ndev: EGLint = 0;
        let ok = unsafe { query_devices(1, &mut dev, &mut ndev) };
        if ok != EGL_TRUE || ndev < 1 {
            warn!("egl: eglQueryDevicesEXT found no device");
            return None;
        }
        let dpy = unsafe { get_platform_display(EGL_PLATFORM_DEVICE_EXT, dev, std::ptr::null()) };
        if dpy.is_null() {
            warn!("egl: eglGetPlatformDisplay(DEVICE) failed");
            return None;
        }
        let mut major: EGLint = 0;
        let mut minor: EGLint = 0;
        if unsafe { initialize(dpy, &mut major, &mut minor) } != EGL_TRUE {
            warn!("egl: eglInitialize failed");
            return None;
        }
        // Require dma_buf import.
        let exts = unsafe { query_string(dpy, EGL_EXTENSIONS) };
        if exts.is_null()
            || !unsafe { std::ffi::CStr::from_ptr(exts) }
                .to_string_lossy()
                .contains("EGL_EXT_image_dma_buf_import")
        {
            warn!("egl: display lacks EGL_EXT_image_dma_buf_import");
            return None;
        }

        Some(Egl {
            dpy,
            create_image,
            destroy_image,
            _lib: lib,
        })
    }

    /// Destroy an `EGLImage` previously returned by [`import_dmabuf`](Self::import_dmabuf)
    /// (used when the caller manages the image lifetime itself rather than via
    /// the RAII guard).
    pub unsafe fn destroy_raw(&self, img: EGLImage) {
        unsafe {
            (self.destroy_image)(self.dpy, img);
        }
    }

    /// Import a single-plane dmabuf as an `EGLImage`. `None` on failure.
    #[allow(clippy::too_many_arguments)]
    pub fn import_dmabuf(
        &self,
        width: i32,
        height: i32,
        fourcc: u32,
        fd: c_int,
        offset: u32,
        stride: u32,
        modifier: u64,
    ) -> Option<EglImageGuard<'_>> {
        let attrs: [EGLAttrib; 17] = [
            EGL_WIDTH,
            width as EGLAttrib,
            EGL_HEIGHT,
            height as EGLAttrib,
            EGL_LINUX_DRM_FOURCC_EXT,
            fourcc as EGLAttrib,
            EGL_DMA_BUF_PLANE0_FD_EXT,
            fd as EGLAttrib,
            EGL_DMA_BUF_PLANE0_OFFSET_EXT,
            offset as EGLAttrib,
            EGL_DMA_BUF_PLANE0_PITCH_EXT,
            stride as EGLAttrib,
            EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT,
            (modifier & 0xffff_ffff) as EGLAttrib,
            EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT,
            (modifier >> 32) as EGLAttrib,
            EGL_NONE,
        ];
        let img = unsafe {
            (self.create_image)(
                self.dpy,
                std::ptr::null_mut(), // EGL_NO_CONTEXT
                EGL_LINUX_DMA_BUF_EXT,
                std::ptr::null_mut(),
                attrs.as_ptr(),
            )
        };
        if img.is_null() {
            warn!("egl: eglCreateImage(dmabuf) failed");
            return None;
        }
        Some(EglImageGuard { egl: self, img })
    }
}

/// RAII wrapper around an `EGLImage` (destroyed on drop).
pub struct EglImageGuard<'a> {
    egl: &'a Egl,
    img: EGLImage,
}

impl EglImageGuard<'_> {
    pub fn raw(&self) -> EGLImage {
        self.img
    }
}

impl Drop for EglImageGuard<'_> {
    fn drop(&mut self) {
        unsafe {
            (self.egl.destroy_image)(self.egl.dpy, self.img);
        }
    }
}

fn sym(get_proc: FnGetProcAddress, name: &str) -> Option<*mut c_void> {
    let c = CString::new(name).ok()?;
    let p = unsafe { get_proc(c.as_ptr()) };
    if p.is_null() { None } else { Some(p) }
}

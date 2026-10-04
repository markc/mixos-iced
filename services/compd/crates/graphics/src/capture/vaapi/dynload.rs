//! Minimal `dlopen`/`dlsym` wrapper.
//!
//! `libcuda` and `libEGL` are loaded at RUNTIME (not link-time) so the
//! compositor binary stays universal: on a non-NVIDIA host (or if the libraries
//! are missing) the zero-copy NVENC path simply fails to load and the caller
//! falls back to the readback encoder — exactly the existing fail-soft policy.
//! `dlopen`/`dlsym` live in libc (glibc ≥ 2.34 merged libdl in), which Rust
//! always links, so no extra link directives are needed.

use std::ffi::{CString, c_char, c_int, c_void};

unsafe extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

const RTLD_NOW: c_int = 2;

/// A `dlopen`ed shared object. Never closed (process-lifetime); the handful of
/// encoder libraries stay resident for the program's life anyway.
pub struct Lib {
    handle: *mut c_void,
}

// The handle is only used to resolve symbols (read-only after open); safe to
// move/share across threads.
unsafe impl Send for Lib {}
unsafe impl Sync for Lib {}

impl Lib {
    /// `dlopen(soname, RTLD_NOW)`. Returns `None` if the library is absent.
    pub fn open(soname: &str) -> Option<Lib> {
        let c = CString::new(soname).ok()?;
        let handle = unsafe { dlopen(c.as_ptr(), RTLD_NOW) };
        if handle.is_null() {
            return None;
        }
        Some(Lib { handle })
    }

    /// Resolve a symbol to a raw pointer, or `None` if absent. Caller transmutes
    /// to the correct `extern "C"` fn type.
    pub fn sym(&self, name: &str) -> Option<*mut c_void> {
        let c = CString::new(name).ok()?;
        let p = unsafe { dlsym(self.handle, c.as_ptr()) };
        if p.is_null() { None } else { Some(p) }
    }
}

/// Resolve `name` from `lib` and transmute to `$ty`, returning from the
/// enclosing function with `None` if the symbol is missing.
#[macro_export]
macro_rules! load_sym {
    ($lib:expr, $name:literal, $ty:ty) => {
        match $lib.sym($name) {
            Some(p) => unsafe { std::mem::transmute::<*mut std::ffi::c_void, $ty>(p) },
            None => {
                warn!(concat!("dynload: missing symbol ", $name));
                return None;
            }
        }
    };
}

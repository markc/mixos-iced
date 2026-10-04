//! render-gles: the GLES renderer's own concerns. EGL contexts, multi-GPU
//! renderer construction, element wrapping, colour handling, the dmabuf
//! format vocabulary and negotiation (`format`), and GPU/output preferences.

#[macro_use]
extern crate model;

pub mod color;
pub mod context;
pub mod element;
pub mod format;
pub mod multigpu;
pub mod preference;

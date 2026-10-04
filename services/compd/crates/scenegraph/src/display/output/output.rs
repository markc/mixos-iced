use std::os::unix::raw::dev_t;

use crate::display::backend::backend::Backend;
use smithay::{output::Output, wayland::dmabuf::DmabufFeedbackBuilder};
use render_gles::format::answer::answer;
use render_gles::format::registrar::registrar;
use render_gles::format::role::role::Role;
use world::state::Loop;

pub fn register(_loop: &mut Loop, output: &Output) {
    let _global = output.create_global::<dispatcher::state::state::Dispatch>(&_loop.state.output.display_handle);

    // Every world's Space, not just the hosted one — a world parked when a monitor
    // appears would otherwise never learn about it (`map_output_everywhere`).
    _loop.inner.map_output_everywhere(output, smithay::utils::Point::from((0, 0)));
}

/// Bind the EGL display (so smithay can import client dmabufs) and REGISTER what
/// it can take.
///
/// Registration only — the advertisement that used to follow it lives in
/// [`advertise_dmabuf`] now. They were one function, and that put an ANSWER in
/// the middle of the registration phase: the dmabuf feedback was built here,
/// while the wgpu adapters were still probing on their own thread, so it was
/// computed from a registrar that was not finished. See `Registrar::expect`.
pub fn bind_display(_loop: &mut Loop, backend_loader: &mut dyn Backend) {
    let egl_formats = backend_loader.bind_display(&_loop.state.output.display_handle);
    // The EGL import list is a DEVICE capability, registered like any other. The
    // advertisement's fallback and the log's baseline, NOT an intersection term.
    let formats = _loop.inner.kernel.get(&render_gles::format::registrar::registrar::FORMATS).clone();
    formats.register(registrar::Device::UNSPECIFIED, Role::GlesSample, egl_formats, "egl (bind_display)");
}

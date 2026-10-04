//! Buffer submission + redraw request on the winit window.

use smithay::backend::SwapBuffersError;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::{Frame, Renderer};
use smithay::backend::winit::WinitGraphicsBackend;
use smithay::utils::{Physical, Rectangle, Transform};

/// Swap with `damage` (the tracker's, never empty): the host recomposites only
/// what changed. Also requests winit's frame callback, which paces the next
/// `request_redraw`. A failed swap is returned, never a panic: the caller
/// repaints in full on a later frame (a compositor must not die because one
/// frame could not be shown).
pub fn submit(
    backend: &mut WinitGraphicsBackend<GlesRenderer>,
    damage: &[Rectangle<i32, Physical>],
) -> Result<(), SwapBuffersError> {
    backend.submit(Some(damage))
}

/// Make the window surface current again on the renderer's GL context.
///
/// Work done between frames outside a render (screencopy's mapping after the
/// swap) can leave the context current with no surface. The next frame queries
/// the buffer age before binding, and EGL refuses that query for a surface that
/// is not current (`EGL_BAD_SURFACE`, then a needless full repaint), so whoever
/// leaves the context like that restores it.
pub fn make_surface_current(
    backend: &mut WinitGraphicsBackend<GlesRenderer>,
) -> Result<(), smithay::backend::SwapBuffersError> {
    // `bind` alone does NOT make anything current: for an EGL surface
    // `GlesRenderer::bind` only names the target
    // (vendor/smithay/src/backend/renderer/gles/mod.rs, `Bind<EGLSurface>`). The
    // target is made current when a frame begins on it (`render`), so begin an
    // empty one (no clear, no draw) and finish it. `bind` still handles a pending
    // resize, as smithay asks. Safe, unlike pairing `egl_context()` with
    // `egl_surface()` by hand.
    let size = backend.window_size();
    let (renderer, mut framebuffer) = backend.bind()?;
    let frame = renderer.render(&mut framebuffer, size, Transform::Normal)?;
    frame.finish()?;
    Ok(())
}

/// [`make_surface_current`] only when the renderer's context and the window
/// surface are not already current on this thread (two `eglGetCurrent*` reads,
/// no GL work, on the common path). Called before a frame's buffer-age query,
/// so ANY surfaceless GL use between frames is covered: a commit handler's
/// import, a new surface's setup, screencopy's mapping, startup prewarm.
pub fn ensure_surface_current(
    backend: &mut WinitGraphicsBackend<GlesRenderer>,
) -> Result<(), smithay::backend::SwapBuffersError> {
    let surface_current = backend.egl_surface().is_current();
    if surface_current && backend.renderer().egl_context().is_current() {
        return Ok(());
    }
    make_surface_current(backend)
}

pub fn request_redraw(backend: &mut WinitGraphicsBackend<GlesRenderer>) {
    backend.window().request_redraw();
}

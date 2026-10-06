//! Explicit file captures and paused plain copies without a presentation surface.

use crate::{Readback, Source, file};
use smithay::backend::renderer::Bind;
use smithay::backend::renderer::element::RenderElement;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::output::Output;
use smithay::utils::{Physical, Size};

pub fn pending(output: &Output, serve_plain_copies: bool) -> bool {
    file::pending_output(output) || (serve_plain_copies && crate::pending_plain(output))
}

pub fn fail_output(output: &Output, serve_plain_copies: bool) {
    if serve_plain_copies {
        crate::fail_plain_output(output);
    } else {
        file::fail_output(output, "output cannot be rendered");
    }
}

pub fn fail_pending(serve_plain_copies: bool) {
    if serve_plain_copies {
        crate::fail_plain_pending();
    } else {
        file::fail_all("no capture frame available");
    }
}

pub fn capture<E>(
    renderer: &mut GlesRenderer,
    elements: &[E],
    is_cursor: impl Fn(&E) -> bool,
    output: &Output,
    size: Size<i32, Physical>,
    scale: f64,
    serve_plain_copies: bool,
) where
    E: RenderElement<GlesRenderer>,
{
    // A snapshot is not output damage. Damage copies stay queued for a real
    // output frame, even when an explicit file capture renders the same scene.
    let due = if serve_plain_copies {
        crate::sources_due_inner(output, None, true)
    } else {
        crate::SourcesDue::default()
    };
    let mut cursor = if due.cursor || file::pending_picture(output, true) {
        crate::render_offscreen(renderer, elements, |_| true, size, scale)
            .map_err(|err| model::warn!("capture: offscreen render failed: {err}"))
            .ok()
    } else {
        None
    };
    let mut cursorless = if due.cursorless || file::pending_picture(output, false) {
        crate::render_offscreen(
            renderer,
            elements,
            |element| !is_cursor(element),
            size,
            scale,
        )
        .map_err(|err| model::warn!("capture: cursorless render failed: {err}"))
        .ok()
    } else {
        None
    };
    let cursor_target = cursor
        .as_mut()
        .and_then(|texture| renderer.bind(texture).ok());
    let cursorless_target = cursorless
        .as_mut()
        .and_then(|texture| renderer.bind(texture).ok());
    // Do not fall back to the other picture on failure: honour cursor policy.
    if (due.cursor && cursor_target.is_none()) || (due.cursorless && cursorless_target.is_none()) {
        crate::fail_plain_output(output);
        return;
    }
    let readback = Readback {
        size,
        origin_bottom_left: false,
    };
    if let Some(framebuffer) = cursor_target.as_ref() {
        file::service(
            renderer,
            Source {
                framebuffer,
                readback,
            },
            output,
            file::CaptureSource::Offscreen,
            true,
        );
    } else if file::pending_picture(output, true) {
        file::fail_output_capture(output, "offscreen render or bind failed");
    }
    if let Some(framebuffer) = cursorless_target.as_ref() {
        file::service(
            renderer,
            Source {
                framebuffer,
                readback,
            },
            output,
            file::CaptureSource::Offscreen,
            false,
        );
    } else if file::pending_picture(output, false) {
        file::fail_output_capture(output, "cursorless offscreen render or bind failed");
    }
    if serve_plain_copies {
        crate::service_inner(
            renderer,
            cursor_target.as_ref().map(|framebuffer| Source {
                framebuffer,
                readback,
            }),
            cursorless_target.as_ref().map(|framebuffer| Source {
                framebuffer,
                readback,
            }),
            output,
            None,
            true,
        )
        .finish(renderer);
        crate::fail_plain_output(output);
    }
}

/// Render each requested window in its own target, never the output scene.
/// The host resolves the generation again at render time before building it.
pub fn capture_windows<E>(
    renderer: &mut GlesRenderer,
    output: &Output,
    mut elements: impl FnMut(
        &mut GlesRenderer,
        comp_model::capture::CaptureWindow,
    )
        -> Result<(Vec<E>, Size<i32, Physical>), comp_model::reply::ControlReply>,
) where
    E: RenderElement<GlesRenderer>,
{
    let scale = output.current_scale().fractional_scale();
    file::service_windows(output, |target| {
        let (elements, size) = elements(renderer, target)?;
        let mut texture = crate::render_offscreen(renderer, &elements, |_| true, size, scale)
            .map_err(file::capture_failed)?;
        let framebuffer = renderer
            .bind(&mut texture)
            .map_err(|err| file::capture_failed(format!("bind window capture: {err}")))?;
        let readback = Readback {
            size,
            origin_bottom_left: false,
        };
        let pixels = file::read_pixels(
            renderer,
            Source {
                framebuffer: &framebuffer,
                readback,
            },
        )
        .map_err(file::capture_failed)?;
        Ok((readback, pixels))
    });
}

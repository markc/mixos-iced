//! Winit event dispatch -> compositor lifecycle. The Focus(false) modifier
//! hack now routes through the shared `seat::modifier::clear` entry
//! (the same problem exists on native TTY switch).

use crate::scene::compose::compose::WinitRenderContext;
use smithay::backend::winit::WinitEvent;
use world::state::Loop;

pub fn route(event: &WinitEvent, state: &mut Loop, context: &mut WinitRenderContext) {
    match event {
        WinitEvent::Resized { size, scale_factor } => {
            // Root entry for the host's window size + fractional scale.
            // No stretch in the GLES present path, so keep the ORIGINAL physical output +
            // fractional scale. This fills the host window, but the output size/scale DO
            // track host DPI — intentionally a stress-test candidate for the output-size /
            // DPI-unaware code paths.
            // compd's `--scale` replaces the host's factor:
            // the gate host (weston headless) only ever reports integers.
            let scale = crate::window::factory::factory::output_scale(*scale_factor);
            info!(
                "winit(gles): resized {size:?} host-scale {scale_factor} output-scale {} (physical output, fractional scale)",
                scale.fractional_scale()
            );
            scenegraph::state::lifecycle::lifecycle::resize(context.output.clone(), *size, Some(scale));
            state.state.schedule_redraw(protocols::redraw::schedule::schedule::RedrawReason::Output);
        }
        WinitEvent::Input(input_event) => {
            // Per-event logging omitted: input is a high-frequency path.
            scenegraph::state::lifecycle::lifecycle::input(state, input_event);
            // (Lock engage is drained in the control-plane ping source — the lock
            // keybinding calls `ping_control()` — not here, so it never depends on
            // input arriving and doesn't poll per frame.)
        }
        WinitEvent::PointerLeft => {
            world::comp::scenes::pointer_motion(state, None);
            // The host cursor left the window. Drop any armed edge pan — winit sends
            // nothing more once the pointer is outside, so nothing else would stop it.
            seat::pointer::input::extent::release(state);
            // Likewise a corner engaged at the window edge.
            state.inner.comp.corners.reset();
        }
        WinitEvent::Focus(focused) => {
            info!("winit: focus={focused}");
            // Take the pointer while focused, hand it back when not.
            crate::input::capture::capture::capture(
                context.winit_backend.window(),
                *focused,
            );
            if !focused {
                world::comp::scenes::pointer_motion(state, None);
                info!("winit: focus lost — clearing held modifiers");
                seat::modifier::clear::clear::clear_held_modifiers(state);
                // A capture released mid-scroll leaves nothing to stop it.
                seat::pointer::input::extent::release(state);
            }
        }
        WinitEvent::Redraw => {
            crate::scene::compose::compose::draw(state, context);
        }
        WinitEvent::CloseRequested => {
            info!("winit: close requested — stopping compositor");
            scenegraph::state::lifecycle::lifecycle::stop(state);
        }
        _ => (),
    }
}

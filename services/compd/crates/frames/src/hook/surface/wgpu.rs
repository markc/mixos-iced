use smithay::backend::renderer::gles::GlesRenderer;
use smithay::utils::{Physical, Size};
use world::state::Loop;
use world::surface::protocol::protocol::SurfaceMessageType;

pub fn hook(state: &mut Loop, x: &mut GlesRenderer, size: Size<i32, Physical>) {
    // Nothing is constructed here. The shared iced GPU context and every world's
    // `IcedRegistry` are created by `scene.frame`'s `gpu::begin`, which runs
    // before this hook (on the first frame, over the renderer's own EGL
    // context).
    // This hook only drains the surface-message buffer into the (asserted-present)
    // registry of the focused world.
    load_incoming_buffer(state, x, size);
}

fn load_incoming_buffer(state: &mut Loop, x: &mut GlesRenderer, size: Size<i32, Physical>) {
    {
        // Drain the channel into the buffer (single slot borrow).
        let surface = state.inner.surface_mut();
        'drain: while true {
            if let Ok(ok) = surface.surface_message_buffer_channel.1.try_recv() {
                info!("Buffer item receive");
                surface.surface_message_buffer.push(ok);
            } else {
                break 'drain;
            }
        }
    }

    // Takes the buffer by draining it
    let taken = std::mem::take(&mut state.inner.surface_mut().surface_message_buffer);

    // Delegate actions
    for item in taken {
        info!("Delegate message...: {:?}", item);
        match item.message {
            SurfaceMessageType::Capture(capture_message) => {
                recorder::interface::interface::handle(state, x, capture_message)
            }
        }
    }
}

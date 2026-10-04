use smithay::backend::input::{InputBackend, InputEvent};
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::utils::{Buffer, Logical, Physical, Point, Rectangle, Scale, Size, Transform};
use world::state::Loop;
use crate::display::output::backend::Backend;
use crate::display::output::output;

pub fn initialize(
    _loop: &mut Loop,
    output: &Output,
    display_handle: &DisplayHandle,
    backend: &mut dyn Backend,
) -> OutputDamageTracker {
    // Create the backend. This creates a winit/udev instance
    // let (backend) = backend_loader.load();

    // Registers the output in smithay space. ( monitor _
    output::register(_loop, output);

    // Creates the damage tracker
    let output_damage_tracker = OutputDamageTracker::from_output(&output);

    // Allows smithay to render DMABUF imports from clients, and registers the EGL
    // import set with the format layer.
    //
    // ADVERTISING them is a separate step now (`output::advertise_dmabuf`, driven
    // by the loader). Building the feedback here meant answering the format layer
    // in the middle of the registration phase, while the wgpu adapters were still
    // probing — so the advertisement was computed from an unfinished registrar.
    output::bind_display(_loop, backend);

    // Damage tracker created for the output(monitor)
    output_damage_tracker
}

pub fn input<I: InputBackend>(_loop: &mut Loop, input_event: &InputEvent<I>) {
    // The human-input point: the comp policy's host passthrough gate and
    // human-activity record (`input.seats`, `last_origin`).
    if !world::comp::injection::human_input(_loop, input_event) {
        return;
    }
    // return self._loop..process_input_event(input_event);
    seat::delegate::delegate::process_input_event(_loop, input_event)
}

pub fn stop(_loop: &mut Loop) {
    _loop.inner.loader.loop_signal.stop();
}

pub fn resize(
    output: Output,
    size: Size<i32, Physical>,
    scale_factor: Option<smithay::output::Scale>,
) {
    output.change_current_state(
        Some(Mode {
            size,
            refresh: 60_000,
        }),
        None,
        scale_factor,
        None,
    );
}

use std::sync::mpsc;
use ui::{HandleId, IcedHandle, IcedRegistry};

pub struct SurfaceState {
    // The wgpu GPU context is shared driver data (kernel `ICED_CONTEXT`), not
    // per-world. This world owns only its registry, built lazily from it.
    pub registry: Option<IcedRegistry>,

    pub test_dmabuf_done: bool,
    pub test_example_done: bool,

    pub surface_message_buffer: Vec<crate::surface::protocol::protocol::SurfaceMessage>,
    pub surface_message_buffer_channel: (mpsc::Sender<crate::surface::protocol::protocol::SurfaceMessage>, mpsc::Receiver<crate::surface::protocol::protocol::SurfaceMessage>)
}


impl SurfaceState {
    pub fn new() -> Self {
        return Self {
            surface_message_buffer: vec!(),
            surface_message_buffer_channel: mpsc::channel(),
            registry: None,
            test_dmabuf_done: false,
            test_example_done: false,
        };
    }
}

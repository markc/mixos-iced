use smithay::desktop::Window;
use crate::window::lifecycle::event::event::WindowLifecycleEvent;

pub struct WindowLifecycle{
    pub incoming: Vec<WindowLifecycleEvent>
}

impl WindowLifecycle {
    pub fn new() -> Self {
        return Self {
            incoming: vec!(),
        }
    }
}
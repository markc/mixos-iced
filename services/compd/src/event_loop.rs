use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;
use world::state::Loop;
use dispatcher::state::state::Dispatch;

pub fn create<'a>() -> Result<(EventLoop<'a, Loop>, Display<Dispatch>), Box<dyn std::error::Error>> {
    let event_loop = EventLoop::try_new()?;
    let display = Display::new()?;

    Ok((event_loop, display))
}

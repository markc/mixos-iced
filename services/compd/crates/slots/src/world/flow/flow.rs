use crate::channel::router::base::ChannelRouter;
use crate::input::event::base::{InputEvent, InputFlow};
use crate::storage::slot::base::Storage;
use crate::trait_::system::base::{BufferCx, Receiver, System, SystemCx};
use std::any::Any;
use std::collections::VecDeque;

/// Cascade guard: more deliveries than this in one drain is a feedback loop.
const DISPATCH_LIMIT: usize = 10_000;
/// The world's lifecycle driving, over its parts. Buffers flush after every call.
pub struct Parts<'w> {
    pub id: uuid::Uuid,
    pub name: &'static str,
    pub storage: &'w mut Storage,
    pub channels: &'w mut ChannelRouter,
    pub systems: &'w mut Vec<Box<dyn System>>,
    pub receivers: &'w mut Vec<Receiver>,
    pub input_order: &'w mut Vec<usize>,
    pub buffers: &'w mut VecDeque<(usize, Box<dyn Any>)>,
}

pub fn flush(parts: &mut Parts, kernel: &Storage) {
    while let Some((index, message)) = parts.buffers.pop_front() {
        let mut cx = BufferCx { storage: parts.storage, kernel, channels: parts.channels, world: parts.id };
        parts.systems[index].buffer(&mut cx, message);
    }
    // End of buffer reduction (path 1): if a `transact()` flagged this world and its
    // commit is due, persist its changed entries. Cheap timestamp check otherwise.
    if crate::persist::mark::base::take_if_due(parts.id) {
        crate::persist::flush::base::commit_world(
            parts.id, parts.storage, parts.systems,
        );
    }
}
pub fn each_system(
    parts: &mut Parts,
    kernel: &Storage,
    mut platform: Option<&mut dyn Any>,
    mut seat: Option<&mut dyn Any>,
    mut f: impl FnMut(&mut dyn System, &mut SystemCx),
) {
    for index in 0..parts.systems.len() {
        let platform = match &mut platform { Some(p) => Some(&mut **p), None => None };
        let seat = match &mut seat { Some(s) => Some(&mut **s), None => None };
        {
            let mut cx = SystemCx::new(parts.storage, kernel, parts.channels, platform, seat, parts.buffers, index);
            f(parts.systems[index].as_mut(), &mut cx);
        }
        flush(parts, kernel);
    }
}
/// Drain events to every bound listener (by ref); cascades settle in-drain.
pub fn dispatch(parts: &mut Parts, kernel: &Storage) {
    let mut delivered = 0;
    loop {
        let Some((channel_id, message)) = parts.channels.pop() else { return };
        for i in 0..parts.receivers.len() {
            if parts.receivers[i].channel_id != channel_id {
                continue;
            }
            let index = parts.receivers[i].system_index;
            {
                let mut cx = SystemCx::new(parts.storage, kernel, parts.channels, None, None, parts.buffers, index);
                (parts.receivers[i].invoke)(parts.systems[index].as_mut(), &mut cx, message.as_ref());
            }
            flush(parts, kernel);
            delivered += 1;
            if delivered > DISPATCH_LIMIT {
                panic!("world '{}': channel cascade exceeded {DISPATCH_LIMIT} deliveries", parts.name);
            }
        }
    }
}
/// Synchronous bus traversal: layer order, stop on Consume; flush per system.
/// `seat` (the wayland `Dispatch`) and `platform` (the live window Space) are lent
/// disjointly so a Pass-1 input system can hit-test + seat-op synchronously
///
pub fn input(
    parts: &mut Parts,
    kernel: &Storage,
    event: &InputEvent,
    mut platform: Option<&mut dyn Any>,
    mut seat: Option<&mut dyn Any>,
) -> InputFlow {
    for i in 0..parts.input_order.len() {
        let index = parts.input_order[i];
        let platform = match &mut platform { Some(p) => Some(&mut **p), None => None };
        let seat = match &mut seat { Some(s) => Some(&mut **s), None => None };
        let flow = {
            let mut cx = SystemCx::new(parts.storage, kernel, parts.channels, platform, seat, parts.buffers, index);
            parts.systems[index].input(&mut cx, event)
        };
        flush(parts, kernel);
        if flow == InputFlow::Consume {
            return InputFlow::Consume;
        }
    }
    InputFlow::Pass
}

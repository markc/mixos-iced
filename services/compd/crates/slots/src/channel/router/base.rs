use crate::channel::token::base::ChannelTx;
use std::any::Any;
use std::collections::VecDeque;

/// Per-world message queue. A single FIFO preserves global send order across
/// channels; messages are dispatched when the world drains (before the next
/// frame plan). Deliberately platform-free: the orchestration layer installs a
/// wake hook (e.g. a calloop `Ping`) so a send wakes the event loop.
#[derive(Default)]
pub struct ChannelRouter {
    queue: VecDeque<(usize, Box<dyn Any>)>,
    wake: Option<Box<dyn Fn()>>,
}

impl ChannelRouter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Install the loop wake hook. Called once by the driving layer.
    pub fn set_wake(&mut self, wake: Box<dyn Fn()>) {
        self.wake = Some(wake);
    }

    /// Announce an event — fire and forget; requires the channel's (single)
    /// sender token. Never delivers synchronously.
    pub fn send<M: 'static>(&mut self, tx: &'static ChannelTx<M>, message: M) {
        self.queue.push_back((tx.channel.ensure_id(), Box::new(message)));
        if let Some(wake) = &self.wake {
            wake();
        }
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// Pop the oldest queued message as `(channel_id, boxed message)`.
    /// The world's dispatch loop downcasts via the bound receiver.
    pub fn pop(&mut self) -> Option<(usize, Box<dyn Any>)> {
        self.queue.pop_front()
    }
}

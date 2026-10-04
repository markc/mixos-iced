// The bounded command channel from the port worker to the engine: a std
// bounded channel plus a waker the engine supplies (a calloop `Ping`, an
// eventfd write, a condvar), so every send wakes the engine's loop. The
// engine never polls it.

//! The port → engine command channel.

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};

use crate::port::PortCommand;

/// Wakes the engine's loop. Called once per successful send.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// A waker that does nothing (tests, or an engine that services on its own
/// cadence after every dispatch).
pub fn no_waker() -> Waker {
    Arc::new(|| {})
}

/// The worker's end: a bounded send that wakes the engine.
#[derive(Clone)]
pub struct CommandSender {
    sender: SyncSender<PortCommand>,
    waker: Waker,
}

impl CommandSender {
    /// A refused command comes back boxed (only the failure path allocates).
    pub fn try_send(&self, command: PortCommand) -> Result<(), TrySendError<Box<PortCommand>>> {
        self.sender.try_send(command).map_err(|error| match error {
            TrySendError::Full(command) => TrySendError::Full(Box::new(command)),
            TrySendError::Disconnected(command) => TrySendError::Disconnected(Box::new(command)),
        })?;
        (self.waker)();
        Ok(())
    }
}

/// The engine's end.
pub struct CommandSource {
    receiver: Receiver<PortCommand>,
}

impl CommandSource {
    pub fn try_recv(&self) -> Result<PortCommand, TryRecvError> {
        self.receiver.try_recv()
    }
}

/// A channel of `capacity` commands.
pub fn command_channel(capacity: usize, waker: Waker) -> (CommandSender, CommandSource) {
    let (sender, receiver) = mpsc::sync_channel(capacity);
    (CommandSender { sender, waker }, CommandSource { receiver })
}

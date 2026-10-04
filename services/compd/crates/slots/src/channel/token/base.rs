use std::marker::PhantomData;
use std::sync::atomic::{AtomicUsize, Ordering};

const UNREGISTERED: usize = usize::MAX;

/// Global channel-id allocator (separate space from storage slot ids).
static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

/// A channel token. Declared as a `static` by the crate that owns the message
/// type via [`define_channel!`]; anyone importing the token can send, the owning
/// system binds the receiver at `register()` time.
/// The CONSTANT REFERENCE is the identity — no string identifiers.
pub struct Channel<M> {
    id: AtomicUsize,
    _marker: PhantomData<fn() -> M>,
}

impl<M> Channel<M> {
    pub const fn new() -> Self {
        Self { id: AtomicUsize::new(UNREGISTERED), _marker: PhantomData }
    }

    pub fn id(&self) -> Option<usize> {
        match self.id.load(Ordering::Acquire) {
            UNREGISTERED => None,
            id => Some(id),
        }
    }

    /// The channel id, allocating one on first use (idempotent, thread-safe).
    pub fn ensure_id(&self) -> usize {
        let current = self.id.load(Ordering::Acquire);
        if current != UNREGISTERED {
            return current;
        }
        let fresh = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        match self.id.compare_exchange(UNREGISTERED, fresh, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => fresh,
            Err(won) => won,
        }
    }
}

/// Send capability for a channel. A channel has exactly ONE sender: the
/// module where the event originates keeps this `pub(crate)` — enforced by
/// visibility, mirroring storage write tokens. Channels carry EVENTS (what
/// happened), never commands; any number of listeners react.
pub struct ChannelTx<M: 'static> {
    pub channel: &'static Channel<M>,
}

impl<M> ChannelTx<M> {
    pub const fn new(channel: &'static Channel<M>) -> Self {
        Self { channel }
    }
}

/// Declare an event channel: a public token (listen with it) and a
/// crate-private sender (only the owning module announces the event).
///
/// ```ignore
/// define_channel!(pub GROUP_UPDATED, GROUP_UPDATED_TX: GroupUpdated);
/// ```
#[macro_export]
macro_rules! define_channel {
    (pub $name:ident, $tx:ident : $M:ty) => {
        pub static $name: $crate::channel::token::base::Channel<$M> = $crate::channel::token::base::Channel::new();
        pub(crate) static $tx: $crate::channel::token::base::ChannelTx<$M> =
            $crate::channel::token::base::ChannelTx::new(&$name);
    };
}

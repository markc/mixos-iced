use slots::channel::router::base::ChannelRouter;
use slots::channel::token::base::{Channel, ChannelTx};
use std::any::Any;
use std::rc::Rc;

/// TRANSITIONAL: kernel channels whose receivers are legacy `fn(&mut L, M)`
/// handlers that still need the whole loop object. Lets producers go
/// fire-and-forget TODAY; each receiver migrates into a proper world system
/// once its state moves into storage, and this module dies with the god object.
///
/// Generic over the loop type so it can live inside it without a dep cycle.
pub struct LegacyBus<L> {
    pub router: ChannelRouter,
    handlers: Vec<(usize, Rc<dyn Fn(&mut L, &dyn Any)>)>,
}

impl<L> Default for LegacyBus<L> {
    fn default() -> Self {
        Self { router: ChannelRouter::new(), handlers: Vec::new() }
    }
}

impl<L: 'static> LegacyBus<L> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Announce an event — fire and forget; requires the (single) sender token.
    pub fn send<M: 'static>(&mut self, tx: &'static ChannelTx<M>, message: M) {
        self.router.send(tx, message);
    }

    /// Listen on an event channel; events fan out to every listener by ref.
    pub fn register<M: 'static>(&mut self, channel: &'static Channel<M>, handler: fn(&mut L, &M)) {
        self.handlers.push((
            channel.ensure_id(),
            Rc::new(move |l: &mut L, message: &dyn Any| {
                let message = message
                    .downcast_ref::<M>()
                    .unwrap_or_else(|| panic!("message type mismatch on <{}>", std::any::type_name::<M>()));
                handler(l, message);
            }),
        ));
    }
}

/// Drain the bus owned by `L` itself: `get` projects to the bus, handlers run
/// with the full `&mut L`. Pop-then-call keeps the borrows disjoint.
pub fn drain<L>(l: &mut L, get: fn(&mut L) -> &mut LegacyBus<L>) {
    loop {
        let Some((id, message)) = get(l).router.pop() else { return };
        // Fan-out: clone the matching handlers first to end the bus borrow.
        let handlers: Vec<_> =
            get(l).handlers.iter().filter(|(h, _)| *h == id).map(|(_, f)| f.clone()).collect();
        for handler in handlers {
            handler(l, message.as_ref());
        }
    }
}

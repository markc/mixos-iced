use crate::channel::token::base::Channel;
use crate::persist::document::entry::base::DocumentEntry;
use crate::persist::entry::base::PersistEntry;
use crate::input::bus::base::InputBus;
use crate::input::event::base::{InputEvent, InputFlow};
use crate::input::layer::base::InputLayer;
use crate::storage::slot::base::Storage;
use crate::world::frame::base::{FramePlan, FrameTick};
pub use crate::trait_::cx::cx::{BufferCx, SystemCx};
use std::any::Any;

/// A unit of behavior living in a world. Lifecycle: `register` → `start`
/// (after ALL registers) → `on_enable`/`on_disable` around world swaps;
/// per frame: `input`, `update`, `draw` — all with READ-ONLY storage.
/// `buffer` applies this system's queued mutation messages.
pub trait System: Any {
    fn name(&self) -> &'static str;
    fn register(&mut self, builder: &mut WorldBuilder);
    fn start(&mut self, _cx: &mut SystemCx) {}
    fn on_enable(&mut self, _cx: &mut SystemCx) {}
    fn on_disable(&mut self, _cx: &mut SystemCx) {}
    fn input(&mut self, _cx: &mut SystemCx, _event: &InputEvent) -> InputFlow {
        InputFlow::Pass
    }
    fn update(&mut self, _cx: &mut SystemCx, _tick: &FrameTick) {}
    fn draw(&mut self, _cx: &mut SystemCx, _plan: &mut FramePlan) {}
    /// Apply one self-addressed message (downcast to this system's buffer
    /// type). May read/mutate own slots and announce events; may NOT write
    /// further buffer messages.
    fn buffer(&mut self, _cx: &mut BufferCx, _message: Box<dyn Any>) {}

    /// Single-value storages this system persists (built with `persist_version!`).
    /// Rehydrated at world start; written when they change. Default: none.
    fn persist(&self) -> &'static [&'static PersistEntry] {
        &[]
    }

    /// Collection storages this system persists as filesystem tables (built with
    /// `define_document!`). Rehydrated at world start; per-record put/delete/re-link
    /// when they change. Default: none.
    fn documents(&self) -> &'static [&'static DocumentEntry] {
        &[]
    }
}

/// A bound channel listener; events fan out by reference to every listener.
pub struct Receiver {
    pub channel_id: usize,
    pub system_index: usize,
    pub invoke: Box<dyn Fn(&mut dyn System, &mut SystemCx, &dyn Any)>,
}

/// Collects a world's wiring during `System::register`.
#[derive(Default)]
pub struct WorldBuilder {
    pub storage: Storage,
    pub receivers: Vec<Receiver>,
    pub input: InputBus,
    current_system: usize,
}

impl WorldBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set by the world while iterating systems; bindings attach to this index.
    pub fn set_current_system(&mut self, index: usize) {
        self.current_system = index;
    }

    /// Claim input at a priority layer (see input.layer constants).
    pub fn input(&mut self, layer: InputLayer) {
        self.input.register(layer, self.current_system);
    }

    /// Listen on an event channel; the handler observes events by reference.
    pub fn receive<S, M>(&mut self, channel: &'static Channel<M>, handler: fn(&mut S, &mut SystemCx, &M))
    where
        S: System,
        M: 'static,
    {
        let system_index = self.current_system;
        self.receivers.push(Receiver {
            channel_id: channel.ensure_id(),
            system_index,
            invoke: Box::new(move |system, cx, message| {
                let system = (system as &mut dyn Any)
                    .downcast_mut::<S>()
                    .unwrap_or_else(|| panic!("listener for <{}> bound to a different system type", std::any::type_name::<M>()));
                let message = message
                    .downcast_ref::<M>()
                    .unwrap_or_else(|| panic!("message type mismatch on channel <{}>", std::any::type_name::<M>()));
                handler(system, cx, message);
            }),
        });
    }
}

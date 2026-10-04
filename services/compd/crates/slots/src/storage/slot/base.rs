use crate::storage::token::base::{Token, TokenMut};
use std::any::Any;

/// Type-erased slot store. One per world (plus one global store on the
/// orchestrator). Slots are indexed by the process-wide token id, so a lookup
/// is an array index — no hashing.
#[derive(Default)]
pub struct Storage {
    slots: Vec<Option<Box<dyn Any>>>,
}

impl Storage {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a slot. Panics if this storage already holds the token's slot —
    /// a slot has exactly one owner.
    pub fn insert<T: 'static>(&mut self, token: &'static Token<T>, value: T) {
        let id = token.ensure_id();
        if self.slots.len() <= id {
            self.slots.resize_with(id + 1, || None);
        }
        let slot = &mut self.slots[id];
        if slot.is_some() {
            panic!("storage slot <{}> registered twice", std::any::type_name::<T>());
        }
        *slot = Some(Box::new(value));
    }

    pub fn contains<T: 'static>(&self, token: &'static Token<T>) -> bool {
        self.try_get(token).is_some()
    }

    pub fn try_get<T: 'static>(&self, token: &'static Token<T>) -> Option<&T> {
        let id = token.id()?;
        self.slots.get(id)?.as_ref()?.downcast_ref::<T>()
    }

    /// Fallible mutable read: `Some` only if this storage holds the slot. Use
    /// when a slot is genuinely optional per-world (e.g. a background system a
    /// given world doesn't run) — the focused-world accessors lean on this to
    /// degrade gracefully instead of panicking on a world that lacks the slot.
    pub fn try_get_mut<T: 'static>(&mut self, token: &'static TokenMut<T>) -> Option<&mut T> {
        let id = token.read.id()?;
        self.slots.get_mut(id)?.as_mut()?.downcast_mut::<T>()
    }

    /// Read a slot. Panics (with the token name) if absent — a missing slot is
    /// a wiring bug, not a runtime condition.
    pub fn get<T: 'static>(&self, token: &'static Token<T>) -> &T {
        match self.try_get(token) {
            Some(value) => value,
            None => panic!("storage slot <{}> is not registered in this world", std::any::type_name::<T>()),
        }
    }

    /// Mutate a slot. Requires the write token, which only the owning crate
    /// can name — that visibility rule is the entire mutation policy.
    pub fn get_mut<T: 'static>(&mut self, token: &'static TokenMut<T>) -> &mut T {
        let name = std::any::type_name::<T>();
        let id = match token.read.id() {
            Some(id) => id,
            None => panic!("storage slot <{name}> is not registered in this world"),
        };
        match self.slots.get_mut(id).and_then(|s| s.as_mut()).and_then(|s| s.downcast_mut::<T>()) {
            Some(value) => value,
            None => panic!("storage slot <{name}> is not registered in this world"),
        }
    }
}

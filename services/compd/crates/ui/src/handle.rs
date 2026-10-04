//! Handles into the `IcedRegistry`.
//!
//! Two flavors:
//! - `HandleId`: opaque, untyped identifier. Used internally and for
//!   operations that don't need the UI type (hit-test results, untyped
//!   event dispatch).
//! - `IcedHandle<U>`: typed wrapper. Required for type-safe message
//!   dispatch.
//!
//! Both are `Copy + Eq + Hash`. Cheap to store and pass around.

use std::marker::PhantomData;

use crate::engine::IcedUi;

/// Opaque, type-erased instance identifier. Stable for the lifetime of
/// the instance — destroying and recreating returns a new id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct HandleId(pub u64);

impl HandleId {
    pub fn raw(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for HandleId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "iced#{}", self.0)
    }
}

/// Typed handle. Carries the UI type as a phantom so that operations
/// requiring `U::Message` can be checked at compile time.
///
/// Drop semantics: dropping the handle does NOT destroy the instance.
/// Call `IcedRegistry::destroy` explicitly to release resources.
pub struct IcedHandle<U: IcedUi> {
    pub id: HandleId,
    _marker: PhantomData<fn() -> U>,
}

impl<U: IcedUi> IcedHandle<U> {
    pub(crate) fn new(id: HandleId) -> Self {
        Self {
            id,
            _marker: PhantomData,
        }
    }

    /// Re-attach a type tag to a previously-stripped `HandleId`. The type is
    /// not verified here — message dispatch (`dispatch_message` /
    /// `instance_mut`) re-checks the concrete type at the registry and fails
    /// gracefully on mismatch, so an incorrect `U` is safe (it just won't
    /// resolve), never unsound. Useful for callers that store the untyped
    /// `HandleId` (e.g. to avoid a crate dependency on the UI type) and later
    /// need a typed handle.
    pub fn from_id(id: HandleId) -> Self {
        Self::new(id)
    }

    /// Strip the type tag. Useful when passing to untyped APIs.
    pub fn untyped(self) -> HandleId {
        self.id
    }
}

impl<U: IcedUi> Clone for IcedHandle<U> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<U: IcedUi> Copy for IcedHandle<U> {}

impl<U: IcedUi> PartialEq for IcedHandle<U> {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl<U: IcedUi> Eq for IcedHandle<U> {}

impl<U: IcedUi> std::hash::Hash for IcedHandle<U> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state)
    }
}

impl<U: IcedUi> std::fmt::Debug for IcedHandle<U> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "IcedHandle<{}>({})", std::any::type_name::<U>(), self.id)
    }
}

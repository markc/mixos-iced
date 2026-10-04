use std::marker::PhantomData;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Sentinel id of a token that has not been registered into any storage yet.
const UNREGISTERED: usize = usize::MAX;

/// Global slot-id allocator. Every token gets one id for the whole process,
/// assigned on first registration; per-world storages index by it directly.
static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

/// Read token for a storage slot of type `T`. Declared as a `static` by the
/// owning crate via [`define_storage!`]; importing the token grants read access.
/// The CONSTANT REFERENCE is the identity — no string identifiers; the slot
/// id is an internal detail assigned on first registration.
pub struct Token<T> {
    id: AtomicUsize,
    _marker: PhantomData<fn() -> T>,
}

impl<T> Token<T> {
    pub const fn new() -> Self {
        Self { id: AtomicUsize::new(UNREGISTERED), _marker: PhantomData }
    }

    /// The slot id, if this token has been registered into any storage.
    pub fn id(&self) -> Option<usize> {
        match self.id.load(Ordering::Acquire) {
            UNREGISTERED => None,
            id => Some(id),
        }
    }

    /// The slot id, allocating one on first call (idempotent, thread-safe).
    pub fn ensure_id(&self) -> usize {
        let current = self.id.load(Ordering::Acquire);
        if current != UNREGISTERED {
            return current;
        }
        let fresh = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        match self.id.compare_exchange(UNREGISTERED, fresh, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => fresh,
            Err(won) => won, // another thread registered first; use its id
        }
    }
}

/// Write token: grants mutable access to the slot. The owning crate keeps this
/// `pub(crate)`, which is what enforces "only the system itself updates its
/// storage" — purely by visibility, no runtime checks.
pub struct TokenMut<T: 'static> {
    pub read: &'static Token<T>,
}

impl<T> TokenMut<T> {
    pub const fn new(read: &'static Token<T>) -> Self {
        Self { read }
    }
}

/// Declare a storage slot: a public read token and a crate-private write token.
///
/// ```ignore
/// define_storage!(pub CAMERA, CAMERA_MUT: CameraData);
/// ```
#[macro_export]
macro_rules! define_storage {
    (pub $read:ident, $write:ident : $T:ty) => {
        pub static $read: $crate::storage::token::base::Token<$T> = $crate::storage::token::base::Token::new();
        pub(crate) static $write: $crate::storage::token::base::TokenMut<$T> =
            $crate::storage::token::base::TokenMut::new(&$read);
    };
}

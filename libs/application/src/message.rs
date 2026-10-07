// SPDX-License-Identifier: MIT OR Apache-2.0
//! A shared consumable payload for Clone/Eq UI message frameworks. Clones name
//! the same delivery; equality is delivery identity, never resource equality.
use std::{fmt, sync::{Arc, Mutex}};

pub struct Once<T>(Arc<Mutex<Option<T>>>);
impl<T> Once<T> {
    pub fn new(value: T) -> Self { Self(Arc::new(Mutex::new(Some(value)))) }
    pub fn take(&self) -> Option<T> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take()
    }
}
impl<T> Clone for Once<T> {
    fn clone(&self) -> Self { Self(Arc::clone(&self.0)) }
}
impl<T> PartialEq for Once<T> {
    fn eq(&self, other: &Self) -> bool { Arc::ptr_eq(&self.0, &other.0) }
}
impl<T> Eq for Once<T> {}
impl<T> fmt::Debug for Once<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Once").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clones_share_one_nonclone_payload_and_identity() {
        struct Payload(u32);
        let delivery = Once::new(Payload(42));
        let clone = delivery.clone();
        assert_eq!(delivery, clone);
        assert_ne!(delivery, Once::new(Payload(42)));
        let left = std::thread::spawn(move || clone.take());
        let right = delivery.take();
        let left = left.join().unwrap();
        assert_eq!(left.is_some() as usize + right.is_some() as usize, 1);
        assert_eq!(left.or(right).unwrap().0, 42);
        assert!(delivery.take().is_none());
    }
}

// SPDX-License-Identifier: MIT
//! Non-default native acceptance scope; no application state or native leases.

use std::{cell::Cell, marker::PhantomData, rc::Rc};

thread_local! {
    static AFTER_COMMIT: Cell<Option<bool>> = const { Cell::new(None) };
}

/// A one-draw, one-thread fault scope. Drop restores the ordinary renderer.
pub struct Scope(PhantomData<Rc<()>>);

impl Scope {
    pub fn arm() -> Result<Self, &'static str> {
        AFTER_COMMIT.with(|slot| {
            if slot.get().is_some() {
                return Err("native fault scope already occupied");
            }
            slot.set(Some(false));
            Ok(Self(PhantomData))
        })
    }

    pub fn consumed(&self) -> bool {
        AFTER_COMMIT.with(|slot| slot.get() == Some(true))
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        AFTER_COMMIT.with(|slot| slot.set(None));
    }
}

/// Consume only after the real buffer commit succeeded, once in this draw.
pub fn take_after_commit_failure() -> bool {
    AFTER_COMMIT.with(|slot| {
        if slot.get() != Some(false) {
            return false;
        }
        slot.set(Some(true));
        true
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fault_is_once_scoped_and_cannot_escape_to_another_thread() {
        assert!(!take_after_commit_failure());
        let scope = Scope::arm().unwrap();
        assert!(Scope::arm().is_err());
        assert!(
            !std::thread::spawn(take_after_commit_failure)
                .join()
                .unwrap()
        );
        assert!(take_after_commit_failure());
        assert!(scope.consumed());
        assert!(!take_after_commit_failure());
        drop(scope);
        assert!(!take_after_commit_failure());
        let retry = Scope::arm().unwrap();
        assert!(!retry.consumed());
    }
}

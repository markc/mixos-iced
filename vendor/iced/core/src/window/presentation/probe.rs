// SPDX-License-Identifier: MIT
//! Non-default native acceptance scope; no application state or native leases.

use std::{cell::Cell, marker::PhantomData, rc::Rc};

thread_local! {
    static FAULT: Cell<Option<(FailurePoint, bool)>> = const { Cell::new(None) };
}

/// Actual renderer boundary where a one-draw acceptance error is injected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailurePoint {
    /// After the actual pre-present hook, before buffer submission.
    BeforeCommit,
    /// After a successful real buffer submission.
    AfterCommit,
}

/// A one-draw, one-thread fault scope. Drop restores the ordinary renderer.
pub struct Scope(PhantomData<Rc<()>>);

impl Scope {
    pub fn arm() -> Result<Self, &'static str> {
        Self::arm_at(FailurePoint::AfterCommit)
    }

    /// Arm an explicit boundary; ordinary `arm` retains AfterCommit behaviour.
    pub fn arm_at(point: FailurePoint) -> Result<Self, &'static str> {
        FAULT.with(|slot| {
            if slot.get().is_some() {
                return Err("native fault scope already occupied");
            }
            slot.set(Some((point, false)));
            Ok(Self(PhantomData))
        })
    }

    pub fn consumed(&self) -> bool {
        FAULT.with(|slot| slot.get().is_some_and(|(_, consumed)| consumed))
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        FAULT.with(|slot| slot.set(None));
    }
}

/// Consume only after the real buffer commit succeeded, once in this draw.
pub fn take_after_commit_failure() -> bool {
    take_failure(FailurePoint::AfterCommit)
}

/// Consume after the real pre-present hook but before the actual buffer commit.
pub fn take_before_commit_failure() -> bool {
    take_failure(FailurePoint::BeforeCommit)
}

fn take_failure(point: FailurePoint) -> bool {
    FAULT.with(|slot| {
        if slot.get() != Some((point, false)) {
            return false;
        }
        slot.set(Some((point, true)));
        true
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn before_commit_scope_cannot_be_consumed_at_the_after_commit_boundary() {
        let scope = Scope::arm_at(FailurePoint::BeforeCommit).unwrap();
        assert!(!take_after_commit_failure());
        assert!(!scope.consumed());
        assert!(take_before_commit_failure());
        assert!(scope.consumed());
        assert!(!take_before_commit_failure());
        assert!(!take_after_commit_failure());
        drop(scope);
        assert!(!take_before_commit_failure());
        let ordinary = Scope::arm().unwrap();
        assert!(!take_before_commit_failure());
        assert!(!ordinary.consumed());
        assert!(take_after_commit_failure());
    }

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

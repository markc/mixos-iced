// SPDX-License-Identifier: MIT OR Apache-2.0
//! Bounded participant observations, supplied by existing native owners.
//!
//! Registration is not application, and application is not presentation.
//! Visibility comes from compositor lifecycle facts; native frame history comes
//! from the application's actual `frames::Handle`. No observation requests a
//! draw, starts a worker or creates an authority. A deadline means nonresponsive
//! within that observation budget, not a diagnosis that a process is hung.

use crate::frames::{FrameOutcome, FrameStamp, Handle};
use settings::consumer::SnapshotIdentity;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Visibility {
    Visible,
    Hidden,
    Minimised,
    InactiveSession,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scope {
    pub service: String,
    pub process_instance: u64,
    pub session_generation: u64,
    pub surface: u64,
    pub surface_incarnation: u64,
}

/// Minted by this observer, never accepted from a Bus request body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Token(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Applying,
    Applied,
    AwaitingPresentation,
    Presented,
    Hidden,
    Minimised,
    InactiveSession,
    Nonresponsive,
    Closed,
    Unsupported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Pending,
    Accepted,
    Applied,
    Presented,
}

/// All values share an explicitly named host monotonic clock. Transport time,
/// wall time and an unrelated presentation clock are never subtracted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Clock {
    /// Native authenticated host identity, never supplied by application JSON.
    pub host_id: String,
    pub boot_id: String,
    pub clock_id: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Accepted {
    pub identity: SnapshotIdentity,
    pub timing: Option<Timing>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Timing {
    pub clock: Clock,
    pub nanoseconds: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Observation {
    pub scope: Scope,
    pub state: State,
    pub phase: Phase,
    pub accepted: Option<SnapshotIdentity>,
    pub applied: Option<SnapshotIdentity>,
    pub accepted_to_applied_ns: Option<u64>,
    pub accepted_to_presented_ns: Option<u64>,
    pub presentation: Option<crate::frames::FrameObservation>,
}

struct Participant {
    token: Token,
    scope: Scope,
    frames: Option<Handle>,
    visibility: Visibility,
    closed: bool,
    accepted: Option<Accepted>,
    applied: Option<(SnapshotIdentity, FrameStamp, Option<Timing>)>,
    baseline_request: Option<u64>,
    reported: Option<crate::frames::Snapshot>,
    reported_capability: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Capacity,
    Exhausted,
    Retired,
    WrongGeneration,
    WrongIdentity,
}

pub struct Registry {
    participants: BTreeMap<(String, u64), Participant>,
    capacity: usize,
    next: u64,
    clock: Clock,
}

impl Registry {
    pub fn new(capacity: usize, clock: Clock) -> Self {
        Self {
            participants: BTreeMap::new(),
            capacity,
            next: 1,
            clock,
        }
    }

    /// Called on native registration/window creation, not a timeout poll.
    /// Replacing a service retires its old token even when names are reused.
    pub fn register(
        &mut self,
        scope: Scope,
        frames: Option<Handle>,
        visibility: Visibility,
    ) -> Result<Token, Error> {
        let key = (scope.service.clone(), scope.surface);
        if let Some(existing) = self
            .participants
            .get(&key)
            .filter(|participant| !participant.closed && participant.scope == scope)
        {
            return Ok(existing.token);
        }
        if !self.participants.contains_key(&key) && self.participants.len() >= self.capacity {
            // Closed history is bounded by the same capacity. Never reuse a
            // minted token, so evicting its tombstone cannot revive callbacks.
            if let Some(retired) = self
                .participants
                .iter()
                .filter(|(_, p)| p.closed)
                .min_by_key(|(_, p)| p.token.0)
                .map(|(key, _)| key.clone())
            {
                self.participants.remove(&retired);
            } else {
                return Err(Error::Capacity);
            }
        }
        if frames.as_ref().is_some_and(|handle| {
            let snapshot = handle.snapshot();
            snapshot.closed
                || snapshot.live_generation != Some(scope.session_generation)
                || snapshot
                    .window
                    .is_some_and(|window| window.raw() != scope.surface)
        }) {
            return Err(Error::WrongGeneration);
        }
        let token = Token(self.next);
        self.next = self.next.checked_add(1).ok_or(Error::Exhausted)?;
        let baseline_request = frames.as_ref().and_then(|handle| {
            handle
                .snapshot()
                .last_presented
                .and_then(|frame| frame.request_id)
        });
        self.participants.insert(
            key,
            Participant {
                token,
                scope,
                frames,
                visibility,
                closed: false,
                accepted: None,
                applied: None,
                baseline_request,
                reported: None,
                reported_capability: false,
            },
        );
        Ok(token)
    }

    /// A native owner may ingest copied receipts only after authenticating the
    /// registered source and its independent real surface incarnation.
    pub fn register_reported(
        &mut self,
        scope: Scope,
        visibility: Visibility,
    ) -> Result<Token, Error> {
        let token = self.register(scope, None, visibility)?;
        self.participant(token)?.reported_capability = true;
        Ok(token)
    }

    /// Installs an authenticated owner's actual frame observation for a live token.
    pub fn report_frames(
        &mut self,
        token: Token,
        snapshot: crate::frames::Snapshot,
    ) -> Result<(), Error> {
        let participant = self.participant(token)?;
        if !participant.reported_capability
            || snapshot.live_generation != Some(participant.scope.session_generation)
            || snapshot
                .window
                .is_some_and(|window| window.raw() != participant.scope.surface)
        {
            return Err(Error::WrongGeneration);
        }
        if participant
            .reported
            .as_ref()
            .is_some_and(|previous| snapshot.lifecycle_revision < previous.lifecycle_revision)
        {
            return Err(Error::WrongGeneration);
        }
        participant.reported = Some(snapshot);
        Ok(())
    }

    fn participant(&mut self, token: Token) -> Result<&mut Participant, Error> {
        let participant = self
            .participants
            .values_mut()
            .find(|participant| participant.token == token && !participant.closed)
            .ok_or(Error::Retired)?;
        if participant.frames.as_ref().is_some_and(|handle| {
            let snapshot = handle.snapshot();
            snapshot.closed
                || snapshot.live_generation != Some(participant.scope.session_generation)
        }) {
            return Err(Error::WrongGeneration);
        }
        Ok(participant)
    }

    pub fn retire(&mut self, token: Token) -> Result<(), Error> {
        self.participants
            .values_mut()
            .find(|participant| participant.token == token && !participant.closed)
            .ok_or(Error::Retired)?
            .closed = true;
        Ok(())
    }

    pub fn visibility(&mut self, token: Token, visibility: Visibility) -> Result<(), Error> {
        let participant = self.participant(token)?;
        if participant.visibility != Visibility::Visible && visibility == Visibility::Visible {
            participant.baseline_request = participant
                .frames
                .as_ref()
                .map(Handle::snapshot)
                .or_else(|| participant.reported.clone())
                .and_then(|snapshot| snapshot.last_presented.and_then(|frame| frame.request_id));
        }
        participant.visibility = visibility;
        Ok(())
    }

    /// Called with the existing authority's accepted receipt. A new authority
    /// identity never inherits the previous revision's application timing.
    pub fn accepted(&mut self, token: Token, accepted: Accepted) -> Result<(), Error> {
        let participant = self.participant(token)?;
        if participant
            .accepted
            .as_ref()
            .is_none_or(|previous| previous.identity != accepted.identity)
        {
            participant.applied = None;
            participant.baseline_request = participant
                .frames
                .as_ref()
                .map(Handle::snapshot)
                .or_else(|| participant.reported.clone())
                .and_then(|snapshot| snapshot.last_presented.and_then(|frame| frame.request_id));
            participant.accepted = Some(accepted);
        }
        Ok(())
    }

    /// Called only after the existing native settings host acknowledges its
    /// atomic activation. The stamp is captured from that exact installed view.
    pub fn applied(
        &mut self,
        token: Token,
        identity: SnapshotIdentity,
        stamp: FrameStamp,
        timing: Option<Timing>,
    ) -> Result<(), Error> {
        let participant = self.participant(token)?;
        if participant
            .accepted
            .as_ref()
            .is_none_or(|accepted| accepted.identity != identity)
        {
            return Err(Error::WrongIdentity);
        }
        // Repeated reads cannot move the original activation time forward.
        if participant.applied.is_none() {
            participant.applied = Some((identity, stamp, timing));
        } else if participant
            .applied
            .as_ref()
            .is_some_and(|(_, installed, _)| *installed != stamp)
        {
            return Err(Error::WrongIdentity);
        }
        Ok(())
    }

    /// Local model/layout changes bind later natural frames while retaining
    /// the original authority activation time. They cannot relabel epochs.
    pub fn view_stamp(&mut self, token: Token, stamp: FrameStamp) -> Result<(), Error> {
        let participant = self.participant(token)?;
        let (_, installed, _) = participant.applied.as_mut().ok_or(Error::WrongIdentity)?;
        if installed.activation_epoch != stamp.activation_epoch
            || installed.local_revision > stamp.local_revision
        {
            return Err(Error::WrongIdentity);
        }
        *installed = stamp;
        Ok(())
    }

    /// A read-only reduction. Call from native events or once at a bounded
    /// deadline; `deadline_elapsed` never invents a presentation receipt.
    pub fn observe(&self, token: Token, deadline_elapsed: bool) -> Result<Observation, Error> {
        let participant = self
            .participants
            .values()
            .find(|participant| participant.token == token)
            .ok_or(Error::Retired)?;
        let accepted = participant.accepted.as_ref();
        let applied = participant.applied.as_ref();
        let snapshot = participant
            .frames
            .as_ref()
            .map(Handle::snapshot)
            .or_else(|| participant.reported.clone());
        let current = snapshot.as_ref().is_none_or(|snapshot| {
            !snapshot.closed
                && snapshot.live_generation == Some(participant.scope.session_generation)
                && snapshot
                    .window
                    .is_none_or(|window| window.raw() == participant.scope.surface)
        });
        let presentation = if current {
            snapshot
                .as_ref()
                .filter(|snapshot| {
                    snapshot.last_presented_revision == Some(snapshot.lifecycle_revision)
                })
                .and_then(|snapshot| snapshot.last_presented)
                .filter(|frame| {
                    frame.window.raw() == participant.scope.surface
                        && frame.request_id.is_some_and(|request| {
                            participant
                                .baseline_request
                                .is_none_or(|baseline| request > baseline)
                        })
                        && applied.is_some_and(|(_, stamp, _)| *stamp == frame.stamp)
                        && matches!(frame.outcome, FrameOutcome::Presented { .. })
                })
        } else {
            None
        };
        let eligible = participant.visibility == Visibility::Visible;
        let state =
            if participant.closed || snapshot.as_ref().is_some_and(|snapshot| snapshot.closed) {
                State::Closed
            } else if !current {
                State::Applying
            } else {
                match participant.visibility {
                    Visibility::Hidden => State::Hidden,
                    Visibility::Minimised => State::Minimised,
                    Visibility::InactiveSession => State::InactiveSession,
                    Visibility::Visible => {
                        if applied.is_none() {
                            if deadline_elapsed {
                                State::Nonresponsive
                            } else {
                                State::Applying
                            }
                        } else if participant.frames.is_none() && !participant.reported_capability {
                            State::Unsupported
                        } else if presentation.is_some() {
                            State::Presented
                        } else if deadline_elapsed {
                            State::Nonresponsive
                        } else {
                            State::AwaitingPresentation
                        }
                    }
                }
            };
        let accepted_to_applied_ns =
            accepted
                .zip(applied)
                .and_then(|(accepted, (_, _, applied))| {
                    let accepted = accepted.timing.as_ref()?;
                    let applied = applied.as_ref()?;
                    (accepted.clock == applied.clock && accepted.clock == self.clock)
                        .then(|| applied.nanoseconds.checked_sub(accepted.nanoseconds))
                        .flatten()
                });
        let accepted_to_presented_ns = if eligible {
            accepted.zip(presentation).and_then(|(accepted, frame)| {
                let accepted = accepted.timing.as_ref()?;
                match frame.outcome {
                    FrameOutcome::Presented {
                        clock_id: Some(clock_id),
                        seconds,
                        nanoseconds,
                        ..
                    } if clock_id == accepted.clock.clock_id && accepted.clock == self.clock => {
                        seconds
                            .checked_mul(1_000_000_000)?
                            .checked_add(u64::from(nanoseconds))?
                            .checked_sub(accepted.nanoseconds)
                    }
                    _ => None,
                }
            })
        } else {
            None
        };
        let phase = if eligible && presentation.is_some() && current && !participant.closed {
            Phase::Presented
        } else if applied.is_some() {
            Phase::Applied
        } else if accepted.is_some() {
            Phase::Accepted
        } else {
            Phase::Pending
        };
        Ok(Observation {
            scope: participant.scope.clone(),
            state,
            phase,
            accepted: accepted.map(|accepted| accepted.identity.clone()),
            applied: applied.map(|(identity, _, _)| identity.clone()),
            accepted_to_applied_ns,
            accepted_to_presented_ns,
            presentation: if eligible && !participant.closed {
                presentation
            } else {
                None
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iced::window::Id;
    use settings::Revision;

    #[test]
    fn churn_surface_reuse_and_untimed_replay_preserve_identity_fences() {
        let mut registry = Registry::new(1, clock());
        let window = Id::unique();
        let old = registry
            .register(scope(window, 1), None, Visibility::Visible)
            .unwrap();
        registry
            .accepted(
                old,
                Accepted {
                    identity: identity(1),
                    timing: None,
                },
            )
            .unwrap();
        registry.applied(old, identity(1), stamp(1), None).unwrap();
        registry.accepted(old, accepted(1)).unwrap();
        let current = registry.observe(old, false).unwrap();
        assert_eq!(current.phase, Phase::Applied);
        assert!(current.accepted_to_applied_ns.is_none());
        let mut replacement = scope(window, 1);
        replacement.surface_incarnation += 1;
        let new = registry
            .register(replacement, None, Visibility::Visible)
            .unwrap();
        assert_ne!(old, new);
        assert_eq!(registry.retire(old), Err(Error::Retired));
        registry.retire(new).unwrap();
        for _ in 0..32 {
            let token = registry
                .register(scope(Id::unique(), 1), None, Visibility::Visible)
                .unwrap();
            assert_eq!(registry.retire(new), Err(Error::Retired));
            registry.retire(token).unwrap();
        }
    }

    #[test]
    fn copied_receipts_cannot_relabel_old_generation_and_hidden_receipts_stay_pending() {
        let handle = Handle::new();
        let window = Id::unique();
        handle.set_live_generation(Some(1));
        let retained = handle.binding(stamp(1));
        let old = retained.captured();
        old.observe(
            window,
            Some(1),
            FrameOutcome::Presented {
                seconds: 0,
                nanoseconds: 300,
                refresh_ns: 0,
                output_sequence: 1,
                flags: 0,
                clock_id: Some(1),
            },
        );
        handle.set_live_generation(Some(2));
        let mut registry = Registry::new(8, clock());
        let token = registry
            .register_reported(scope(window, 2), Visibility::Visible)
            .unwrap();
        registry.accepted(token, accepted(1)).unwrap();
        registry
            .applied(token, identity(1), stamp(1), None)
            .unwrap();
        registry.report_frames(token, handle.snapshot()).unwrap();
        assert_eq!(
            registry.observe(token, false).unwrap().state,
            State::AwaitingPresentation
        );
        old.observe(window, Some(2), FrameOutcome::Discarded);
        registry.report_frames(token, handle.snapshot()).unwrap();
        assert!(
            registry
                .observe(token, false)
                .unwrap()
                .presentation
                .is_none()
        );
        retained.captured().observe(
            window,
            Some(3),
            FrameOutcome::Presented {
                seconds: 0,
                nanoseconds: 400,
                refresh_ns: 0,
                output_sequence: 3,
                flags: 0,
                clock_id: Some(1),
            },
        );
        registry.report_frames(token, handle.snapshot()).unwrap();
        assert_eq!(
            registry.observe(token, false).unwrap().state,
            State::Presented
        );
        registry.visibility(token, Visibility::Hidden).unwrap();
        let hidden = registry.observe(token, true).unwrap();
        assert_eq!(hidden.state, State::Hidden);
        assert!(hidden.presentation.is_none());
        registry.visibility(token, Visibility::Visible).unwrap();
        assert_eq!(
            registry.observe(token, false).unwrap().state,
            State::AwaitingPresentation
        );
        retained.captured().observe(
            window,
            Some(4),
            FrameOutcome::Presented {
                seconds: 0,
                nanoseconds: 500,
                refresh_ns: 0,
                output_sequence: 4,
                flags: 0,
                clock_id: Some(1),
            },
        );
        registry.report_frames(token, handle.snapshot()).unwrap();
        assert_eq!(
            registry.observe(token, false).unwrap().state,
            State::Presented
        );
    }

    fn clock() -> Clock {
        Clock {
            host_id: "owned-host".into(),
            boot_id: "owned-boot".into(),
            clock_id: 1,
        }
    }
    fn identity(revision: u64) -> SnapshotIdentity {
        SnapshotIdentity {
            incarnation: "authority".into(),
            revision: Revision(revision),
            design_revision: Revision(revision),
            source_digest: "source".into(),
        }
    }
    fn accepted(revision: u64) -> Accepted {
        Accepted {
            identity: identity(revision),
            timing: Some(Timing {
                clock: clock(),
                nanoseconds: 100,
            }),
        }
    }
    fn stamp(epoch: u64) -> FrameStamp {
        FrameStamp {
            activation_epoch: epoch,
            local_revision: 0,
        }
    }
    fn scope(window: Id, generation: u64) -> Scope {
        Scope {
            service: "app".into(),
            process_instance: 7,
            session_generation: generation,
            surface: window.raw(),
            surface_incarnation: window.raw(),
        }
    }
    fn present(handle: &Handle, window: Id, epoch: u64, request: u64, clock_id: Option<u32>) {
        handle.binding(stamp(epoch)).observe(
            window,
            Some(request),
            FrameOutcome::Presented {
                clock_id,
                seconds: 0,
                nanoseconds: 300,
                refresh_ns: 16,
                output_sequence: request,
                flags: 0,
            },
        );
    }

    #[test]
    fn real_frame_history_cannot_turn_hidden_minimised_or_inactive_into_presented() {
        let handle = Handle::new();
        let window = Id::unique();
        handle.set_live_generation(Some(1));
        let mut registry = Registry::new(2, clock());
        let token = registry
            .register(scope(window, 1), Some(handle.clone()), Visibility::Visible)
            .unwrap();
        registry.accepted(token, accepted(1)).unwrap();
        registry
            .applied(
                token,
                identity(1),
                stamp(1),
                Some(Timing {
                    clock: clock(),
                    nanoseconds: 200,
                }),
            )
            .unwrap();
        present(&handle, window, 1, 1, Some(1));
        let visible = registry.observe(token, false).unwrap();
        assert_eq!(visible.phase, Phase::Presented);
        assert_eq!(visible.accepted_to_applied_ns, Some(100));
        assert_eq!(visible.accepted_to_presented_ns, Some(200));
        for (visibility, state) in [
            (Visibility::Hidden, State::Hidden),
            (Visibility::Minimised, State::Minimised),
            (Visibility::InactiveSession, State::InactiveSession),
        ] {
            registry.visibility(token, visibility).unwrap();
            let hidden = registry.observe(token, true).unwrap();
            assert_eq!(hidden.state, state);
            assert_eq!(hidden.phase, Phase::Applied);
            assert!(hidden.presentation.is_none());
            assert!(hidden.accepted_to_presented_ns.is_none());
        }
        registry.visibility(token, Visibility::Visible).unwrap();
        assert_eq!(
            registry.observe(token, false).unwrap().phase,
            Phase::Applied,
            "return requires a new natural frame"
        );
        present(&handle, window, 1, 2, Some(1));
        assert_eq!(
            registry.observe(token, false).unwrap().phase,
            Phase::Presented
        );
    }

    #[test]
    fn native_connection_loss_and_replacement_fence_old_tokens_and_receipts() {
        let handle = Handle::new();
        let window = Id::unique();
        handle.set_live_generation(Some(1));
        let mut registry = Registry::new(1, clock());
        let old = registry
            .register(scope(window, 1), Some(handle.clone()), Visibility::Visible)
            .unwrap();
        registry.accepted(old, accepted(1)).unwrap();
        registry
            .applied(
                old,
                identity(1),
                stamp(1),
                Some(Timing {
                    clock: clock(),
                    nanoseconds: 200,
                }),
            )
            .unwrap();
        present(&handle, window, 1, 1, Some(1));
        handle.set_live_generation(None);
        assert_eq!(
            registry.applied(
                old,
                identity(1),
                stamp(1),
                Some(Timing {
                    clock: clock(),
                    nanoseconds: 200
                })
            ),
            Err(Error::WrongGeneration)
        );
        assert_ne!(
            registry.observe(old, false).unwrap().phase,
            Phase::Presented
        );
        registry.retire(old).unwrap();
        assert_eq!(registry.observe(old, false).unwrap().state, State::Closed);
        handle.set_live_generation(Some(2));
        let new = registry
            .register(scope(window, 2), Some(handle.clone()), Visibility::Visible)
            .unwrap();
        assert_ne!(old, new);
        assert_eq!(registry.accepted(old, accepted(2)), Err(Error::Retired));
        registry.accepted(new, accepted(2)).unwrap();
        registry
            .applied(
                new,
                identity(2),
                stamp(2),
                Some(Timing {
                    clock: clock(),
                    nanoseconds: 200,
                }),
            )
            .unwrap();
        present(&handle, window, 1, 2, Some(1));
        assert_eq!(
            registry.observe(new, true).unwrap().state,
            State::Nonresponsive
        );
        present(&handle, window, 2, 3, Some(1));
        assert_eq!(
            registry.observe(new, false).unwrap().state,
            State::Presented
        );
    }

    #[test]
    fn authority_supersession_and_foreign_clocks_never_create_latency_samples() {
        let handle = Handle::new();
        let window = Id::unique();
        handle.set_live_generation(Some(1));
        let mut registry = Registry::new(1, clock());
        let token = registry
            .register(scope(window, 1), Some(handle.clone()), Visibility::Visible)
            .unwrap();
        registry.accepted(token, accepted(2)).unwrap();
        registry.accepted(token, accepted(3)).unwrap();
        assert_eq!(
            registry.applied(
                token,
                identity(2),
                stamp(2),
                Some(Timing {
                    clock: clock(),
                    nanoseconds: 200
                })
            ),
            Err(Error::WrongIdentity)
        );
        let foreign = Clock {
            host_id: "foreign-host".into(),
            boot_id: "other-boot".into(),
            clock_id: 1,
        };
        registry
            .applied(
                token,
                identity(3),
                stamp(3),
                Some(Timing {
                    clock: foreign,
                    nanoseconds: 200,
                }),
            )
            .unwrap();
        present(&handle, window, 3, 1, None);
        let observation = registry.observe(token, false).unwrap();
        assert_eq!(observation.phase, Phase::Presented);
        assert!(observation.accepted_to_applied_ns.is_none());
        assert!(observation.accepted_to_presented_ns.is_none());
    }

    #[test]
    fn applied_ack_without_native_capability_stays_applied_and_bounded() {
        let mut registry = Registry::new(1, clock());
        let window = Id::unique();
        let token = registry
            .register(scope(window, 1), None, Visibility::Visible)
            .unwrap();
        registry.accepted(token, accepted(1)).unwrap();
        registry
            .applied(
                token,
                identity(1),
                stamp(1),
                Some(Timing {
                    clock: clock(),
                    nanoseconds: 200,
                }),
            )
            .unwrap();
        let observation = registry.observe(token, false).unwrap();
        assert_eq!(observation.state, State::Unsupported);
        assert_eq!(observation.phase, Phase::Applied);
        assert!(observation.presentation.is_none());
        assert_eq!(
            registry.register(scope(Id::unique(), 1), None, Visibility::Visible),
            Err(Error::Capacity)
        );
    }
}

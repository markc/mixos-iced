// SPDX-License-Identifier: MIT OR Apache-2.0
//! Reduction on the existing scene-host event pass. No watcher or authority.
use crate::port::PresentationNotice;
use application::{
    frames,
    participants::{self, Accepted, Clock, Phase, Scope, State, Timing, Token, Visibility},
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) struct Window {
    pub id: u64,
    pub incarnation: u64,
    pub pid: u64,
    pub visibility: Visibility,
}

struct Owner {
    registration: String,
    scope: Scope,
    token: Token,
    value: Value,
    window: Option<(u64, u64)>,
    frame_window: Option<u64>,
    frame_owner: Option<u64>,
    operation: Option<settings::clock::Commit>,
}

struct Association {
    registration: String,
    window: (u64, u64),
    retired: bool,
    last_request: Option<u64>,
}

struct Target {
    window: Option<(u64, u64)>,
    visibility: Visibility,
}

pub(crate) struct Participants {
    registry: participants::Registry,
    clock: Option<Clock>,
    registrations: BTreeMap<String, String>,
    owners: BTreeMap<String, Owner>,
    latest: BTreeMap<String, PresentationNotice>,
    associations: BTreeMap<(String, u64), Association>,
    last: Value,
}

impl Default for Participants {
    fn default() -> Self {
        let clock = settings::clock::now().map(|stamp| Clock {
            host_id: "local-native-broker".into(),
            boot_id: stamp.boot_id,
            clock_id: stamp.clock_id,
        });
        let registry = participants::Registry::new(
            128,
            clock.clone().unwrap_or(Clock {
                host_id: "unavailable".into(),
                boot_id: "unavailable".into(),
                clock_id: 0,
            }),
        );
        Self {
            registry,
            clock,
            registrations: BTreeMap::new(),
            owners: BTreeMap::new(),
            latest: BTreeMap::new(),
            associations: BTreeMap::new(),
            last: Value::Null,
        }
    }
}

impl Participants {
    pub fn registrations(&mut self, registrations: BTreeMap<String, String>) {
        self.associations.retain(|(service, _), association| {
            registrations.get(service) == Some(&association.registration)
        });
        for (key, owner) in &self.owners {
            if registrations.get(&owner.scope.service) != Some(&owner.registration) {
                let _ = self.registry.retire(owner.token);
                self.latest.remove(key);
            }
        }
        self.registrations = registrations;
    }
    pub fn notice(&mut self, notice: PresentationNotice) {
        if self.registrations.get(&notice.service) != Some(&notice.registration) {
            return;
        }
        if self.latest.len() >= 128 && !self.latest.contains_key(&notice.service) {
            return;
        }
        if self.latest.get(&notice.service).is_none_or(|old| {
            old.registration != notice.registration || old.sequence < notice.sequence
        }) {
            self.latest.insert(notice.service.clone(), notice);
        }
    }
    fn timing(&self, value: &Value) -> Option<Timing> {
        let stamp: settings::clock::Stamp = serde_json::from_value(value.clone()).ok()?;
        let clock = self.clock.as_ref()?;
        if stamp.boot_id != clock.boot_id || stamp.clock_id != clock.clock_id {
            return None;
        }
        Some(Timing {
            clock: clock.clone(),
            nanoseconds: stamp.nanoseconds,
        })
    }
    fn install(
        &mut self,
        key: String,
        service: String,
        registration: String,
        value: Value,
        snapshot: frames::Snapshot,
        target: Target,
    ) {
        let Target { window, visibility } = target;
        let Some(pid) = value["pid"].as_u64() else {
            return;
        };
        let Some(generation) = value["connection_generation"].as_u64() else {
            return;
        };
        let frame_window = snapshot
            .window
            .map(|window| window.raw())
            .or_else(|| value["frame_owner_window"].as_u64());
        let Some(surface) = frame_window.or_else(|| window.map(|(id, _)| id)) else {
            return;
        };
        if snapshot.live_generation != Some(generation) {
            return;
        }
        let scope = Scope {
            service: service.clone(),
            process_instance: pid,
            session_generation: generation,
            surface,
            surface_incarnation: window.map_or(surface, |(_, incarnation)| incarnation),
        };
        let replacing = self.owners.get(&key).is_none_or(|owner| {
            owner.registration != registration
                || owner.scope != scope
                || owner.window != window
                || owner.frame_owner != snapshot.owner
        });
        if !self.owners.contains_key(&key) && self.owners.len() >= 128 {
            let retired = self.owners.iter().find_map(|(key, owner)| {
                self.registry
                    .observe(owner.token, false)
                    .ok()
                    .filter(|observed| observed.state == State::Closed)
                    .map(|_| key.clone())
            });
            if let Some(retired) = retired {
                self.owners.remove(&retired);
            } else {
                return;
            }
        }
        if replacing && let Some(old) = self.owners.get(&key) {
            let _ = self.registry.retire(old.token);
        }
        let Ok(token) = self.registry.register_reported(scope.clone(), visibility) else {
            return;
        };
        let current = serde_json::from_value::<settings::consumer::SnapshotIdentity>(
            value["settings"]["current"].clone(),
        )
        .ok();
        let applied = serde_json::from_value::<settings::consumer::SnapshotIdentity>(
            value["settings"]["applied"].clone(),
        )
        .ok();
        let operation = serde_json::from_value::<settings::clock::Commit>(
            value["settings_observation"]["authority"].clone(),
        )
        .ok()
        .filter(|operation| Some(&operation.identity) == current.as_ref());
        if let Some(identity) = current {
            let timing = operation
                .as_ref()
                .filter(|operation| operation.changed)
                .and_then(|operation| self.timing(&json!(operation.accepted)));
            let _ = self.registry.accepted(token, Accepted { identity, timing });
        }
        if let (Some(identity), Some(epoch), Some(revision)) = (
            applied,
            value["installed_frame_stamp"]["activation_epoch"].as_u64(),
            value["installed_frame_stamp"]["local_revision"].as_u64(),
        ) {
            let point = &value["settings_observation"]["applied"];
            let exact = point["generation"].as_u64() == Some(generation)
                && serde_json::from_value::<settings::consumer::SnapshotIdentity>(
                    point["identity"].clone(),
                )
                .ok()
                .as_ref()
                    == Some(&identity);
            let timing = exact.then(|| self.timing(&point["at"])).flatten();
            let stamp = frames::FrameStamp {
                activation_epoch: epoch,
                local_revision: revision,
            };
            if self
                .registry
                .applied(token, identity, stamp, timing)
                .is_err()
            {
                let _ = self.registry.view_stamp(token, stamp);
            }
        }
        // Install acceptance before the new copied receipt so a coalesced ACK
        // plus presentation event compares against the previous receipt only.
        let _ = self.registry.visibility(token, visibility);
        let frame_owner = snapshot.owner;
        if snapshot.window.is_some() {
            let _ = self.registry.report_frames(token, snapshot);
        }
        self.owners.insert(
            key,
            Owner {
                registration,
                scope,
                token,
                value,
                window,
                frame_window,
                frame_owner,
                operation,
            },
        );
    }
    pub fn sync(
        &mut self,
        windows: &[Window],
        inactive: bool,
        local_service: Option<&str>,
        local_value: Value,
        local_frames: Vec<(String, u64, frames::Snapshot, bool)>,
        deadline_elapsed: bool,
    ) -> Value {
        let alive: BTreeSet<_> = windows
            .iter()
            .map(|window| (window.id, window.incarnation))
            .collect();
        for association in self.associations.values_mut() {
            if !alive.contains(&association.window) {
                association.retired = true;
            }
        }
        let latest: Vec<_> = self.latest.values().cloned().collect();
        for mut notice in latest {
            if Some(notice.service.as_str()) == local_service {
                continue;
            }
            if !notice.value["settings"]["context"]
                .as_str()
                .is_some_and(|context| context.starts_with("app:"))
            {
                continue;
            }
            let Some(snapshot) = frames::decode_snapshot_json(&notice.value["native_frames"])
            else {
                continue;
            };
            let pid = notice.value["pid"].as_u64();
            let matches: Vec<_> = windows
                .iter()
                .filter(|window| Some(window.pid) == pid)
                .collect();
            // Ambiguous multi-window publishers need an explicit native window
            // association; do not select an arbitrary matching PID.
            if matches.len() != 1 {
                continue;
            }
            let window = matches[0];
            let Some(frame_owner) = snapshot.owner else {
                continue;
            };
            let key = (notice.service.clone(), frame_owner);
            if !self.associations.contains_key(&key) {
                // Tombstones remain until registration retirement. Exhaustion
                // is explicit unavailability, never eviction which revives an
                // old owner callback after a same-PID window replacement.
                for ((service, _), old) in &mut self.associations {
                    if service == &notice.service {
                        old.retired = true;
                    }
                }
                if self.associations.len() >= 128 {
                    continue;
                }
                self.associations.insert(
                    key.clone(),
                    Association {
                        registration: notice.registration.clone(),
                        window: (window.id, window.incarnation),
                        retired: false,
                        last_request: None,
                    },
                );
            }
            let association = self
                .associations
                .get_mut(&key)
                .expect("admitted frame owner");
            if association.retired
                || association.registration != notice.registration
                || association.window != (window.id, window.incarnation)
            {
                continue;
            }
            if let Some(request) = snapshot.last_presented.and_then(|frame| frame.request_id) {
                association.last_request = Some(
                    association
                        .last_request
                        .map_or(request, |old| old.max(request)),
                );
            }
            let visibility = if inactive {
                Visibility::InactiveSession
            } else {
                window.visibility
            };
            notice.value["observer_provenance"] = notice.provenance;
            self.install(
                notice.service.clone(),
                notice.service,
                notice.registration,
                notice.value,
                snapshot,
                Target {
                    window: Some((window.id, window.incarnation)),
                    visibility,
                },
            );
        }
        let local_keys: BTreeSet<_> = local_frames
            .iter()
            .map(|(scene, _, _, _)| format!("{}/{scene}", local_service.unwrap_or("")))
            .collect();
        if let Some(service) = local_service
            && let Some(registration) = self.registrations.get(service).cloned()
        {
            for (scene, frame_owner, snapshot, shown) in local_frames {
                let visibility = if inactive {
                    Visibility::InactiveSession
                } else if shown {
                    Visibility::Visible
                } else {
                    Visibility::Hidden
                };
                let mut value = local_value.clone();
                value["frame_owner_window"] = json!(frame_owner);
                self.install(
                    format!("{service}/{scene}"),
                    service.into(),
                    registration.clone(),
                    value,
                    snapshot,
                    Target {
                        window: None,
                        visibility,
                    },
                );
            }
        }
        let mut rows = Vec::new();
        for (key, owner) in &self.owners {
            if owner.frame_owner.is_some_and(|frame_owner| {
                self.associations
                    .get(&(owner.scope.service.clone(), frame_owner))
                    .is_some_and(|association| association.retired)
            }) {
                let _ = self.registry.retire(owner.token);
            }
            if owner.window.is_some_and(|window| !alive.contains(&window)) {
                let _ = self.registry.retire(owner.token);
            }
            if owner.window.is_none() && !local_keys.contains(key) {
                let _ = self.registry.retire(owner.token);
            }
            let Ok(observed) = self.registry.observe(owner.token, deadline_elapsed) else {
                continue;
            };
            let current_operation = owner.operation.as_ref().filter(|operation| {
                operation.changed && observed.accepted.as_ref() == Some(&operation.identity)
            });
            let preparation_failure = serde_json::from_value::<settings::consumer::PreparationFailure>(
                owner.value["settings"]["preparation_failure"].clone(),
            )
            .ok()
            .filter(|failure| {
                owner.value["settings"]["confirmed"] == true
                    && failure.generation > 0
                    && owner.value["settings"]["generation"].as_u64() == Some(failure.generation)
                    && observed.accepted.as_ref() == Some(&failure.identity)
                    && current_operation.is_some()
                    && observed.state != State::Closed
            });
            let phase = match observed.phase {
                Phase::Pending => "pending",
                Phase::Accepted => "accepted",
                Phase::Applied => "applied",
                Phase::Presented => "presented",
            };
            let state = match observed.state {
                State::Applying => "applying",
                State::Applied => "applied",
                State::AwaitingPresentation => "awaiting_presentation",
                State::Presented => "presented",
                State::Hidden => "hidden",
                State::Minimised => "minimised",
                State::InactiveSession => "inactive_session",
                State::Nonresponsive => "nonresponsive",
                State::Closed => "closed",
                State::Unsupported => "unsupported",
            };
            rows.push(json!({"key":key,"service":owner.scope.service,"pid":owner.scope.process_instance,
                "registration_incarnation":owner.registration,"connection_generation":owner.scope.session_generation,
                "native_window":owner.window.map(|(id,generation)|json!({"id":id,"generation":generation})),
                "frame_window":owner.frame_window,"surface_incarnation":owner.scope.surface_incarnation,
                "frame_owner":owner.frame_owner,
                "provenance":owner.value["observer_provenance"],
                "state":state,"phase":phase,"context":owner.value["settings"]["context"],
                "current":observed.accepted,"applied":observed.applied,"resources":owner.value["settings"]["resources"],
                "operation_id":current_operation.map(|operation|&operation.operation_id),
                "preparation_failure":preparation_failure,
                "accepted_to_applied_ns":current_operation.and(observed.accepted_to_applied_ns),
                "accepted_to_presented_ns":current_operation.and(observed.accepted_to_presented_ns),
                "presentation":observed.presentation.map(frames::observation_json)}));
        }
        let next = json!({"contract":"application.participants.v1","clock":self.clock.as_ref().map(|clock|json!({"host_id":clock.host_id,"boot_id":clock.boot_id,"clock_id":clock.clock_id})),"participants":rows});
        self.last = next.clone();
        next
    }
    pub fn snapshot(&self) -> Value {
        self.last.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use application::iced::window::Id;

    fn notice(handle: &frames::Handle, sequence: u64) -> PresentationNotice {
        let identity = json!({"incarnation":"authority","revision":"1","design_revision":"1","source_digest":"source"});
        serde_json::from_value::<settings::consumer::SnapshotIdentity>(identity.clone())
            .expect("owner fixture uses the production snapshot identity wire contract");
        PresentationNotice {
            service: "term".into(),
            registration: "registered".into(),
            sequence,
            generation: 1,
            provenance: json!({"origin":"local","peer_pid":7}),
            value: json!({"pid":7,"connection_generation":1,
                "settings":{"context":"app:term","current":identity,"applied":identity},
                "settings_observation":{"authority":{"operation_id":"changed","identity":identity,
                    "changed":true,"validation_started":null,"commit_started":null,"accepted":null}},
                "installed_frame_stamp":{"activation_epoch":1,"local_revision":0},
                "native_frames":frames::snapshot_json(&handle.snapshot())}),
        }
    }
    fn window(id: u64) -> Window {
        Window {
            id,
            incarnation: id,
            pid: 7,
            visibility: Visibility::Visible,
        }
    }
    fn sync(participants: &mut Participants, windows: &[Window]) -> Value {
        participants.sync(windows, false, None, Value::Null, Vec::new(), false)
    }
    fn present(handle: &frames::Handle, window: Id, request: u64) {
        handle
            .binding(frames::FrameStamp {
                activation_epoch: 1,
                local_revision: 0,
            })
            .captured()
            .observe(
                window,
                Some(request),
                frames::FrameOutcome::Presented {
                    clock_id: None,
                    seconds: 0,
                    nanoseconds: 0,
                    refresh_ns: 0,
                    output_sequence: request,
                    flags: 0,
                },
            );
    }
    #[test]
    fn remote_and_quoin_failures_keep_settings_generation_separate_and_retire() {
        let identity = json!({"incarnation":"authority","revision":"1","design_revision":"1","source_digest":"source"});
        let failure = json!({"identity":identity,"generation":11,"fault":{"code":"resource_missing","path":"resources","message":"Missing set"}});
        let handle = frames::Handle::new();
        handle.set_live_generation(Some(1));
        let mut participants = Participants::default();
        participants.registrations(BTreeMap::from([
            ("term".into(), "registered".into()), ("shell".into(), "shell-registration".into())
        ]));
        let mut remote = notice(&handle, 1);
        remote.value["settings"]["confirmed"] = json!(true);
        remote.value["settings"]["generation"] = json!(11);
        remote.value["settings"]["applied"] = Value::Null;
        remote.value["settings"]["preparation_failure"] = failure.clone();
        let local = remote.value.clone();
        participants.notice(remote.clone());
        let local_handle = frames::Handle::new();
        local_handle.set_live_generation(Some(1));
        let rows = participants.sync(&[window(10)], false, Some("shell"), local,
            vec![("control".into(), 20, local_handle.snapshot(), true)], false);
        assert_eq!(rows["participants"].as_array().unwrap().len(), 2);
        for row in rows["participants"].as_array().unwrap() {
            assert_eq!(row["connection_generation"], 1);
            assert_eq!(row["preparation_failure"], failure);
            assert!(row["applied"].is_null());
        }
        remote.sequence = 2;
        remote.value["settings"]["preparation_failure"]["generation"] = json!(12);
        participants.notice(remote.clone());
        let stale = sync(&mut participants, &[window(10)]);
        let term = stale["participants"].as_array().unwrap().iter().find(|row| row["service"] == "term").unwrap();
        assert!(term["preparation_failure"].is_null());
        remote.sequence = 3;
        remote.value["settings"]["preparation_failure"] = failure.clone();
        remote.value["settings"]["preparation_failure"]["identity"]["revision"] = json!("2");
        participants.notice(remote.clone());
        assert!(sync(&mut participants, &[window(10)])["participants"].as_array().unwrap().iter().all(|row| row["preparation_failure"].is_null()));
        remote.sequence = 4;
        remote.value["settings"]["preparation_failure"] = failure.clone();
        remote.value["settings"]["preparation_failure"]["generation"] = json!(0);
        remote.value["settings"]["generation"] = json!(0);
        participants.notice(remote.clone());
        assert!(sync(&mut participants, &[window(10)])["participants"].as_array().unwrap().iter().all(|row| row["preparation_failure"].is_null()));
        remote.sequence = 5;
        remote.value["settings"]["preparation_failure"] = failure;
        remote.value["settings"]["generation"] = json!(11);
        remote.value["settings"]["confirmed"] = json!(false);
        participants.notice(remote);
        assert!(sync(&mut participants, &[window(10)])["participants"].as_array().unwrap().iter().all(|row| row["preparation_failure"].is_null()));
        assert!(sync(&mut participants, &[])["participants"].as_array().unwrap().iter().all(|row| row["state"] == "closed" && row["preparation_failure"].is_null()));
    }
    #[test]
    fn retired_receipt_and_late_callback_cannot_certify_same_pid_replacement() {
        let mut participants = Participants::default();
        participants.registrations(BTreeMap::from([("term".into(), "registered".into())]));
        let old = frames::Handle::new();
        old.set_live_generation(Some(1));
        let old_window = Id::unique();
        present(&old, old_window, 1);
        participants.notice(notice(&old, 1));
        assert_eq!(
            sync(&mut participants, &[window(10)])["participants"][0]["state"],
            "presented",
            "legitimate first presentation is not discarded"
        );
        assert_eq!(
            sync(&mut participants, &[])["participants"][0]["state"],
            "closed"
        );
        let retained = sync(&mut participants, &[window(11)]);
        assert_eq!(retained["participants"][0]["state"], "closed");
        assert_eq!(retained["participants"][0]["native_window"]["id"], 10);
        present(&old, old_window, 2);
        participants.notice(notice(&old, 2));
        assert_eq!(
            sync(&mut participants, &[window(11)])["participants"][0]["state"],
            "closed",
            "even a newer callback from the retired Handle cannot rebind"
        );
        assert_eq!(
            participants.associations[&("term".into(), old.snapshot().owner.unwrap())].last_request,
            Some(1),
            "retirement retains the original request baseline instead of moving it with a late callback"
        );
        let fresh = frames::Handle::new();
        fresh.set_live_generation(Some(1));
        participants.notice(notice(&fresh, 3));
        let pending = sync(&mut participants, &[window(11)]);
        assert_eq!(pending["participants"][0]["state"], "awaiting_presentation");
        assert!(pending["participants"][0]["presentation"].is_null());
        present(&fresh, Id::unique(), 1);
        participants.notice(notice(&fresh, 4));
        let presented = sync(&mut participants, &[window(11)]);
        assert_eq!(presented["participants"][0]["state"], "presented");
        assert_eq!(presented["participants"][0]["native_window"]["id"], 11);
    }
    #[test]
    fn pending_owner_is_associated_before_its_first_callback() {
        let mut participants = Participants::default();
        participants.registrations(BTreeMap::from([("term".into(), "registered".into())]));
        let handle = frames::Handle::new();
        handle.set_live_generation(Some(1));
        participants.notice(notice(&handle, 1));
        assert!(
            sync(&mut participants, &[window(10)])["participants"][0]["presentation"].is_null()
        );
        sync(&mut participants, &[]);
        present(&handle, Id::unique(), 1);
        participants.notice(notice(&handle, 2));
        assert_eq!(
            sync(&mut participants, &[window(11)])["participants"][0]["state"],
            "closed"
        );
        assert!(
            participants
                .associations
                .values()
                .all(|association| association.retired)
        );
        participants.registrations(BTreeMap::new());
        assert!(
            participants.associations.is_empty(),
            "broker retirement bounds tombstone lifetime"
        );
    }
    #[test]
    fn association_capacity_never_evicts_a_retired_owner_or_retains_false_current_proof() {
        let mut participants = Participants::default();
        participants.registrations(BTreeMap::from([("term".into(), "registered".into())]));
        let first = frames::Handle::new();
        first.set_live_generation(Some(1));
        participants.notice(notice(&first, 1));
        sync(&mut participants, &[window(10)]);
        for sequence in 2..=128 {
            let owner = frames::Handle::new();
            owner.set_live_generation(Some(1));
            participants.notice(notice(&owner, sequence));
            sync(&mut participants, &[window(10)]);
        }
        assert_eq!(participants.associations.len(), 128);
        let excess = frames::Handle::new();
        excess.set_live_generation(Some(1));
        participants.notice(notice(&excess, 129));
        assert_eq!(
            sync(&mut participants, &[window(10)])["participants"][0]["state"],
            "closed"
        );
        assert_eq!(participants.associations.len(), 128);
        present(&first, Id::unique(), 1);
        participants.notice(notice(&first, 130));
        assert_eq!(
            sync(&mut participants, &[window(11)])["participants"][0]["state"],
            "closed"
        );
    }
}

// SPDX-License-Identifier: MIT OR Apache-2.0
//! Reduction on the existing scene-host event pass. No watcher or authority.
use application::{frames, participants::{self, Accepted, Clock, Phase, Scope, State, Timing, Token, Visibility}};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use crate::port::PresentationNotice;

pub(crate) struct Window {
    pub id:u64,
    pub incarnation:u64,
    pub pid:u64,
    pub visibility:Visibility,
}

struct Owner {
    registration:String,
    scope:Scope,
    token:Token,
    value:Value,
    window:Option<(u64,u64)>,
    frame_window:Option<u64>,
    operation:Option<settings::clock::Commit>,
}

pub(crate) struct Participants {
    registry:participants::Registry,
    clock:Option<Clock>,
    registrations:BTreeMap<String,String>,
    owners:BTreeMap<String,Owner>,
    latest:BTreeMap<String,PresentationNotice>,
    last:Value,
}

impl Default for Participants {
    fn default()->Self {
        let clock = settings::clock::now().map(|stamp|Clock {host_id:"local-native-broker".into(), boot_id:stamp.boot_id, clock_id:stamp.clock_id});
        let registry = participants::Registry::new(128, clock.clone().unwrap_or(Clock {host_id:"unavailable".into(),boot_id:"unavailable".into(),clock_id:0}));
        Self {registry,clock,registrations:BTreeMap::new(),owners:BTreeMap::new(),latest:BTreeMap::new(),last:Value::Null}
    }
}

impl Participants {
    pub fn registrations(&mut self, registrations:BTreeMap<String,String>) {
        for (key, owner) in &self.owners {
            if registrations.get(&owner.scope.service) != Some(&owner.registration) {
                let _ = self.registry.retire(owner.token);
                self.latest.remove(key);
            }
        }
        self.registrations = registrations;
    }
    pub fn notice(&mut self, notice:PresentationNotice) {
        if self.registrations.get(&notice.service) != Some(&notice.registration) {return;}
        if self.latest.len() >= 128 && !self.latest.contains_key(&notice.service) {return;}
        if self.latest.get(&notice.service).is_none_or(|old| old.registration != notice.registration || old.sequence < notice.sequence) {
            self.latest.insert(notice.service.clone(), notice);
        }
    }
    fn timing(&self, value:&Value)->Option<Timing> {
        let stamp:settings::clock::Stamp = serde_json::from_value(value.clone()).ok()?;
        let clock = self.clock.as_ref()?;
        if stamp.boot_id != clock.boot_id || stamp.clock_id != clock.clock_id {return None;}
        Some(Timing {clock:clock.clone(),nanoseconds:stamp.nanoseconds})
    }
    fn install(&mut self, key:String, service:String, registration:String, value:Value, snapshot:frames::Snapshot,
        window:Option<(u64,u64)>, visibility:Visibility) {
        let Some(pid) = value["pid"].as_u64() else {return;};
        let Some(generation) = value["connection_generation"].as_u64() else {return;};
        let frame_window=snapshot.window.map(|window|window.raw()).or_else(||value["frame_owner_window"].as_u64());
        let Some(surface) = frame_window.or_else(||window.map(|(id,_)|id)) else {return;};
        if snapshot.live_generation != Some(generation) {return;}
        let scope = Scope {service:service.clone(),process_instance:pid,session_generation:generation,surface,
            surface_incarnation:window.map_or(surface,|(_,incarnation)|incarnation)};
        let replacing = self.owners.get(&key).is_none_or(|owner| owner.registration != registration || owner.scope != scope || owner.window != window);
        if !self.owners.contains_key(&key) && self.owners.len() >= 128 {
            let retired = self.owners.iter().find_map(|(key,owner)| {
                self.registry.observe(owner.token,false).ok()
                    .filter(|observed| observed.state == State::Closed).map(|_|key.clone())
            });
            if let Some(retired) = retired { self.owners.remove(&retired); }
            else { return; }
        }
        if replacing && let Some(old) = self.owners.get(&key) {let _ = self.registry.retire(old.token);}
        let Ok(token) = self.registry.register_reported(scope.clone(), visibility) else {return;};
        let current = serde_json::from_value::<settings::consumer::SnapshotIdentity>(value["settings"]["current"].clone()).ok();
        let applied = serde_json::from_value::<settings::consumer::SnapshotIdentity>(value["settings"]["applied"].clone()).ok();
        let operation = serde_json::from_value::<settings::clock::Commit>(value["settings_observation"]["authority"].clone()).ok()
            .filter(|operation| Some(&operation.identity) == current.as_ref());
        if let Some(identity) = current {
            let timing = operation.as_ref().filter(|operation|operation.changed).and_then(|operation|self.timing(&json!(operation.accepted)));
            let _ = self.registry.accepted(token, Accepted {identity, timing});
        }
        if let (Some(identity),Some(epoch),Some(revision)) = (applied, value["installed_frame_stamp"]["activation_epoch"].as_u64(), value["installed_frame_stamp"]["local_revision"].as_u64()) {
            let point = &value["settings_observation"]["applied"];
            let exact = point["generation"].as_u64() == Some(generation)
                && serde_json::from_value::<settings::consumer::SnapshotIdentity>(point["identity"].clone()).ok().as_ref() == Some(&identity);
            let timing = exact.then(||self.timing(&point["at"])).flatten();
            let stamp = frames::FrameStamp {activation_epoch:epoch,local_revision:revision};
            if self.registry.applied(token,identity,stamp,timing).is_err() {let _ = self.registry.view_stamp(token, stamp);}
        }
        // Install acceptance before the new copied receipt so a coalesced ACK
        // plus presentation event compares against the previous receipt only.
        let _ = self.registry.visibility(token,visibility);
        if snapshot.window.is_some() {let _ = self.registry.report_frames(token,snapshot);}
        self.owners.insert(key, Owner {registration,scope,token,value,window,frame_window,operation});
    }
    pub fn sync(&mut self, windows:&[Window], inactive:bool, local_service:Option<&str>, local_value:Value,
        local_frames:Vec<(String,u64,frames::Snapshot,bool)>, deadline_elapsed:bool)->Value {
        let latest:Vec<_> = self.latest.values().cloned().collect();
        for mut notice in latest {
            if Some(notice.service.as_str()) == local_service {continue;}
            if !notice.value["settings"]["context"].as_str().is_some_and(|context|context.starts_with("app:")) {continue;}
            let Some(snapshot) = frames::decode_snapshot_json(&notice.value["native_frames"]) else {continue;};
            let pid = notice.value["pid"].as_u64();
            let matches:Vec<_> = windows.iter().filter(|window|Some(window.pid)==pid).collect();
            // Ambiguous multi-window publishers need an explicit native window
            // association; do not select an arbitrary matching PID.
            if matches.len() != 1 {continue;}
            let window = matches[0];
            let visibility = if inactive {Visibility::InactiveSession} else {window.visibility};
            notice.value["observer_provenance"]=notice.provenance;
            self.install(notice.service.clone(),notice.service,notice.registration,notice.value,snapshot,
                Some((window.id,window.incarnation)),visibility);
        }
        let local_keys:BTreeSet<_> = local_frames.iter().map(|(scene,_,_,_)| {
            format!("{}/{scene}",local_service.unwrap_or(""))
        }).collect();
        if let Some(service) = local_service {
            if let Some(registration) = self.registrations.get(service).cloned() {
                for (scene, frame_owner, snapshot, shown) in local_frames {
                    let visibility = if inactive {Visibility::InactiveSession} else if shown {Visibility::Visible} else {Visibility::Hidden};
                    let mut value=local_value.clone(); value["frame_owner_window"]=json!(frame_owner);
                    self.install(format!("{service}/{scene}"), service.into(),registration.clone(),value,snapshot,None,visibility);
                }
            }
        }
        let alive:BTreeSet<_> = windows.iter().map(|window|(window.id,window.incarnation)).collect();
        let mut rows = Vec::new();
        for (key, owner) in &self.owners {
            if owner.window.is_some_and(|window|!alive.contains(&window)) {let _ = self.registry.retire(owner.token);}
            if owner.window.is_none() && !local_keys.contains(key) {let _ = self.registry.retire(owner.token);}
            let Ok(observed) = self.registry.observe(owner.token,deadline_elapsed) else {continue;};
            let current_operation = owner.operation.as_ref().filter(|operation| operation.changed && observed.accepted.as_ref() == Some(&operation.identity));
            let phase = match observed.phase {Phase::Pending=>"pending",Phase::Accepted=>"accepted",Phase::Applied=>"applied",Phase::Presented=>"presented"};
            let state = match observed.state {State::Applying=>"applying",State::Applied=>"applied",State::AwaitingPresentation=>"awaiting_presentation",State::Presented=>"presented",State::Hidden=>"hidden",State::Minimised=>"minimised",State::InactiveSession=>"inactive_session",State::Nonresponsive=>"nonresponsive",State::Closed=>"closed",State::Unsupported=>"unsupported"};
            rows.push(json!({"key":key,"service":owner.scope.service,"pid":owner.scope.process_instance,
                "registration_incarnation":owner.registration,"connection_generation":owner.scope.session_generation,
                "native_window":owner.window.map(|(id,generation)|json!({"id":id,"generation":generation})),
                "frame_window":owner.frame_window,"surface_incarnation":owner.scope.surface_incarnation,
                "provenance":owner.value["observer_provenance"],
                "state":state,"phase":phase,"context":owner.value["settings"]["context"],
                "current":observed.accepted,"applied":observed.applied,"resources":owner.value["settings"]["resources"],
                "operation_id":current_operation.map(|operation|&operation.operation_id),
                "accepted_to_applied_ns":current_operation.and(observed.accepted_to_applied_ns),
                "accepted_to_presented_ns":current_operation.and(observed.accepted_to_presented_ns),
                "presentation":observed.presentation.map(frames::observation_json)}));
        }
        let next = json!({"contract":"application.participants.v1","clock":self.clock.as_ref().map(|clock|json!({"host_id":clock.host_id,"boot_id":clock.boot_id,"clock_id":clock.clock_id})),"participants":rows});
        self.last = next.clone();
        next
    }
    pub fn snapshot(&self)->Value {self.last.clone()}
}

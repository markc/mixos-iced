// SPDX-License-Identifier: MIT OR Apache-2.0
//! Finite native requests over copied owner receipts. No periodic reads.
use application::native_actor::Accepted;
use application::native_queue::Permit;
use application::participants::OwnerFence;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    future::Future,
    time::{Duration, Instant},
};
use tokio::sync::watch;

#[derive(Clone)]
pub(crate) struct Observed {
    sequence: u64,
    at: Instant,
    value: Value,
}
#[derive(Clone, Default)]
pub(crate) struct History {
    records: VecDeque<Observed>,
    latest: Value,
    fingerprint: Value,
    sequence: u64,
}
impl History {
    pub(crate) fn observe(&mut self, value: Value, at: Instant) -> bool {
        if self.latest == value {
            return false;
        }
        let mut fingerprint = value.clone();
        if let Some(rows) = fingerprint["participants"].as_array_mut() {
            for row in rows {
                if !row["presentation"].is_null() {
                    row["presentation"] = row["presentation"]["stamp"].clone();
                }
                // Ordinary frames update the inspector's physical receipt,
                // not the first presentation of this operation and stamp.
                if let Some(row) = row.as_object_mut() {
                    row.remove("accepted_to_presented_ns");
                }
            }
        }
        if self.records.is_empty() || self.fingerprint != fingerprint {
            let Some(sequence) = self.sequence.checked_add(1) else {
                return false;
            };
            self.sequence = sequence;
            let mut retained = value.clone();
            if let (Some(rows), Some(previous)) = (
                retained["participants"].as_array_mut(),
                self.records
                    .back()
                    .and_then(|record| record.value["participants"].as_array()),
            ) {
                for row in rows {
                    if let Some(old) = previous.iter().find(|old| {
                        old["key"] == row["key"] && phase_fingerprint(old) == phase_fingerprint(row)
                    }) {
                        *row = old.clone();
                    }
                }
            }
            self.records.push_back(Observed {
                sequence,
                at,
                value: retained,
            });
            if self.records.len() > 128 {
                self.records.pop_front();
            }
            self.fingerprint = fingerprint;
        }
        self.latest = value;
        true
    }
}

fn phase_fingerprint(row: &Value) -> Value {
    let mut row = row.clone();
    if !row["presentation"].is_null() {
        row["presentation"] = row["presentation"]["stamp"].clone();
    }
    if let Some(row) = row.as_object_mut() {
        row.remove("accepted_to_presented_ns");
    }
    row
}

pub(crate) struct Spec {
    operation: String,
    services: BTreeSet<String>,
    timeout: Duration,
    presented: bool,
    closed: bool,
    preparation_failed: bool,
    keys: Option<BTreeSet<String>>,
    owners: Option<BTreeMap<String, OwnerFence>>,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct WireSpec {
    operation_id: String,
    services: Vec<String>,
    timeout_ms: u64,
    #[serde(default)]
    until: Option<String>,
    #[serde(default)]
    keys: Option<Vec<String>>,
    #[serde(default)]
    owners: Option<BTreeMap<String, OwnerFence>>,
}
pub(crate) fn parse(body: &str) -> Option<Spec> {
    if body.len() > 16 * 1024 {
        return None;
    }
    let value: WireSpec = serde_json::from_str(body).ok()?;
    let (presented, closed, preparation_failed) = match value.until.as_deref() {
        None | Some("settled") => (false, false, false),
        Some("presented") => (true, false, false),
        Some("closed") => (false, true, false),
        Some("preparation_failed") => (false, false, true),
        _ => return None,
    };
    let keys = if let Some(rows) = value.keys {
        if rows.is_empty() || rows.len() > 128 {
            return None;
        }
        let mut keys = BTreeSet::new();
        for key in rows {
            if key.is_empty() || key.len() > 512 || !keys.insert(key) {
                return None;
            }
        }
        Some(keys)
    } else {
        None
    };
    if preparation_failed {
        let keys = keys.as_ref()?;
        let owners = value.owners.as_ref()?;
        if owners.len() != keys.len()
            || owners.keys().any(|key| !keys.contains(key))
            || owners.values().any(|owner| {
                owner.pid == 0 || owner.connection_generation == 0
                    || owner.registration_incarnation.is_empty()
                    || owner.registration_incarnation.len() > 256
            })
        {
            return None;
        }
    } else if value.owners.is_some() {
        return None;
    }
    let operation = value.operation_id;
    if operation.is_empty() || operation.len() > 128 {
        return None;
    }
    let timeout = value.timeout_ms;
    if !(1..=60_000).contains(&timeout) {
        return None;
    }
    let rows = value.services;
    if rows.is_empty() || rows.len() > 128 {
        return None;
    }
    let mut services = BTreeSet::new();
    for service in rows {
        if service.is_empty() || service.len() > 256 || !services.insert(service) {
            return None;
        }
    }
    Some(Spec {
        operation,
        services,
        timeout: Duration::from_millis(timeout),
        presented,
        closed,
        preparation_failed,
        keys,
        owners: value.owners,
    })
}

fn complete(value: &Value, spec: &Spec) -> bool {
    let Some(rows) = value["participants"].as_array() else {
        return false;
    };
    if spec.keys.as_ref().is_some_and(|keys| {
        keys.iter()
            .any(|key| !rows.iter().any(|row| row["key"] == key.as_str()))
    }) {
        return false;
    }
    if spec.preparation_failed && spec.keys.as_ref().is_some_and(|keys| {
        keys.iter().any(|key| {
            let matching: Vec<_> = rows.iter().filter(|row| row["key"] == key.as_str()).collect();
            matching.len() != 1 || matching[0]["service"].as_str().is_none_or(|service| !spec.services.contains(service))
        })
    }) {
        return false;
    }
    spec.services.iter().all(|service| {
        let selected: Vec<_> = rows
            .iter()
            .filter(|row| {
                row["service"] == service.as_str()
                    && spec.keys.as_ref().is_none_or(|keys| {
                        row["key"].as_str().is_some_and(|key| keys.contains(key))
                    })
            })
            .collect();
        !selected.is_empty()
            && selected.iter().all(|row| {
                if spec.preparation_failed {
                    let Some(key) = row["key"].as_str() else { return false; };
                    let Some(expected) = spec.owners.as_ref().and_then(|owners| owners.get(key)) else { return false; };
                    let actual = serde_json::from_value::<OwnerFence>(json!({
                        "pid":row["pid"],"connection_generation":row["connection_generation"],
                        "registration_incarnation":row["registration_incarnation"],
                        "surface_incarnation":row["surface_incarnation"],
                        "native_window":row["native_window"],"frame_owner":row["frame_owner"]
                    })).ok();
                    let failure = serde_json::from_value::<settings::consumer::PreparationFailure>(row["preparation_failure"].clone()).ok();
                    let current = serde_json::from_value::<settings::consumer::SnapshotIdentity>(row["current"].clone()).ok();
                    return row["state"] != "closed"
                        && row["operation_id"] == spec.operation
                        && actual.as_ref() == Some(expected)
                        && failure.as_ref().is_some_and(|failure| failure.generation > 0 && current.as_ref() == Some(&failure.identity));
                }
                if spec.closed {
                    return row["state"] == "closed";
                }
                if spec.presented {
                    return row["operation_id"] == spec.operation && row["state"] == "presented";
                }
                if row["state"] == "closed" {
                    return true;
                }
                row["operation_id"] == spec.operation
                    && matches!(
                        row["state"].as_str(),
                        Some("presented" | "hidden" | "minimised" | "inactive_session")
                    )
            })
    })
}

fn on_time<'a>(
    history: &'a History,
    spec: &Spec,
    baseline: u64,
    deadline: Instant,
) -> Option<&'a Observed> {
    history.records.iter().find(|observed| {
        observed.sequence >= baseline && observed.at <= deadline && complete(&observed.value, spec)
    })
}

pub(crate) fn run(
    request: Accepted,
    spec: Spec,
    mut receipts: watch::Receiver<History>,
) -> (
    Permit,
    impl Future<Output = Result<(), String>> + Send + 'static,
) {
    let baseline = receipts
        .borrow()
        .records
        .iter()
        .rev()
        .find(|record| record.at <= request.admitted_at())
        .map_or(0, |record| record.sequence);
    request.into_task(move |client,command,admitted| async move {
    let deadline=tokio::time::Instant::from_std(admitted+spec.timeout);
    let mut lifecycle=client.subscribe_state();
    if *lifecycle.borrow_and_update()!=bus::ConnState::Connected || client.connection_generation()!=command.generation {
        return Err("participant request generation retired".into());
    }
    let mut history=receipts.borrow_and_update().clone();
    let mut value;
    let elapsed=loop {
        if let Some(observed)=on_time(&history,&spec,baseline,deadline.into_std()) {
            value=observed.value.clone();break false;
        }
        if history.records.front().is_some_and(|record|record.sequence>baseline.saturating_add(1)) {
            return tokio::time::timeout(Duration::from_secs(1),client.respond_parts(command.generation,&command.from,&command.command,command.id.as_deref(),12,
                r#"{"error_code":"PARTICIPANT_HISTORY_GAP","message":"original phase unavailable"}"#))
                .await.map_err(|_|"participant gap reply timed out".to_owned())?.map_err(|error|error.to_string());
        }
        if tokio::time::Instant::now()>=deadline {
            value=history.records.iter().rev().find(|record|record.at<=deadline.into_std())
                .map_or(Value::Null,|record|record.value.clone());break true;
        }
        tokio::select! {
            biased;
            _=tokio::time::sleep_until(deadline)=>{
                history=receipts.borrow_and_update().clone();
            },
            changed=receipts.changed()=>{
                if changed.is_err() {return Err("participant owner retired".into());}
                history=receipts.borrow_and_update().clone();
            }
            changed=lifecycle.changed()=>{
                if changed.is_err() || *lifecycle.borrow_and_update()!=bus::ConnState::Connected
                    || client.connection_generation()!=command.generation {return Err("participant request generation retired".into());}
            }
        }
    };
    // A deadline receipt copies the actual installed/presented phase. It only
    // classifies eligible incomplete live targets; hidden buffers stay hidden.
    let target_known=value["participants"].as_array().is_some_and(|rows|rows.iter().any(|row|row["operation_id"]==spec.operation));
    if elapsed && let Some(rows)=value["participants"].as_array_mut() {
        for row in rows {
            let selected=row["service"].as_str().is_some_and(|service|spec.services.contains(service))
                && spec.keys.as_ref().is_none_or(|keys|row["key"].as_str().is_some_and(|key|keys.contains(key)));
            if selected && target_known && (matches!(row["state"].as_str(),Some("applying"|"applied"|"awaiting_presentation"))
                || (row["state"]=="presented" && row["operation_id"]!=spec.operation)) {
                row["state"]=json!("nonresponsive");
                row["deadline_target_operation"]=json!(spec.operation);
            }
        }
    }
    let body=json!({"contract":"application.participants.wait.v1","operation_id":spec.operation,
        "services":spec.services,"keys":spec.keys,"until":if spec.preparation_failed {"preparation_failed"} else if spec.presented {"presented"} else if spec.closed {"closed"} else {"settled"},"deadline_elapsed":elapsed,"observations":value}).to_string();
    tokio::time::timeout(Duration::from_secs(1),client.respond_parts(command.generation,&command.from,&command.command,command.id.as_deref(),0,&body))
        .await.map_err(|_|"participant reply timed out".to_owned())?
        .map_err(|error|error.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preparation_failure_is_operation_and_owner_fenced_and_allows_cold_owners() {
        let identity = json!({"incarnation":"authority","revision":"2","design_revision":"1","source_digest":"digest"});
        let fence = json!({"pid":42,"connection_generation":7,"registration_incarnation":"native-registration","surface_incarnation":3,"native_window":null,"frame_owner":null});
        let body = json!({"operation_id":"failed-op","services":["shell"],"keys":["shell/control"],"owners":{"shell/control":fence},"until":"preparation_failed","timeout_ms":100});
        let spec = parse(&body.to_string()).unwrap();
        let mut row = fence.clone();
        row["key"] = json!("shell/control");
        row["service"] = json!("shell");
        row["operation_id"] = json!("failed-op");
        row["state"] = json!("applying");
        row["current"] = identity.clone();
        row["applied"] = Value::Null;
        row["preparation_failure"] = json!({"identity":identity,"generation":11,"fault":{"code":"resource_failed","path":"resources","message":"Missing resource"}});
        let receipt = |row: &Value| json!({"participants":[row]});
        assert!(complete(&receipt(&row), &spec));
        for (field, replacement) in [
            ("operation_id", json!("wrong-operation")),
            ("state", json!("closed")),
            ("pid", json!(43)),
            ("connection_generation", json!(8)),
            ("registration_incarnation", json!("replacement")),
            ("surface_incarnation", json!(4)),
            ("frame_owner", json!(99)),
            ("native_window", json!({"id":1,"generation":1})),
            ("preparation_failure", Value::Null),
        ] {
            let mut wrong = row.clone();
            wrong[field] = replacement;
            assert!(!complete(&receipt(&wrong), &spec), "{field}");
        }
        let mut stale = row.clone();
        stale["preparation_failure"]["identity"]["revision"] = json!("1");
        assert!(!complete(&receipt(&stale), &spec));
        stale = row.clone();
        stale["preparation_failure"]["generation"] = json!(0);
        assert!(!complete(&receipt(&stale), &spec));
        let mut cold_body = body.clone();
        cold_body["owners"]["shell/control"]["surface_incarnation"] = json!(0);
        let cold_spec = parse(&cold_body.to_string()).unwrap();
        let mut cold_row = row.clone();
        cold_row["surface_incarnation"] = json!(0);
        assert!(complete(&receipt(&cold_row), &cold_spec));
        let mut outside = body.clone();
        outside["keys"] = json!(["shell/control", "term/main"]);
        outside["owners"]["term/main"] = fence.clone();
        let outside_spec = parse(&outside.to_string()).unwrap();
        let mut other = row.clone();
        other["key"] = json!("term/main");
        other["service"] = json!("term");
        assert!(!complete(&json!({"participants":[row.clone(),other]}), &outside_spec));
        let mut missing = body.clone();
        missing.as_object_mut().unwrap().remove("owners");
        assert!(parse(&missing.to_string()).is_none());
        missing = body.clone();
        missing["owners"] = json!({});
        assert!(parse(&missing.to_string()).is_none());
        missing = body.clone();
        missing["until"] = json!("settled");
        assert!(parse(&missing.to_string()).is_none());
        let mut history = History::default();
        let mut before = row.clone();
        before["preparation_failure"] = Value::Null;
        let at = Instant::now();
        history.observe(receipt(&before), at);
        history.observe(receipt(&row), at + Duration::from_millis(1));
        assert_eq!(history.records.len(), 2);
        assert!(on_time(&history, &spec, 1, at).is_none());
        assert!(on_time(&history, &spec, 1, at + Duration::from_millis(2)).is_some());
    }
    #[test]
    fn ordinary_frames_preserve_first_phase_receipt_and_latency() {
        let first_at = Instant::now();
        let mut history = History::default();
        let receipt = |frame: u64| {
            json!({"participants":[{
                "key":"term/main", "service":"term", "incarnation":1,
                "state":"presented", "operation_id":"op",
                "presentation":{"stamp":"stamp", "request_id":frame},
                "accepted_to_presented_ns":frame * 100
            }]})
        };
        for frame in 1..=256 {
            assert!(history.observe(receipt(frame), first_at + Duration::from_millis(frame)));
        }
        assert_eq!(history.records.len(), 1);
        assert_eq!(history.records[0].at, first_at + Duration::from_millis(1));
        assert_eq!(history.records[0].value, receipt(1));
        assert_eq!(history.latest, receipt(256));
        let mut with_sibling = receipt(257);
        with_sibling["participants"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "key":"shell/control", "service":"shell", "state":"applying"
            }));
        history.observe(with_sibling.clone(), first_at + Duration::from_millis(257));
        with_sibling["participants"][1]["state"] = json!("presented");
        with_sibling["participants"][0] = receipt(258)["participants"][0].clone();
        history.observe(with_sibling, first_at + Duration::from_millis(258));
        assert_eq!(
            history.records.back().unwrap().value["participants"][0],
            receipt(1)["participants"][0]
        );
        let spec = parse(
            r#"{"operation_id":"op","services":["term"],"until":"presented","timeout_ms":10}"#,
        )
        .unwrap();
        let settled = on_time(&history, &spec, 1, first_at + Duration::from_millis(10)).unwrap();
        assert_eq!(
            settled.value["participants"][0]["accepted_to_presented_ns"],
            100
        );
        let mut changed = receipt(257);
        changed["participants"][0]["state"] = json!("hidden");
        history.observe(changed, first_at + Duration::from_millis(257));
        let mut changed = receipt(258);
        changed["participants"][0]["presentation"]["stamp"] = json!("new-stamp");
        history.observe(changed, first_at + Duration::from_millis(258));
        let mut changed = receipt(259);
        changed["participants"][0]["incarnation"] = json!(2);
        history.observe(changed, first_at + Duration::from_millis(259));
        assert_eq!(history.records.len(), 6);
    }
    #[test]
    fn bounded_wait_refuses_ambiguous_or_unbounded_requests() {
        assert!(
            parse(r#"{"operation_id":"op","services":["term","shell"],"timeout_ms":1000}"#)
                .is_some()
        );
        for body in [
            r#"{"operation_id":"op","services":["term","term"],"timeout_ms":1000}"#,
            r#"{"operation_id":"op","operation_id":"other","services":["term"],"timeout_ms":1000}"#,
            r#"{"operation_id":"op","services":["term"],"timeout_ms":60001}"#,
            r#"{"operation_id":"op","services":[],"timeout_ms":1000}"#,
            r#"{"operation_id":"op","services":["term"],"timeout_ms":1000,"deadline_elapsed":true}"#,
        ] {
            assert!(parse(body).is_none());
        }
    }
    #[test]
    fn hidden_phase_is_terminal_only_for_the_actual_changed_operation() {
        let spec =
            parse(r#"{"operation_id":"new","services":["term"],"timeout_ms":1000}"#).unwrap();
        assert!(!complete(
            &json!({"participants":[{"service":"term","state":"hidden","operation_id":"old"}]}),
            &spec
        ));
        assert!(complete(
            &json!({"participants":[{"service":"term","state":"hidden","operation_id":"new","phase":"applied"}]}),
            &spec
        ));
        assert!(!complete(
            &json!({"participants":[{"service":"term","state":"awaiting_presentation","operation_id":"new"}]}),
            &spec
        ));
        assert!(!complete(&json!({"participants":[]}), &spec));
        let strict=parse(r#"{"operation_id":"new","services":["shell"],"keys":["shell/control"],"until":"presented","timeout_ms":1000}"#).unwrap();
        assert!(!complete(
            &json!({"participants":[{"key":"shell/control","service":"shell","state":"hidden","operation_id":"new"}]}),
            &strict
        ));
        assert!(!complete(
            &json!({"participants":[{"key":"shell/other","service":"shell","state":"presented","operation_id":"new"}]}),
            &strict
        ));
        assert!(complete(
            &json!({"participants":[{"key":"shell/control","service":"shell","state":"presented","operation_id":"new"},
            {"key":"shell/other","service":"shell","state":"hidden","operation_id":"new"}]}),
            &strict
        ));
    }

    #[test]
    fn late_settlement_preserves_predeadline_proof_and_refuses_afterdeadline_proof() {
        let spec = parse(
            r#"{"operation_id":"new","services":["term"],"until":"presented","timeout_ms":10}"#,
        )
        .unwrap();
        let admitted = Instant::now();
        let deadline = admitted + Duration::from_millis(10);
        let hidden =
            json!({"participants":[{"service":"term","state":"hidden","operation_id":"new"}]});
        let presented =
            json!({"participants":[{"service":"term","state":"presented","operation_id":"new"}]});
        let mut before = History::default();
        before.observe(hidden.clone(), admitted);
        before.observe(presented.clone(), admitted + Duration::from_millis(9));
        before.observe(hidden.clone(), admitted + Duration::from_millis(11));
        let delayed = on_time(&before, &spec, 1, deadline)
            .expect("predeadline proof survives a later hidden event and delayed settlement");
        assert_eq!(delayed.at, admitted + Duration::from_millis(9));
        let mut after = History::default();
        after.observe(hidden, admitted);
        after.observe(presented, admitted + Duration::from_millis(11));
        assert!(
            complete(&after.latest, &spec),
            "latest state is genuinely presented"
        );
        assert!(
            on_time(&after, &spec, 1, deadline).is_none(),
            "later native proof cannot be marked on time"
        );
    }
}

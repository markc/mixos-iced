// SPDX-License-Identifier: MIT OR Apache-2.0
//! Finite native requests over copied owner receipts. No periodic reads.
use std::{collections::{BTreeSet,VecDeque}, future::Future, time::{Duration,Instant}};
use application::native_actor::Accepted;
use application::native_queue::Permit;
use serde_json::{Value,json};
use tokio::sync::watch;

#[derive(Clone)]
pub(crate) struct Observed {sequence:u64,at:Instant,value:Value}
#[derive(Clone,Default)]
pub(crate) struct History {records:VecDeque<Observed>,latest:Value,fingerprint:Value,sequence:u64}
impl History {
    pub(crate) fn observe(&mut self,value:Value,at:Instant)->bool {
        if self.latest==value {return false;}
        let mut fingerprint=value.clone();
        if let Some(rows)=fingerprint["participants"].as_array_mut() {
            for row in rows {if !row["presentation"].is_null() {row["presentation"]=row["presentation"]["stamp"].clone();}}
        }
        if self.records.is_empty() || self.fingerprint!=fingerprint {
            let Some(sequence)=self.sequence.checked_add(1) else {return false;};
            self.sequence=sequence;
            self.records.push_back(Observed {sequence,at,value:value.clone()});
            if self.records.len()>128 {self.records.pop_front();}
            self.fingerprint=fingerprint;
        }
        self.latest=value;
        true
    }
}

pub(crate) struct Spec { operation:String, services:BTreeSet<String>, timeout:Duration, presented:bool, closed:bool, keys:Option<BTreeSet<String>> }
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct WireSpec {operation_id:String,services:Vec<String>,timeout_ms:u64,#[serde(default)] until:Option<String>,#[serde(default)] keys:Option<Vec<String>>}
pub(crate) fn parse(body:&str)->Option<Spec> {
    if body.len()>16*1024 {return None;}
    let value:WireSpec=serde_json::from_str(body).ok()?;
    let (presented,closed)=match value.until.as_deref() {None|Some("settled")=>(false,false),Some("presented")=>(true,false),Some("closed")=>(false,true),_=>return None};
    let keys=if let Some(rows)=value.keys {
        if rows.is_empty() || rows.len()>128 {return None;}
        let mut keys=BTreeSet::new();
        for key in rows {if key.is_empty() || key.len()>512 || !keys.insert(key) {return None;}}
        Some(keys)
    } else {None};
    let operation=value.operation_id;
    if operation.is_empty() || operation.len()>128 {return None;}
    let timeout=value.timeout_ms;
    if !(1..=60_000).contains(&timeout) {return None;}
    let rows=value.services;
    if rows.is_empty() || rows.len()>128 {return None;}
    let mut services=BTreeSet::new();
    for service in rows {if service.is_empty() || service.len()>256 || !services.insert(service) {return None;}}
    Some(Spec {operation,services,timeout:Duration::from_millis(timeout),presented,closed,keys})
}

fn complete(value:&Value,spec:&Spec)->bool {
    let Some(rows)=value["participants"].as_array() else {return false;};
    if spec.keys.as_ref().is_some_and(|keys|keys.iter().any(|key|!rows.iter().any(|row|row["key"]==key.as_str()))) {return false;}
    spec.services.iter().all(|service| {
        let selected:Vec<_>=rows.iter().filter(|row|row["service"]==service.as_str()
            && spec.keys.as_ref().is_none_or(|keys|row["key"].as_str().is_some_and(|key|keys.contains(key)))).collect();
        !selected.is_empty() && selected.iter().all(|row| {
            if spec.closed {return row["state"]=="closed";}
            if spec.presented {return row["operation_id"]==spec.operation && row["state"]=="presented";}
            if row["state"]=="closed" {return true;}
            row["operation_id"]==spec.operation && matches!(row["state"].as_str(),Some("presented"|"hidden"|"minimised"|"inactive_session"))
        })
    })
}

fn on_time<'a>(history:&'a History,spec:&Spec,baseline:u64,deadline:Instant)->Option<&'a Observed> {
    history.records.iter().find(|observed|observed.sequence>=baseline
        && observed.at<=deadline && complete(&observed.value,spec))
}

pub(crate) fn run(request:Accepted,spec:Spec,mut receipts:watch::Receiver<History>)->(Permit,impl Future<Output=Result<(),String>>+Send+'static) {
    let baseline=receipts.borrow().records.iter().rev().find(|record|record.at<=request.admitted_at())
        .map_or(0,|record|record.sequence);
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
        "services":spec.services,"keys":spec.keys,"until":if spec.presented {"presented"} else if spec.closed {"closed"} else {"settled"},"deadline_elapsed":elapsed,"observations":value}).to_string();
    tokio::time::timeout(Duration::from_secs(1),client.respond_parts(command.generation,&command.from,&command.command,command.id.as_deref(),0,&body))
        .await.map_err(|_|"participant reply timed out".to_owned())?
        .map_err(|error|error.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_wait_refuses_ambiguous_or_unbounded_requests() {
        assert!(parse(r#"{"operation_id":"op","services":["term","shell"],"timeout_ms":1000}"#).is_some());
        for body in [r#"{"operation_id":"op","services":["term","term"],"timeout_ms":1000}"#,
            r#"{"operation_id":"op","operation_id":"other","services":["term"],"timeout_ms":1000}"#,
            r#"{"operation_id":"op","services":["term"],"timeout_ms":60001}"#,
            r#"{"operation_id":"op","services":[],"timeout_ms":1000}"#,
            r#"{"operation_id":"op","services":["term"],"timeout_ms":1000,"deadline_elapsed":true}"#] {
            assert!(parse(body).is_none());
        }
    }
    #[test]
    fn hidden_phase_is_terminal_only_for_the_actual_changed_operation() {
        let spec=parse(r#"{"operation_id":"new","services":["term"],"timeout_ms":1000}"#).unwrap();
        assert!(!complete(&json!({"participants":[{"service":"term","state":"hidden","operation_id":"old"}]}),&spec));
        assert!(complete(&json!({"participants":[{"service":"term","state":"hidden","operation_id":"new","phase":"applied"}]}),&spec));
        assert!(!complete(&json!({"participants":[{"service":"term","state":"awaiting_presentation","operation_id":"new"}]}),&spec));
        assert!(!complete(&json!({"participants":[]}),&spec));
        let strict=parse(r#"{"operation_id":"new","services":["shell"],"keys":["shell/control"],"until":"presented","timeout_ms":1000}"#).unwrap();
        assert!(!complete(&json!({"participants":[{"key":"shell/control","service":"shell","state":"hidden","operation_id":"new"}]}),&strict));
        assert!(!complete(&json!({"participants":[{"key":"shell/other","service":"shell","state":"presented","operation_id":"new"}]}),&strict));
        assert!(complete(&json!({"participants":[{"key":"shell/control","service":"shell","state":"presented","operation_id":"new"},
            {"key":"shell/other","service":"shell","state":"hidden","operation_id":"new"}]}),&strict));
    }

    #[test]
    fn late_settlement_preserves_predeadline_proof_and_refuses_afterdeadline_proof() {
        let spec=parse(r#"{"operation_id":"new","services":["term"],"until":"presented","timeout_ms":10}"#).unwrap();
        let admitted=Instant::now(); let deadline=admitted+Duration::from_millis(10);
        let hidden=json!({"participants":[{"service":"term","state":"hidden","operation_id":"new"}]});
        let presented=json!({"participants":[{"service":"term","state":"presented","operation_id":"new"}]});
        let mut before=History::default();
        before.observe(hidden.clone(),admitted);
        before.observe(presented.clone(),admitted+Duration::from_millis(9));
        before.observe(hidden.clone(),admitted+Duration::from_millis(11));
        let delayed=on_time(&before,&spec,1,deadline).expect("predeadline proof survives a later hidden event and delayed settlement");
        assert_eq!(delayed.at,admitted+Duration::from_millis(9));
        let mut after=History::default(); after.observe(hidden,admitted);
        after.observe(presented,admitted+Duration::from_millis(11));
        assert!(complete(&after.latest,&spec),"latest state is genuinely presented");
        assert!(on_time(&after,&spec,1,deadline).is_none(),"later native proof cannot be marked on time");
    }
}

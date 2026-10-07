// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fixture-only frame commands sharing the application's real observer.

use crate::{frames::{Expected, Fence, FrameObservation, FrameOutcome, FrameStamp, Handle}, iced::window::Id};
use bus::native_client::IncomingCommand;
use serde_json::{Value, json};
use std::{sync::{Arc, Mutex}, time::{Duration, Instant}};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    pub window: Id,
    pub stamp: Option<FrameStamp>,
}

/// One bounded target mailbox, published by the existing UI owner after
/// activation and actual window creation. Publication requests no redraw.
#[derive(Clone)]
pub struct Endpoint {
    handle: Handle,
    target: Arc<Mutex<Option<Target>>>,
}

impl Endpoint {
    pub fn new(handle: Handle) -> Self { Self {handle, target:Arc::new(Mutex::new(None))} }

    pub fn handle(&self) -> &Handle { &self.handle }

    /// A replacement window needs a new endpoint and observer. Closing this
    /// owner prevents old callbacks or waiters from certifying that window.
    pub fn publish(&self, target: Target) -> Result<(), &'static str> {
        let mut current = self.target.lock().unwrap();
        if current.is_some_and(|previous| previous.window != target.window) {
            drop(current);
            self.handle.close();
            return Err("replacement window requires a new frame owner");
        }
        *current = Some(target);
        Ok(())
    }

    pub fn target(&self) -> Option<Target> { *self.target.lock().unwrap() }

    fn check_generation(&self, incoming: &IncomingCommand) -> Result<(), String> {
        if self.handle.snapshot().live_generation != Some(incoming.generation) {
            return Err("frame owner has no matching live generation".into());
        }
        Ok(())
    }

    pub(super) fn state(&self, incoming: &IncomingCommand) -> Result<String, String> {
        let body = super::parse_body(&incoming.body)?;
        super::check_barrier_fields(&body, &["run","instance","generation"])?;
        self.check_generation(incoming)?;
        Ok(json!({"ok":true,"target":self.target().map(|target| json!({"window":target.window.raw(),"stamp":target.stamp.map(stamp_json)})),
            "evidence":snapshot_json(&self.handle.snapshot())}).to_string())
    }

    pub(super) async fn wait(&self, incoming: &IncomingCommand, fence: Fence) -> Result<String, String> {
        let body = super::parse_body(&incoming.body)?;
        super::check_barrier_fields(&body, &["run","instance","generation","window","activation_epoch","local_revision","timeout_ms"])?;
        self.check_generation(incoming)?;
        let (window, stamp, timeout) = parse_wait(&body)?;
        let target = self.target().ok_or_else(|| "frame target not ready".to_owned())?;
        if target.window.raw() != window { return Err("wrong frame window".into()); }
        if target.stamp.is_none() { return Err("installed frame stamp not ready".into()); }
        let receipt = self.handle.wait(Expected {window:target.window, stamp, fence}, Instant::now() + timeout)
            .await.map_err(|error| format!("frame wait: {error:?}"))?;
        Ok(json!({"ok":true,"receipt":observation_json(receipt)}).to_string())
    }
}

fn parse_wait(body: &Value) -> Result<(u64, FrameStamp, Duration), String> {
    let number = |field| body.get(field).and_then(Value::as_u64).ok_or_else(|| format!("missing or invalid {field}"));
    let window = number("window")?;
    let activation_epoch = number("activation_epoch")?;
    if activation_epoch == 0 { return Err("activation_epoch must be nonzero".into()); }
    let local_revision = number("local_revision")?;
    let timeout_ms = match body.get("timeout_ms") {None=>2000, Some(_)=>number("timeout_ms")?};
    if !(1..=10000).contains(&timeout_ms) { return Err("timeout_ms must be in 1..=10000".into()); }
    Ok((window, FrameStamp {activation_epoch, local_revision}, Duration::from_millis(timeout_ms)))
}

pub fn stamp_json(stamp: FrameStamp) -> Value {
    json!({"activation_epoch":stamp.activation_epoch,"local_revision":stamp.local_revision})
}

/// Encode copied native metadata once for every application. Historical
/// receipts carry their own stamp and never acquire current Bus provenance.
pub fn observation_json(receipt: FrameObservation) -> Value {
    let outcome = match receipt.outcome {
        FrameOutcome::Presented {clock_id,seconds,nanoseconds,refresh_ns,output_sequence,flags} => json!({
            "kind":"presented","clock_id":clock_id,"seconds":seconds,"nanoseconds":nanoseconds,
            "refresh_ns":refresh_ns,"output_sequence":output_sequence,"flags":flags}),
        other => json!({"kind":match other {
            FrameOutcome::Discarded=>"discarded", FrameOutcome::Unsupported=>"unsupported",
            FrameOutcome::Capacity=>"capacity", FrameOutcome::Exhausted=>"exhausted",
            FrameOutcome::Closed=>"closed", FrameOutcome::SubmissionFailed=>"submission_failed",
            FrameOutcome::Presented {..}=>unreachable!(),
        }}),
    };
    json!({"window":receipt.window.raw(),"stamp":stamp_json(receipt.stamp),"request_id":receipt.request_id,"outcome":outcome})
}

pub fn snapshot_json(snapshot: &crate::frames::Snapshot) -> Value {
    json!({"window":snapshot.window.map(Id::raw),"closed":snapshot.closed,
        "live_generation":snapshot.live_generation,"lifecycle_revision":snapshot.lifecycle_revision,
        "last_observation":snapshot.last_observation.map(observation_json),
        "last_presented":snapshot.last_presented.map(observation_json)})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_deadline_and_stamp_requirements() {
        let valid = json!({"window":1,"activation_epoch":2,"local_revision":0});
        assert_eq!(parse_wait(&valid).unwrap().2, Duration::from_secs(2));
        for value in [json!(0),json!(10001),json!(-1),json!(1.5),json!("2000"),Value::Null] {
            let mut bad = valid.clone(); bad["timeout_ms"] = value;
            assert!(parse_wait(&bad).is_err());
        }
        let mut zero = valid; zero["activation_epoch"] = json!(0);
        assert!(parse_wait(&zero).is_err());
    }

    #[test]
    fn target_replacement_closes_instead_of_relabelling_the_original_window() {
        let endpoint = Endpoint::new(Handle::new());
        let a = Id::unique(); let b = Id::unique();
        endpoint.publish(Target {window:a,stamp:None}).unwrap();
        assert!(endpoint.publish(Target {window:b,stamp:None}).is_err());
        assert_eq!(endpoint.target().unwrap().window,a);
        assert!(endpoint.handle.snapshot().closed);
    }

    #[test]
    fn encoded_history_retains_native_clock_and_drawn_identity() {
        let handle = Handle::new();
        let window = Id::unique();
        let stamp = FrameStamp {activation_epoch:9,local_revision:3};
        handle.binding(stamp).observe(window,Some(71),FrameOutcome::Presented {
            clock_id:Some(4),seconds:101,nanoseconds:999,refresh_ns:16,output_sequence:81,flags:7});
        handle.set_live_generation(Some(20));
        let evidence = snapshot_json(&handle.snapshot());
        assert_eq!(evidence["live_generation"],20);
        let receipt = &evidence["last_presented"];
        assert_eq!(receipt["stamp"]["activation_epoch"],9);
        assert_eq!(receipt["request_id"],71);
        assert_eq!(receipt["outcome"]["clock_id"],4);
        assert_eq!(receipt["outcome"]["output_sequence"],81);
        assert!(receipt.get("live_generation").is_none());
        handle.binding(stamp).observe(window,None,FrameOutcome::Unsupported);
        let evidence = snapshot_json(&handle.snapshot());
        assert_eq!(evidence["last_observation"]["outcome"]["kind"],"unsupported");
        assert_eq!(evidence["last_presented"]["request_id"],71);
    }

    #[tokio::test]
    async fn exact_history_satisfies_without_drawing_but_old_fence_and_foreign_window_fail() {
        let handle = Handle::new();
        let endpoint = Endpoint::new(handle.clone());
        let window = Id::unique();
        let stamp = FrameStamp {activation_epoch:5,local_revision:0};
        handle.set_live_generation(Some(7));
        endpoint.publish(Target {window,stamp:Some(stamp)}).unwrap();
        handle.binding(stamp).observe(window,Some(2),FrameOutcome::Presented {
            clock_id:None,seconds:1,nanoseconds:2,refresh_ns:3,output_sequence:4,flags:5});
        let command = |window:Id| IncomingCommand {generation:7,from:"fixture".into(),command:"app.acceptance.frame.wait".into(),id:None,args:Value::Null,
            body:json!({"run":"owned","instance":1,"window":window.raw(),"activation_epoch":5,"local_revision":0}).to_string(),headers:Default::default()};
        let before = handle.snapshot();
        let result = endpoint.wait(&command(window),handle.fence()).await.unwrap();
        assert_eq!(serde_json::from_str::<Value>(&result).unwrap()["receipt"]["request_id"],2);
        assert_eq!(before,handle.snapshot(),"query must not manufacture a redraw or receipt");
        assert!(endpoint.wait(&command(Id::unique()),handle.fence()).await.unwrap_err().contains("wrong frame window"));
        let stale = handle.fence();
        handle.set_live_generation(None);
        handle.set_live_generation(Some(7));
        assert!(endpoint.wait(&command(window),stale).await.unwrap_err().contains("LifecycleChanged"));
        let mut extra = command(window);
        let mut body:Value = serde_json::from_str(&extra.body).unwrap();
        body["lifecycle_revision"] = json!(0);
        extra.body = body.to_string();
        assert!(endpoint.wait(&extra,handle.fence()).await.unwrap_err().contains("unknown request field"));
    }
}

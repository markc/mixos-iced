// SPDX-License-Identifier: MIT OR Apache-2.0
//! Shared native acceptance command parsing and routing.
//!
//! The diagnostic verbs live under `app.acceptance.*` and exist only when an
//! application enables this feature and its fixture launch configuration
//! registers them; default launches register nothing and the feature is not
//! in `application`'s defaults. Ordinary native authentication and admission
//! apply, and ordinary product mutation policy is unchanged.
//!
//! [`track`] is a parsing facade: it receives the originating
//! [`IncomingCommand`] and returns the tracked reply future for the same
//! client, without owning it. The future replies through the existing
//! [`SupervisedClient`] response path, retaining `IncomingCommand.generation`
//! so an old generation cannot answer a new connection. This module owns no
//! client, runtime, thread, operation payload or product busy state, and it
//! spawns nothing: the application's existing Bus worker tracks the future
//! alongside its other responses.

pub mod barrier;
pub mod frames;

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use bus::native_client::{ConnState, IncomingCommand, SupervisedClient};
use serde_json::{Value, json};

use crate::inspect;
use crate::inspect::AliasStatus;

/// The verb prefix of the diagnostic acceptance commands.
pub const VERB_PREFIX: &str = "app.acceptance.";

/// Exact fixture verbs. Unknown prefix matches belong to the caller.
pub const VERBS: &[&str] = &[
    "app.acceptance.describe", "app.acceptance.layout",
    "app.acceptance.barrier.arm", "app.acceptance.barrier.wait",
    "app.acceptance.barrier.release", "app.acceptance.barrier.state",
    "app.acceptance.frame.state", "app.acceptance.frame.wait",
];

pub fn recognises(verb: &str) -> bool { VERBS.contains(&verb) }

/// Final sends have their own bound; the actor retains admission through reap.
pub const REPLY_DEADLINE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackError {
    Retired,
    ReplyTimedOut,
    ReplyFailed(String),
}

/// The bounded reply deadline the Bus worker applies to a layout query.
pub const QUERY_DEADLINE: Duration = Duration::from_secs(2);

/// Explicit owned fixture launch identity. Ordinary launches have neither
/// variable set. Partial, invalid or non-Unicode identities fail startup.
#[derive(Debug, Clone)]
pub struct Launch {
    pub run: String,
    pub instance: u64,
}

impl Launch {
    pub fn from_env() -> Result<Option<Self>, String> {
        let read = |name| match std::env::var(name) {
            Ok(value) => Ok(Some(value)),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(std::env::VarError::NotUnicode(_)) => Err(format!("{name} must be Unicode")),
        };
        Self::parse(read("MIXOS_ACCEPTANCE_RUN")?.as_deref(), read("MIXOS_ACCEPTANCE_INSTANCE")?.as_deref())
    }

    fn parse(run: Option<&str>, instance: Option<&str>) -> Result<Option<Self>, String> {
        match (run, instance) {
            (None,None) => Ok(None),
            (Some(run),Some(instance)) if !run.is_empty() && run.len() <= barrier::MAX_STRING => {
                let instance = instance.parse::<u64>().ok().filter(|value| *value != 0)
                    .ok_or_else(|| "fixture instance must be a nonzero u64".to_owned())?;
                Ok(Some(Self {run:run.to_owned(),instance}))
            }
            _ => Err("fixture launch requires bounded run and nonzero instance together".into()),
        }
    }
}

/// The per-process acceptance identity, from the fixture launch
/// configuration. It is shared by the describe verb and the fence
/// validation of every mutating verb.
#[derive(Debug, Clone)]
pub struct Describe {
    /// The owned process id.
    pub pid: u32,
    /// The fixture run id, at most [`barrier::MAX_STRING`] bytes.
    pub run: String,
    /// The per-process instance nonce.
    pub instance: u64,
    /// The enabled barrier points.
    pub points: Vec<String>,
    /// The enabled layout aliases.
    pub aliases: Vec<String>,
    /// The bounded query limits.
    pub limits: inspect::Limits,
}

impl Describe {
    /// A describe identity for the given process, run and instance.
    pub fn new(pid: u32, run: impl Into<String>, instance: u64) -> Result<Self, barrier::Error> {
        let run = run.into();
        if run.is_empty() || run.len() > barrier::MAX_STRING {
            return Err(barrier::Error::BadRun { bytes: run.len() });
        }
        Ok(Self {
            pid,
            run,
            instance,
            points: Vec::new(),
            aliases: Vec::new(),
            limits: inspect::Limits::new(),
        })
    }

    /// The enabled barrier points.
    pub fn points(mut self, points: Vec<String>) -> Self {
        self.points = points;
        self
    }

    /// The enabled layout aliases.
    pub fn aliases(mut self, aliases: Vec<String>) -> Self {
        self.aliases = aliases;
        self
    }

    /// The bounded query limits.
    pub fn limits(mut self, limits: inspect::Limits) -> Self {
        self.limits = limits;
        self
    }
}

/// Parses one incoming command and returns the tracked reply future for the
/// same client, or `None` when the command is not a registered
/// `app.acceptance.*` verb. The caller owns and drives the future; this
/// function spawns nothing.
pub fn track(
    client: Arc<SupervisedClient>,
    incoming: IncomingCommand,
    describe: &Describe,
    inspector: &inspect::Handle,
    controller: &barrier::Controller,
) -> Option<impl Future<Output = ()> + Send + 'static> {
    let future = track_result(client, incoming, describe, inspector, controller, None)?;
    Some(async move { let _ = future.await; })
}

/// Result-bearing tracked operation. The existing actor owns admission,
/// cancellation and reaping; this facade owns no tasks or connection.
pub fn track_result(
    client: Arc<SupervisedClient>,
    incoming: IncomingCommand,
    describe: &Describe,
    inspector: &inspect::Handle,
    controller: &barrier::Controller,
    frames: Option<&frames::Endpoint>,
) -> Option<impl Future<Output = Result<(), TrackError>> + Send + 'static> {
    if !recognises(&incoming.command) { return None; }
    let verb = incoming.command.strip_prefix(VERB_PREFIX)?.to_owned();
    let describe = describe.clone();
    let inspector = inspector.clone();
    let controller = controller.clone();
    let frames = frames.cloned();
    // Capture on native admission, before a queued task is first polled.
    let frame_fence = frames.as_ref().map(|endpoint| endpoint.handle().fence());
    let admitted_at = Instant::now();

    Some(async move {
        // Queued work may first be polled after the receiving socket retired.
        // This is a live sample; the owning worker also cancels its controller
        // on lifecycle changes. The reply remains socket-fenced by Bus.
        if !live_generation(&client, incoming.generation) {
            return Err(TrackError::Retired);
        }
        let body = match validate_fixture(&describe, &incoming) {
            Err(error) => Err(error),
            Ok(()) => match verb.as_str() {
            "describe" => Ok(describe_json(&describe, frames.is_some())),
            "layout" => layout_verb(&describe, &inspector, &incoming, admitted_at).await,
            "barrier.arm" => barrier_arm_verb(&controller, &incoming),
            "barrier.wait" => barrier_wait_verb(&controller, &incoming).await,
            "barrier.release" => barrier_release_verb(&controller, &incoming),
            "barrier.state" => barrier_state_verb(&controller, &incoming),
            "frame.state" => frames.as_ref().ok_or_else(|| "frame evidence unsupported".to_owned()).and_then(|endpoint| endpoint.state(&incoming)),
            "frame.wait" => match (&frames, frame_fence) {
                (Some(endpoint), Some(fence)) => endpoint.wait(&incoming, fence, admitted_at).await,
                _ => Err("frame evidence unsupported".to_owned()),
            },
            _ => unreachable!("exact registered verb"),
            },
        };

        if !live_generation(&client, incoming.generation) {
            return Err(TrackError::Retired);
        }
        let body = body.unwrap_or_else(error_json);
        tokio::time::timeout(REPLY_DEADLINE, client
            .respond_parts(
                incoming.generation,
                &incoming.from,
                &incoming.command,
                incoming.id.as_deref(),
                0,
                &body,
            ))
            .await.map_err(|_| TrackError::ReplyTimedOut)?
            .map_err(|error| TrackError::ReplyFailed(error.to_string()))
    })
}

fn live_generation(client: &SupervisedClient, generation: u64) -> bool {
    generation != 0
        && client.connection_generation() == generation
        && client.state() == ConnState::Connected
        && client.connection_generation() == generation
}

pub(super) fn parse_body(body: &str) -> Result<Value, String> {
    if body.len() > 16_384 {
        return Err("request body exceeds 16384 bytes".into());
    }
    let value: Value = serde_json::from_str(if body.trim().is_empty() { "{}" } else { body })
        .map_err(|error| format!("bad request body: {error}"))?;
    if !value.is_object() {
        return Err("request body must be an object".into());
    }
    Ok(value)
}

fn error_json(error: String) -> String {
    serde_json::to_string(&json!({ "ok": false, "error": error }))
        .unwrap_or_else(|_| "{\"ok\":false}".to_owned())
}

fn describe_json(describe: &Describe, frames: bool) -> String {
    serde_json::to_string(&json!({
        "ok": true,
        "pid": describe.pid,
        "run": describe.run,
        "instance": describe.instance,
        "points": describe.points,
        "aliases": describe.aliases,
        "protocol": 1,
        "frames": {"enabled":frames,"waiters":1,"default_timeout_ms":2000,"max_timeout_ms":10000,"retained_records":2},
        "limits": {
            "aliases": describe.limits.max_aliases(),
            "alias_bytes": describe.limits.max_alias_bytes(),
            "records": describe.limits.max_records(),
            "visited": describe.limits.max_visited(),
            "encoded_bytes": describe.limits.max_encoded_bytes(),
        },
    }))
    .unwrap_or_else(|_| "{\"ok\":false,\"error\":\"describe encoding failed\"}".to_owned())
}

async fn layout_verb(
    describe: &Describe,
    inspector: &inspect::Handle,
    incoming: &IncomingCommand,
    admitted_at: Instant,
) -> Result<String, String> {
    let deadline = admitted_at + QUERY_DEADLINE;
    if Instant::now() >= deadline { return Err("layout query timed out before dispatch".into()); }
    let body = parse_body(&incoming.body)?;
    check_barrier_fields(&body, &["run","instance","generation","window","layer","aliases"])?;
    if body.get("layer").is_some_and(|layer| !layer.is_string()) {
        return Err("layer must be a string".into());
    }
    if body.get("aliases").is_some_and(|aliases| !aliases.is_array()) {
        return Err("aliases must be an array".into());
    }

    let window = match body.get("window") {
        None => inspect::Window::Only,
        Some(Value::String(value)) if value == "only" => inspect::Window::Only,
        Some(Value::String(value)) => value
            .parse::<u64>()
            .map(inspect::Window::Id)
            .map_err(|_| format!("bad window id: {value}"))?,
        Some(Value::Number(value)) => value
            .as_u64()
            .map(inspect::Window::Id)
            .ok_or_else(|| format!("bad window id: {value}"))?,
        Some(_) => return Err("window must be \"only\" or a runtime id".to_owned()),
    };

    let layer = match body.get("layer").and_then(Value::as_str) {
        None | Some("base") => inspect::Layer::Base,
        Some("overlay") => inspect::Layer::Overlay,
        Some(other) => return Err(format!("unknown layer: {other}")),
    };

    let mut request = inspect::Request::new(window).layer(layer);
    if let Some(aliases) = body.get("aliases").and_then(Value::as_array) {
        let aliases = aliases
            .iter()
            .map(|alias| alias.as_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| "aliases must be strings".to_owned())?;
        request = request.aliases(aliases);
    }

    if Instant::now() >= deadline { return Err("layout query timed out before dispatch".into()); }
    let snapshot = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), inspector.query(request))
        .await
        .map_err(|_| "layout query timed out".to_owned())?
        .map_err(|error| format!("layout query: {error:?}"))?;

    snapshot_json(&snapshot, &describe.limits)
}

fn snapshot_json(snapshot: &inspect::Snapshot, limits: &inspect::Limits) -> Result<String, String> {
    let bounds = |rectangle: iced::Rectangle| {
        json!([rectangle.x, rectangle.y, rectangle.width, rectangle.height])
    };
    let value = json!({
        "ok": true,
        "window_id": snapshot.window_id,
        "logical_size": [snapshot.logical_size.width, snapshot.logical_size.height],
        "layout_sequence": snapshot.layout_sequence,
        "layer": layer_name(snapshot.layer),
        "visited": snapshot.visited,
        "truncated": snapshot.truncated,
        "records": snapshot.records.iter().map(|record| json!({
            "alias": record.alias,
            "kind": kind_name(record.kind),
            "layout_bounds": bounds(record.layout_bounds),
            "visible_bounds": record.visible_bounds.map(bounds),
            "layer": layer_name(record.layer),
        })).collect::<Vec<_>>(),
        "aliases": snapshot.aliases.iter().map(|result| json!({
            "alias": result.alias,
            "status": match result.status {
                AliasStatus::Found => "found",
                AliasStatus::Missing => "missing",
                AliasStatus::Ambiguous => "ambiguous",
            },
        })).collect::<Vec<_>>(),
    });

    let body = serde_json::to_string(&value).map_err(|error| error.to_string())?;
    if body.len() > limits.max_encoded_bytes() {
        return Err(format!(
            "encoded response of {} bytes exceeds the {} byte limit",
            body.len(),
            limits.max_encoded_bytes()
        ));
    }
    Ok(body)
}

fn barrier_arm_verb(
    controller: &barrier::Controller,
    incoming: &IncomingCommand,
) -> Result<String, String> {
    let body = parse_body(&incoming.body)?;
    check_barrier_fields(&body, &["run", "instance", "generation", "point", "token"])?;
    check_request_generation(&body, incoming.generation)?;
    let token = barrier::Token::try_new(
        body.get("token")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing token".to_owned())?,
    )
    .map_err(|error| format!("{error:?}"))?;

    let receipt = controller
        .arm(barrier::Arm {
            fence: barrier::Fence {
                run: body
                    .get("run")
                    .and_then(Value::as_str)
                    .filter(|run| !run.is_empty() && run.len() <= barrier::MAX_STRING)
                    .ok_or_else(|| "missing run".to_owned())?
                    .to_owned(),
                instance: body
                    .get("instance")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| "missing instance".to_owned())?,
                generation: incoming.generation,
            },
            point: body
                .get("point")
                .and_then(Value::as_str)
                .filter(|point| !point.is_empty() && point.len() <= barrier::MAX_STRING)
                .ok_or_else(|| "missing point".to_owned())?
                .to_owned(),
            token,
        })
        .map_err(|error| format!("{error:?}"))?;

    Ok(receipt_json(&receipt))
}

async fn barrier_wait_verb(
    controller: &barrier::Controller,
    incoming: &IncomingCommand,
) -> Result<String, String> {
    let reference = parse_reference(incoming)?;
    let receipt = controller
        .wait_fenced(&reference)
        .await
        .map_err(|error| format!("{error:?}"))?;
    Ok(receipt_json(&receipt))
}

fn barrier_release_verb(
    controller: &barrier::Controller,
    incoming: &IncomingCommand,
) -> Result<String, String> {
    let reference = parse_reference(incoming)?;
    let receipt = controller
        .release_fenced(&reference)
        .map_err(|error| format!("{error:?}"))?;
    Ok(receipt_json(&receipt))
}

fn barrier_state_verb(
    controller: &barrier::Controller,
    incoming: &IncomingCommand,
) -> Result<String, String> {
    let reference = parse_reference(incoming)?;
    let receipt = controller
        .snapshot_fenced(&reference)
        .map_err(|error| format!("{error:?}"))?;
    Ok(receipt_json(&receipt))
}

fn parse_reference(incoming: &IncomingCommand) -> Result<barrier::Reference, String> {
    let body = parse_body(&incoming.body)?;
    check_barrier_fields(
        &body,
        &["run", "instance", "generation", "sequence", "token"],
    )?;
    check_request_generation(&body, incoming.generation)?;
    let token = barrier::Token::try_new(
        body.get("token")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing token".to_owned())?,
    )
    .map_err(|error| format!("{error:?}"))?;
    Ok(barrier::Reference {
        fence: barrier::Fence {
            run: body
                .get("run")
                .and_then(Value::as_str)
                .filter(|run| !run.is_empty() && run.len() <= barrier::MAX_STRING)
                .ok_or_else(|| "missing or oversized run".to_owned())?
                .to_owned(),
            instance: body
                .get("instance")
                .and_then(Value::as_u64)
                .ok_or_else(|| "missing instance".to_owned())?,
            generation: incoming.generation,
        },
        sequence: body
            .get("sequence")
            .and_then(Value::as_u64)
            .ok_or_else(|| "missing sequence".to_owned())?,
        token,
    })
}

pub(super) fn check_request_generation(body: &Value, generation: u64) -> Result<(), String> {
    if let Some(requested) = body.get("generation")
        && requested.as_u64() != Some(generation)
    {
        return Err("stale or invalid connection generation".into());
    }
    Ok(())
}

pub(super) fn check_barrier_fields(body: &Value, allowed: &[&str]) -> Result<(), String> {
    if body
        .as_object()
        .expect("validated object")
        .keys()
        .any(|key| !allowed.contains(&key.as_str()))
    {
        return Err("unknown request field".into());
    }
    Ok(())
}

fn validate_fixture(describe: &Describe, incoming: &IncomingCommand) -> Result<(), String> {
    let body = parse_body(&incoming.body)?;
    if body.get("run").and_then(Value::as_str) != Some(describe.run.as_str())
        || body.get("instance").and_then(Value::as_u64) != Some(describe.instance) {
        return Err("wrong fixture run or process instance".into());
    }
    check_request_generation(&body, incoming.generation)?;
    if incoming.command == "app.acceptance.describe" {
        check_barrier_fields(&body, &["run","instance","generation"])?;
    }
    Ok(())
}

fn receipt_json(receipt: &barrier::Receipt) -> String {
    serde_json::to_string(&json!({
        "ok": true,
        "run": receipt.run,
        "sequence": receipt.sequence,
        "token": receipt.token.as_str(),
        "state": state_name(receipt.state),
        "point": receipt.point,
        "observation": receipt.observation.as_ref().map(|observation| observation.as_str()),
        "instance": receipt.instance,
        "generation": receipt.generation,
    }))
    .unwrap_or_else(|_| "{\"ok\":false,\"error\":\"receipt encoding failed\"}".to_owned())
}

fn layer_name(layer: inspect::Layer) -> &'static str {
    match layer {
        inspect::Layer::Base => "base",
        inspect::Layer::Overlay => "overlay",
    }
}

fn kind_name(kind: inspect::Kind) -> &'static str {
    match kind {
        inspect::Kind::Container => "container",
        inspect::Kind::Focusable => "focusable",
        inspect::Kind::Scrollable => "scrollable",
        inspect::Kind::TextInput => "text_input",
        inspect::Kind::Text => "text",
        inspect::Kind::Custom => "custom",
    }
}

fn state_name(state: barrier::State) -> &'static str {
    match state {
        barrier::State::Idle => "idle",
        barrier::State::Armed => "armed",
        barrier::State::Reached => "reached",
        barrier::State::Released => "released",
        barrier::State::Cancelled => "cancelled",
        barrier::State::Expired => "expired",
        barrier::State::Closed => "closed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn incoming(verb: &str, body: Value) -> IncomingCommand {
        IncomingCommand {generation:7,from:"fixture".into(),command:verb.into(),id:None,
            args:Value::Null,body:body.to_string(),headers:Default::default()}
    }

    #[test]
    fn exact_verbs_and_fixture_identity_are_required_for_reads_too() {
        assert!(recognises("app.acceptance.frame.wait"));
        assert!(!recognises("app.acceptance.future"));
        assert!(!recognises("app.describe"));
        let describe = Describe::new(1,"owned",11).unwrap();
        for verb in VERBS {
            let mut request = incoming(verb,json!({"run":"owned","instance":11,"generation":7}));
            assert!(validate_fixture(&describe,&request).is_ok());
            request.body = json!({"run":"foreign","instance":11}).to_string();
            assert!(validate_fixture(&describe,&request).is_err());
            request.body = json!({"run":"owned","instance":12}).to_string();
            assert!(validate_fixture(&describe,&request).is_err());
            request.body = json!({"run":"owned","instance":11,"generation":8}).to_string();
            assert!(validate_fixture(&describe,&request).is_err());
        }
        assert!(validate_fixture(&describe,&incoming("app.acceptance.describe",json!({"run":"owned","instance":11,"typo":true}))).is_err());
    }

    #[test]
    fn normal_launch_is_absent_and_partial_fixture_identity_is_refused() {
        assert!(Launch::parse(None,None).unwrap().is_none());
        assert!(Launch::parse(Some("owned"),None).is_err());
        assert!(Launch::parse(None,Some("1")).is_err());
        assert!(Launch::parse(Some(""),Some("1")).is_err());
        assert!(Launch::parse(Some("owned"),Some("0")).is_err());
        assert!(Launch::parse(Some("owned"),Some("-1")).is_err());
        let launch = Launch::parse(Some("owned"),Some("31")).unwrap().unwrap();
        assert_eq!(launch.instance,31);
    }
}

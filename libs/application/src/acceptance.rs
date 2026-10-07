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

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use bus::native_client::{IncomingCommand, SupervisedClient};
use serde_json::{Value, json};

use crate::inspect;
use crate::inspect::AliasStatus;

/// The verb prefix of the diagnostic acceptance commands.
pub const VERB_PREFIX: &str = "app.acceptance.";

/// The bounded reply deadline the Bus worker applies to a layout query.
pub const QUERY_DEADLINE: Duration = Duration::from_secs(2);

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
    let verb = incoming.command.strip_prefix(VERB_PREFIX)?;
    let describe = describe.clone();
    let inspector = inspector.clone();
    let controller = controller.clone();

    Some(async move {
        let body = match verb {
            "describe" => Ok(describe_json(&describe)),
            "layout" => layout_verb(&describe, &inspector, &incoming).await,
            "barrier.arm" => barrier_arm_verb(&controller, &incoming),
            "barrier.wait" => barrier_wait_verb(&controller, &incoming).await,
            "barrier.release" => barrier_release_verb(&controller, &incoming),
            "barrier.state" => barrier_state_verb(&controller, &incoming),
            _ => return,
        };

        let body = body.unwrap_or_else(error_json);
        let _ = client
            .respond_parts(
                incoming.generation,
                &incoming.from,
                &incoming.command,
                incoming.id.as_deref(),
                0,
                &body,
            )
            .await;
    })
}

fn parse_body(body: &str) -> Result<Value, String> {
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

fn describe_json(describe: &Describe) -> String {
    serde_json::to_string(&json!({
        "ok": true,
        "pid": describe.pid,
        "run": describe.run,
        "instance": describe.instance,
        "points": describe.points,
        "aliases": describe.aliases,
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
) -> Result<String, String> {
    let body = parse_body(&incoming.body)?;

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

    let snapshot = tokio::time::timeout(QUERY_DEADLINE, inspector.query(request))
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

fn check_request_generation(body: &Value, generation: u64) -> Result<(), String> {
    if let Some(requested) = body.get("generation")
        && requested.as_u64() != Some(generation)
    {
        return Err("stale or invalid connection generation".into());
    }
    Ok(())
}

fn check_barrier_fields(body: &Value, allowed: &[&str]) -> Result<(), String> {
    if body
        .as_object()
        .expect("validated object")
        .keys()
        .any(|key| !allowed.contains(&key.as_str()))
    {
        return Err("unknown barrier request field".into());
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

// SPDX-License-Identifier: MIT OR Apache-2.0
//! One serialized authority, bounded native input, blocking work off Tokio.
use crate::{authority::Authority, store::Store};
use bus::native_client::{BoundedIncomingEvent, ConnState, SupervisedClient};
use serde::Deserialize;
use serde_json::{Value, json};
use settings::*;
use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Duration};

pub const VERBS: &[&str] = &[
    "settings.describe",
    "settings.get",
    "settings.validate",
    "settings.apply",
    "settings.reset",
    "settings.status",
];
pub fn manifest() -> Vec<bus::VerbDescriptor> {
    std::iter::once(bus::VerbDescriptor::new(
        "HELP",
        "",
        "List served verbs",
        true,
    ))
    .chain(VERBS.iter().map(|verb| {
        bus::VerbDescriptor::new(
            *verb,
            "JSON body",
            "Desktop settings contract 0.1.0",
            !matches!(*verb, "settings.apply" | "settings.reset"),
        )
    }))
    .collect()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusRequest {
    binding: Binding,
    #[serde(default)]
    operation_id: Option<String>,
}
fn decode<T: serde::de::DeserializeOwned>(body: &str) -> Result<T, Value> {
    if body.len() > 384 * 1024 {
        return Err(json!({"status":"validation_failed","message":"Request byte limit exceeded"}));
    }
    serde_json::from_str(body)
        .map_err(|e| json!({"status":"validation_failed","message":e.to_string()}))
}
pub fn dispatch(authority: &mut Authority, verb: &str, body: &str) -> Result<Value, Value> {
    match verb {
        "HELP" => Ok(json!(manifest())),
        "settings.describe" => Ok(settings::describe()),
        "settings.get" => authority.read(decode(body)?),
        "settings.status" => {
            let req: StatusRequest = decode(body)?;
            authority.status(&req.binding, req.operation_id.as_deref())
        }
        "settings.validate" => authority.validate(&decode(body)?),
        "settings.apply" => authority.apply(decode(body)?),
        "settings.reset" => {
            let req: ApplyRequest = decode(body)?;
            if !req.changes.is_empty() {
                return Err(
                    json!({"status":"validation_failed","message":"Reset accepts only reset paths"}),
                );
            }
            authority.apply(req)
        }
        _ => Err(json!({"status":"not_served","verb":verb})),
    }
}
async fn publish(authority: &mut Authority, client: &SupervisedClient) -> anyhow::Result<()> {
    let topic = settings::topic(&authority.accepted.binding.profile);
    let mut inner = bus::wire::BusMessage::new();
    inner.set("command", &topic);
    inner.body = serde_json::to_string(&authority.snapshot)?;
    let headers = BTreeMap::from([("name".into(), topic), ("retain".into(), "true".into())]);
    tokio::time::timeout(
        Duration::from_secs(3),
        client.call_with_headers("noded", "topic.publish", &headers, &inner.to_wire()),
    )
    .await??;
    authority.published = Some(authority.accepted.revision);
    Ok(())
}

pub async fn serve(root: PathBuf, binding: Binding) -> anyhow::Result<()> {
    let mut authority = tokio::task::spawn_blocking(move || {
        let (store, accepted) = Store::open(&root, &binding)?;
        Authority::new(store, accepted)
    })
    .await??;
    let build = buildinfo::build_info!();
    let provenance = bus::RegisterProvenance::from_parts(
        build.pkg,
        build.version,
        build.git_sha,
        build.git_dirty,
        build.build_time,
        buildinfo::now_rfc3339(),
    );
    let client = Arc::new(
        SupervisedClient::connect_options("settingsd", &bus::client_helpers::resolve_noded_url())
            .bounded_incoming(64)
            .fatal_on_registration_rejection(true)
            .with_verbs(manifest())
            .with_provenance(provenance)
            .connect()
            .await?,
    );
    let mut incoming = client
        .incoming_bounded()
        .ok_or_else(|| anyhow::anyhow!("native incoming already taken"))?;
    let mut state = client.subscribe_state();
    if let Err(error) = publish(&mut authority, &client).await {
        tracing::warn!(%error,"initial settings publication pending");
    }
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    loop {
        tokio::select! {
            changed = state.changed() => {
                if changed.is_err() { break; }
                let now = *state.borrow_and_update();
                if now == ConnState::Fatal { anyhow::bail!("settings authority registration rejected"); }
                if now == ConnState::Connected {
                    // Re-registration always repopulates retained state, including
                    // ordinary restart when broker remains alive. No heartbeat.
                    authority.published = None;
                    if let Err(error) = publish(&mut authority,&client).await { tracing::warn!(%error,"settings publication pending"); }
                } else { authority.published = None; }
            }
            event = incoming.recv() => {
                match event {
                    None => break,
                    Some(BoundedIncomingEvent::Overflow { dropped }) => tracing::warn!(dropped,"settings request input overflow; callers must resolve receipts"),
                    Some(BoundedIncomingEvent::Command(command)) => {
                        if command.is_topic_delivery() { continue; }
                        let verb = command.command.clone();
                        let body = command.body.clone();
                        // At most one blocking job. Durable work completes even
                        // when its reply cannot be sent; no mutation replay.
                        let result = tokio::task::spawn_blocking(move || {
                            let reply = dispatch(&mut authority,&verb,&body);
                            (authority,reply)
                        }).await?;
                        authority = result.0;
                        let mut reply = result.1;
                        if client.is_connected() && authority.published != Some(authority.accepted.revision) && !authority.store.recovering {
                            if let Err(error) = publish(&mut authority,&client).await { tracing::warn!(%error,"settings committed with publication pending"); }
                        }
                        if let Ok(ref mut value) = reply {
                            if let Some(object) = value.as_object_mut() {
                                if object.contains_key("publication_pending") { object.insert("publication_pending".into(),json!(authority.published != Some(authority.accepted.revision))); }
                            }
                        }
                        let (rc,value) = match reply { Ok(value) => (0,value), Err(error) => (10,error) };
                        let body = serde_json::to_string(&value)?;
                        if let Err(error) = client.respond(&command,rc,&body).await { tracing::warn!(%error,"settings reply unavailable; operation receipt retained"); }
                    }
                }
            }
            _ = sigterm.recv() => break,
            _ = sigint.recv() => break,
        }
    }
    let _ = tokio::time::timeout(Duration::from_secs(3), client.deregister()).await;
    Ok(())
}

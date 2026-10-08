// SPDX-License-Identifier: MIT OR Apache-2.0
//! The same document and capture commands without a Wayland window. The paired
//! settings UI stays owned (never drained) so the shared worker's lane keeps
//! running; a refused registration ends the service with an error.
use crate::{
    bus::{self, Delivery},
    capture,
    document::Document,
    verbs::{self, Operation},
};
use application::presentation::native::Ui;
use iced::futures::{FutureExt, StreamExt, future::BoxFuture, stream::FuturesUnordered};
use serde_json::{Value, json};
use std::path::PathBuf;
enum Finished {
    Captured(capture::Captured),
    Opened(PathBuf, Document),
    Exported(PathBuf),
}
type Job = BoxFuture<'static, (bus::Request, Result<Finished, String>)>;
pub fn run(service: &str, url: &str, comp: &str, path: Option<PathBuf>) -> Result<(), String> {
    let directory = capture::media_directory()?;
    let (mut document, mut current_path) = match path {
        Some(p) => (Some(Document::open(&p)?), Some(p)),
        None => (None, None),
    };
    let (handle, mut settings_ui, _bootstrap, mut deliveries) = bus::start(service, url, None)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(async {
        let mut jobs:FuturesUnordered<Job>=FuturesUnordered::new();let mut cancellation:Option<tokio::sync::watch::Sender<bool>>=None;let mut metadata=Value::Null;let mut capture_generation=0u64;
        loop {tokio::select! {
            completed=jobs.next(),if !jobs.is_empty()=>{
                if let Some((id,result))=completed {
                    cancellation=None;
                    let reply=match result {
                        Ok(Finished::Captured(c))=>{document=Some(c.document);current_path=Some(c.path);metadata=c.metadata;Ok(info(&mut settings_ui,&handle,&document,&current_path,&metadata,false))},
                        Ok(Finished::Opened(p,d))=>{document=Some(d);current_path=Some(p);metadata=Value::Null;Ok(info(&mut settings_ui,&handle,&document,&current_path,&metadata,false))},
                        Ok(Finished::Exported(p))=>{if let Some(d)=&mut document{d.mark_saved()}current_path=Some(p);Ok(info(&mut settings_ui,&handle,&document,&current_path,&metadata,false))},
                        Err(e)=>Err(e)
                    };respond(&handle,id,reply);
                }
            },
            delivery=deliveries.next()=>{
                let Some(delivery)=delivery else{return Err("Bus worker stopped".into())};
                settings_ui.drain_with(|| handle.settings_generation(), |_| {});
                settings_ui.reconcile(handle.settings_generation());
                let command=match delivery {
                    Delivery::Command(command)=>command,
                    Delivery::Refused{message}=>return Err(format!("registration refused: {message}")),
                    Delivery::Forwarded(_)=>continue,
                    Delivery::Connected|Delivery::Disconnected|Delivery::Changed|Delivery::Settings=>continue,
                };
                if !handle.is_current(&command.id){continue}
                let id=command.id;let verb=command.verb.as_str();
                let value=match verbs::parse(verb,&command.body){Ok(v)=>v,Err(e)=>{respond(&handle,id,Err(e));continue}};
                if matches!(verb,"cap.ping"|"cap.info"){respond(&handle,id,Ok(info(&mut settings_ui,&handle,&document,&current_path,&metadata,!jobs.is_empty())));continue}
                if verb=="app.describe"{respond(&handle,id,describe(&mut settings_ui,&handle));continue}
                if verb=="cap.cancel"{if let Some(tx)=&cancellation{let _=tx.send(true);}respond(&handle,id,Ok(json!({"cancelling":cancellation.is_some()})));continue}
                if !jobs.is_empty(){respond(&handle,id,Err("busy".into()));continue}
                if matches!(verb,"cap.open"|"cap.capture"|"cap.quit")&&document.as_ref().is_some_and(Document::dirty){respond(&handle,id,Err("export or undo unsaved annotations first".into()));continue}
                if verbs::is_edit(verb){let result=document.as_mut().ok_or_else(||"no image".into()).and_then(|d|verbs::edit(d,verb,value));respond(&handle,id,result);continue}
                match verbs::operation(verb,value) {
                    Ok(Operation::Capture(request))=>{
                        let(tx,rx)=tokio::sync::watch::channel(false);cancellation=Some(tx);
                        capture_generation=capture_generation.checked_add(1).expect("capture generations exhausted");
                        let future=capture::take(handle.clone(),comp.into(),request,None,directory.clone(),capture_generation,rx);
                        jobs.push(async move{(id,future.await.map(Finished::Captured).map_err(|e|e.message))}.boxed());
                    },
                    Ok(Operation::Open(path))=>match capture::absolute(&path){Ok(path)=>jobs.push(async move {
                        let result=tokio::task::spawn_blocking(move||Document::open(&path).map(|d|Finished::Opened(path,d))).await.map_err(|e|e.to_string()).and_then(|r|r);(id,result)
                    }.boxed()),Err(e)=>respond(&handle,id,Err(e))},
                    Ok(Operation::Export(path))=>{if let Some(doc)=document.clone(){match capture::absolute(&path){Ok(path)=>jobs.push(async move {
                        let result=tokio::task::spawn_blocking(move||doc.export(&path).map(|()|Finished::Exported(path))).await.map_err(|e|e.to_string()).and_then(|r|r);(id,result)
                    }.boxed()),Err(e)=>respond(&handle,id,Err(e))}}else{respond(&handle,id,Err("no image".into()))}},
                    Ok(Operation::Show)=>respond(&handle,id,Err("headless instance has no window".into())),
                    Ok(Operation::Quit)=>{respond(&handle,id,Ok(json!({"quitting":true})));handle.quit();handle.wait_done(std::time::Duration::from_secs(3));return Ok(())},
                    Err(e)=>respond(&handle,id,Err(e))
                }
            }
        }}
    })
}
fn respond(handle: &bus::BusHandle, id: bus::Request, result: Result<Value, String>) {
    match result {
        Ok(v) => handle.respond(id, 0, v.to_string()),
        Err(e) => handle.respond(id, 10, json!({"error":e}).to_string()),
    }
}
fn info(
    settings_ui: &mut Ui<()>,
    handle: &bus::BusHandle,
    document: &Option<Document>,
    path: &Option<PathBuf>,
    capture: &Value,
    busy: bool,
) -> Value {
    settings_ui.reconcile(handle.settings_generation());
    let mut info = json!({"schema":"cap.v1","headless":true,"busy":busy,"document":document.as_ref().map(Document::info),"path":path,"capture":capture,"version":env!("CARGO_PKG_VERSION"),"pid":std::process::id()});
    info["settings"] = json!(settings_ui.session().host().consumer().evidence());
    info["settings_cache"] = json!(settings_ui.session().cache_evidence());
    info
}
fn describe(settings_ui: &mut Ui<()>, handle: &bus::BusHandle) -> Result<Value, String> {
    settings_ui.reconcile(handle.settings_generation());
    let mut describe = json!({
        "schema":"cap.v1",
        "headless":true,
        "version":env!("CARGO_PKG_VERSION"),
        "transport":"native",
        "verbs":verbs::VERBS
    });
    application::describe::complete_native(&mut describe, application::describe::Identity {
        app_id: None, version: env!("CARGO_PKG_VERSION"),
        pid: std::process::id(), service: handle.service_name(),
    }, settings_ui.session()).map_err(|error| error.to_string())?;
    Ok(describe)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_headless_description_uses_actual_owner_and_null_window_identity() {
        let consumer = settings::consumer::Consumer::for_app(settings::Binding {
            instance: "fixture".into(), profile: "default".into(),
        }, "cap").unwrap();
        let (mut ui, _lane) = application::presentation::native::bridge(
            application::presentation::native::Session::new(consumer),
            application::presentation::native::Worker::offline(|_, _| Ok(())),
        );
        let (handle, _effects) = bus::BusHandle::response_sink();
        let value = describe(&mut ui, &handle).unwrap();
        assert_eq!(value["service"], "cap");
        assert_eq!(value["pid"], std::process::id());
        assert_eq!(value["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(value["headless"], true);
        assert!(value["app_id"].is_null());
        assert!(value["resources"].is_null());
        assert!(value["preparation"].is_object());
        assert_eq!(value["verbs"], json!(verbs::VERBS));
        let before = ui.session().preparation_evidence().desired;
        assert_eq!(describe(&mut ui, &handle).unwrap(), value);
        assert_eq!(ui.session().preparation_evidence().desired, before);
        assert!(ui.session().frame_stamp().is_none());
    }
}

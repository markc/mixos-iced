// SPDX-License-Identifier: MIT OR Apache-2.0
//! The same document and capture commands without a Wayland window.
use crate::{
    bus::{self, Delivery},
    capture,
    document::Document,
    verbs::{self, Operation},
};
use iced::futures::{FutureExt, StreamExt, future::BoxFuture, stream::FuturesUnordered};
use serde_json::{Value, json};
use std::path::PathBuf;
enum Finished {
    Captured(capture::Captured),
    Opened(PathBuf, Document),
    Exported(PathBuf),
}
type Job = BoxFuture<'static, (u64, Result<Finished, String>)>;
pub fn run(service: &str, url: &str, comp: &str, path: Option<PathBuf>) -> Result<(), String> {
    let directory = capture::media_directory()?;
    let (mut document, mut current_path) = match path {
        Some(p) => (Some(Document::open(&p)?), Some(p)),
        None => (None, None),
    };
    let (handle, mut deliveries) = bus::spawn(service, url).map_err(|e| e.to_string())?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(async {
        let mut jobs:FuturesUnordered<Job>=FuturesUnordered::new();let mut cancellation:Option<tokio::sync::watch::Sender<bool>>=None;let mut metadata=Value::Null;
        loop {tokio::select! {
            completed=jobs.next(),if !jobs.is_empty()=>{
                if let Some((id,result))=completed {
                    cancellation=None;
                    let reply=match result {
                        Ok(Finished::Captured(c))=>{document=Some(c.document);current_path=Some(c.path);metadata=c.metadata;Ok(info(&document,&current_path,&metadata,false))},
                        Ok(Finished::Opened(p,d))=>{document=Some(d);current_path=Some(p);metadata=Value::Null;Ok(info(&document,&current_path,&metadata,false))},
                        Ok(Finished::Exported(p))=>{if let Some(d)=&mut document{d.mark_saved()}current_path=Some(p);Ok(info(&document,&current_path,&metadata,false))},
                        Err(e)=>Err(e)
                    };respond(&handle,id,reply);
                }
            },
            delivery=deliveries.next()=>{
                let Some(delivery)=delivery else{return Err("Bus worker stopped".into())};
                let Delivery::Command(command)=delivery else {continue};
                let id=command.id;let verb=command.verb.as_str();
                let value=match verbs::parse(verb,&command.body){Ok(v)=>v,Err(e)=>{respond(&handle,id,Err(e));continue}};
                if matches!(verb,"cap.ping"|"cap.info"){respond(&handle,id,Ok(info(&document,&current_path,&metadata,!jobs.is_empty())));continue}
                if verb=="cap.cancel"{if let Some(tx)=&cancellation{let _=tx.send(true);}respond(&handle,id,Ok(json!({"cancelling":cancellation.is_some()})));continue}
                if !jobs.is_empty(){respond(&handle,id,Err("busy".into()));continue}
                if matches!(verb,"cap.open"|"cap.capture"|"cap.quit")&&document.as_ref().is_some_and(Document::dirty){respond(&handle,id,Err("export or undo unsaved annotations first".into()));continue}
                if verbs::is_edit(verb){let result=document.as_mut().ok_or_else(||"no image".into()).and_then(|d|verbs::edit(d,verb,value));respond(&handle,id,result);continue}
                match verbs::operation(verb,value) {
                    Ok(Operation::Capture(request))=>{
                        let(tx,rx)=tokio::sync::watch::channel(false);cancellation=Some(tx);
                        let future=capture::take(handle.clone(),comp.into(),request,None,directory.clone(),rx);
                        jobs.push(async move{(id,future.await.map(Finished::Captured))}.boxed());
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
fn respond(handle: &bus::BusHandle, id: u64, result: Result<Value, String>) {
    match result {
        Ok(v) => handle.respond(id, 0, v.to_string()),
        Err(e) => handle.respond(id, 10, json!({"error":e}).to_string()),
    }
}
fn info(document: &Option<Document>, path: &Option<PathBuf>, capture: &Value, busy: bool) -> Value {
    json!({"schema":"cap.v1","headless":true,"busy":busy,"document":document.as_ref().map(Document::info),"path":path,"capture":capture,"version":env!("CARGO_PKG_VERSION"),"pid":std::process::id()})
}

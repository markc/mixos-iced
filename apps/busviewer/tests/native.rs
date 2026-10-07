// SPDX-License-Identifier: MIT OR Apache-2.0
//! Run against an isolated real broker on a native-session capable kernel.
use application::iced::futures::StreamExt;
use bus::native_client::NodedClient;
use busviewer::bus::{self as viewer, Delivery};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

#[test]
#[ignore = "requires BUSVIEWER_TEST_URL and an isolated native broker"]
fn native_discovery_calls_topics_and_singleton_registration() {
    let url = std::env::var("BUSVIEWER_TEST_URL").expect("isolated broker URL");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let count=Arc::new(AtomicUsize::new(0));
        let mut clients=Vec::new();
        for name in ["example","legacy","broken"] {
            let client=Arc::new(NodedClient::connect(name,&url).await.unwrap());
            let mut incoming=client.incoming_async().await.unwrap();
            let worker=client.clone();let count=count.clone();
            tokio::spawn(async move {
                while let Some(command)=incoming.recv().await {
                    let (rc,body)=match (name,command.command.as_str()) {
                        ("example","HELP")=>(0,r#"[{"name":"echo","read_only":true},{"name":"refuse","read_only":true}]"#.to_owned()),
                        ("legacy","app.describe")=>(0,r#"{"verbs":["ping"]}"#.to_owned()),
                        ("example","echo")=>{count.fetch_add(1,Ordering::SeqCst);(0,command.body.clone())},
                        ("example","refuse")=>(10,"permission denied".into()),
                        _=>(10,"unknown verb".into()),
                    };
                    worker.respond(&command,rc,&body).await.unwrap();
                }
            });clients.push(client);
        }
        let (handle,mut events)=viewer::start("viewer-test",&url).unwrap();
        let changed=Arc::new(tokio::sync::Notify::new());
        let notification=changed.clone();let responder=handle.clone();
        tokio::spawn(async move {
            while let Some(event)=events.next().await {
                match event {
                    Delivery::Changed=>notification.notify_one(),
                    Delivery::Command{id,verb,..}=>responder.reply(id,0,if verb=="HELP"{busviewer::model::describe()["verbs"].clone()}else{busviewer::model::describe()}),
                    _=>{},
                }
            }
        });
        assert!(viewer::start("viewer-test",&url).is_err(),"duplicate identity must refuse registration");
        let snapshot=viewer::discover(handle.clone()).await;
        assert!(snapshot.error.is_none(),"{:?}",snapshot.error);
        assert!(snapshot.services.contains_key("noded"));
        assert_eq!(snapshot.services["example"].as_ref().unwrap().len(),2);
        assert_eq!(snapshot.services["legacy"].as_ref().unwrap()[0].read_only,None);
        let error=snapshot.services["broken"].as_ref().unwrap_err();
        assert!(error.contains("HELP:")&&error.contains("app.describe:"));
        assert!(snapshot.peer_error.is_none());
        let body=r#"{"value":42}"#.to_owned();
        let reply=handle.raw("example","echo",body.clone()).await.unwrap();
        assert_eq!(reply.rc,0);assert_eq!(reply.body,body);assert_eq!(count.load(Ordering::SeqCst),1);
        let reply=handle.raw("example","refuse",String::new()).await.unwrap();
        assert_eq!(reply.rc,10);assert_eq!(reply.body,"permission denied");
        let new_client=NodedClient::connect("new-citizen",&url).await.unwrap();
        let new_client=Arc::new(new_client);
        let mut incoming=new_client.incoming_async().await.unwrap();let responder=new_client.clone();
        tokio::spawn(async move{while let Some(command)=incoming.recv().await{responder.respond(&command,10,"description unavailable").await.unwrap();}});
        assert!(tokio::time::timeout(Duration::from_secs(5),changed.notified()).await.is_ok(),"real registration topic must wake the viewer");
        let refreshed=viewer::discover(handle.clone()).await;
        assert!(refreshed.services.contains_key("new-citizen"));
        new_client.close().await;
        handle.quit();handle.wait_done();
        for client in clients{client.close().await;}
    });
}

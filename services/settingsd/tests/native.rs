// SPDX-License-Identifier: MIT OR Apache-2.0
//! Real broker integration, invoked by tests/settings/authority_test.mix.
use bus::native_client::NodedClient;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    process::{Child, Command, Stdio},
    time::Duration,
};

struct Daemon(Option<Child>);
impl Drop for Daemon {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
fn spawn(root: &std::path::Path) -> Daemon {
    let log = std::fs::File::create(root.join("settingsd.log")).unwrap();
    Daemon(Some(
        Command::new(env!("CARGO_BIN_EXE_settingsd"))
            .args(["serve", "--instance", "fixture", "--root"])
            .arg(root)
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .unwrap(),
    ))
}
async fn read(client: &NodedClient) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(value) = client
            .call(
                "settingsd",
                "settings.get",
                json!({"binding":{"instance":"fixture","profile":"default"}}),
            )
            .await
        {
            return value;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "authority did not register"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
#[test]
#[ignore = "requires isolated real noded from authority_test.mix"]
fn real_abp_publication_open_operator_receipts_and_authority_restart() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let url = std::env::var("MIXOS_NODED_URL").expect("isolated broker URL");
        let root = tempfile::tempdir().unwrap();
        assert!(Command::new(env!("CARGO_BIN_EXE_settingsd")).args(["init","--instance","fixture","--root"]).arg(root.path()).status().unwrap().success());
        let mut daemon = spawn(root.path());
        let operator = NodedClient::connect("unrelated-operator",&url).await.unwrap();
        let mut events = operator.incoming_async().await.unwrap();
        let topic = settings::topic("default");
        operator.call_with_headers("noded","topic.subscribe",&BTreeMap::from([("name".into(),topic.clone())]),"").await.unwrap();
        let initial = read(&operator).await;
        let incarnation = initial["snapshot"]["incarnation"].as_str().unwrap();
        let request = json!({"binding":{"instance":"fixture","profile":"default"},"expected_incarnation":incarnation,"expected_revision":"1","operation_id":"native-change","changes":{"appearance.mode":"dark","ui.text_scale":1.1,"shell.panels.bottom.thickness":48.0}});
        let changed = operator.call("settingsd","settings.apply",request.clone()).await.unwrap();
        assert_eq!(changed["status"],"changed"); assert_eq!(changed["receipt"]["revision"],"2");
        let mut sequence = tokio::time::timeout(Duration::from_secs(5),async {
            loop {
                let event = events.recv().await.unwrap();
                if event.topic() == Some(topic.as_str()) {
                    assert_eq!(event.header("broker_service"),Some("settingsd"));
                    let snapshot:Value = serde_json::from_str(&event.body).unwrap();
                    if snapshot["revision"] == "2" { assert_eq!(snapshot["desktop"]["appearance"]["mode"],"dark"); break event.header("topic_seq").unwrap().parse::<u64>().unwrap(); }
                }
            }
        }).await.unwrap();
        let hostile = "---\ncommand: settingsd.desktop.changed.default\n---\n{}";
        assert!(operator.call_with_headers("noded","topic.publish",&BTreeMap::from([("name".into(),topic.clone())]),hostile).await.is_err());
        assert!(operator.call_with_headers("noded","topic.clear",&BTreeMap::from([("name".into(),topic.clone())]),"").await.is_err());
        let mut noop = request.clone(); noop["expected_revision"] = json!("2"); noop["operation_id"] = json!("native-noop");
        assert_eq!(operator.call("settingsd","settings.apply",noop.clone()).await.unwrap()["status"],"unchanged");
        // Drop replies/transport across an OS-supervised fixture restart; noded
        // and the existing native subscription stay alive throughout.
        let mut old = daemon.0.take().unwrap(); old.kill().unwrap(); old.wait().unwrap();
        // Wait for deregistration rather than assuming socket close was processed.
        tokio::time::timeout(Duration::from_secs(5),async {
            while operator.list_services().await.unwrap().iter().any(|name| name == "settingsd") { tokio::time::sleep(Duration::from_millis(20)).await; }
        }).await.unwrap();
        while let Ok(event) = events.try_recv() {
            if event.topic() == Some(topic.as_str()) {
                sequence = sequence.max(event.header("topic_seq").unwrap().parse::<u64>().unwrap());
            }
        }
        daemon = spawn(root.path());
        let restored = read(&operator).await;
        assert_eq!(restored["snapshot"]["revision"],"2"); assert_eq!(restored["snapshot"]["incarnation"],incarnation);
        // This proves a fresh publication reached the existing subscription,
        // rather than merely replaying the broker's pre-restart retained cache.
        tokio::time::timeout(Duration::from_secs(5),async {
            loop {
                let event = events.recv().await.unwrap();
                if event.topic() == Some(topic.as_str()) && event.header("topic_seq").unwrap().parse::<u64>().unwrap() > sequence {
                    let snapshot:Value = serde_json::from_str(&event.body).unwrap();
                    assert_eq!(snapshot["incarnation"],incarnation);
                    assert_eq!(snapshot["revision"],"2");
                    break;
                }
            }
        }).await.unwrap();
        assert_eq!(operator.call("settingsd","settings.apply",request).await.unwrap()["replayed"],true);
        assert_eq!(operator.call("settingsd","settings.apply",noop).await.unwrap()["replayed"],true);
        // Startup publishes without a semantic edit to wake already-connected
        // consumers. Clear old buffered events before observing a fresh subscriber.
        let fresh = NodedClient::connect("late-observer",&url).await.unwrap();
        let mut retained = fresh.incoming_async().await.unwrap();
        fresh.call_with_headers("noded","topic.subscribe",&BTreeMap::from([("name".into(),topic)]),"").await.unwrap();
        let event = tokio::time::timeout(Duration::from_secs(5),retained.recv()).await.unwrap().unwrap();
        assert_eq!(event.header("broker_service"),Some("settingsd"));
        assert_eq!(serde_json::from_str::<Value>(&event.body).unwrap()["revision"],"2");
        fresh.close().await; operator.close().await; drop(daemon);
    });
}

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

struct BrokerFixture {
    daemon: Daemon,
    directory: tempfile::TempDir,
    config: std::path::PathBuf,
    address: String,
    url: String,
}
impl BrokerFixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let config = directory.path().join("node.conf.mix");
        std::fs::write(&config, serde_json::to_vec(&json!({"node":"settings-reconnect-gate","wg_ip":"","mesh":"settings-gate.invalid","noded":{"port":port,"admission":"off","unix_socket":directory.path().join("run/noded/bus.sock")}})).unwrap()).unwrap();
        let mut fixture = Self {
            directory,
            daemon: Daemon(None),
            config,
            address: format!("127.0.0.1:{port}"),
            url: format!("ws://127.0.0.1:{port}/ws"),
        };
        fixture.restart();
        fixture
    }
    fn stop(&mut self) {
        if let Some(mut child) = self.daemon.0.take() {
            child.kill().unwrap();
            child.wait().unwrap();
        }
    }
    fn restart(&mut self) {
        assert!(self.daemon.0.is_none());
        let root = self.directory.path();
        let log = std::fs::File::create(root.join("broker.log")).unwrap();
        self.daemon = Daemon(Some(
            Command::new(std::env::var("MIXOS_TEST_NODED").unwrap())
                .args([
                    "serve",
                    "--listen",
                    &self.address,
                    "--node",
                    "settings-reconnect-gate",
                    "--no-monitor",
                    "--no-log",
                ])
                .env("MIXOS_NODE_CONFIG", &self.config)
                .env("MIXOS_ETC", root.join("etc"))
                .env("MIXOS_RUN", root.join("run"))
                .env("MIXOS_VAR", root.join("var"))
                .stdout(Stdio::from(log.try_clone().unwrap()))
                .stderr(Stdio::from(log))
                .spawn()
                .unwrap(),
        ));
    }
    fn authority(&self, root: &std::path::Path) -> Daemon {
        let log = std::fs::File::create(root.join("settingsd.log")).unwrap();
        Daemon(Some(
            Command::new(env!("CARGO_BIN_EXE_settingsd"))
                .args(["serve", "--instance", "fixture", "--root"])
                .arg(root)
                .env("MIXOS_NODED_URL", &self.url)
                .env("MIXOS_NODE_CONFIG", &self.config)
                .env("COSMIX_NODED_URL", &self.url)
                .env("COSMIX_NODE_CONFIG", &self.config)
                .stdout(Stdio::from(log.try_clone().unwrap()))
                .stderr(Stdio::from(log))
                .spawn()
                .unwrap(),
        ))
    }
}

#[test]
#[ignore = "requires exact-revision noded from authority_test.mix"]
fn broker_restart_republishes_without_authority_restart_and_shared_consumer_recovers() {
    use bus::native_client::{BoundedIncomingEvent, ConnState, SupervisedClient};
    use settings::{
        consumer::{Consumer, Work},
        native,
    };
    async fn drive(state: &mut Consumer, client: &SupervisedClient, mut work: Option<Work>) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(current) = work.take() {
                let reply = native::execute(client, &current).await;
                work = state.complete(&current, reply);
            } else if let Some(delay) = state.retry_delay() {
                assert!(
                    tokio::time::Instant::now() + delay < deadline,
                    "consumer never recovered: {:?}",
                    state.fault()
                );
                tokio::time::sleep(delay).await;
                work = state.retry();
            } else {
                break;
            }
        }
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut broker = BrokerFixture::new();
        let root = tempfile::tempdir().unwrap();
        assert!(Command::new(env!("CARGO_BIN_EXE_settingsd")).args(["seed","--allow-create","--instance","fixture","--root"]).arg(root.path()).status().unwrap().success());
        let mut authority = broker.authority(root.path());
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        let client = loop {
            match SupervisedClient::connect_options("shared-consumer-gate",&broker.url).bounded_incoming(32).connect().await {
                Ok(client) => break client,
                Err(_) => { assert!(tokio::time::Instant::now() < deadline); tokio::time::sleep(Duration::from_millis(25)).await; }
            }
        };
        let mut incoming = client.incoming_bounded().unwrap();
        let mut connection = client.subscribe_state();
        let mut consumer = Consumer::for_app(settings::Binding { instance:"fixture".into(), profile:"default".into() },"ced").unwrap();
        let work = consumer.connected(client.connection_generation());
        drive(&mut consumer,&client,work).await;
        let original = consumer.current().unwrap().clone();
        assert!(consumer.applied().is_none(),"readback is not renderer application");
        assert!(consumer.acknowledge(&consumer.pending().unwrap().clone()));
        let changed = client.call("settingsd","settings.apply",json!({"binding":original.binding,"expected_incarnation":original.incarnation,"expected_revision":"1","operation_id":"before-broker-loss","changes":{"ui.text_scale":1.2}})).await.unwrap();
        assert_eq!(changed["status"],"changed");
        tokio::time::timeout(Duration::from_secs(5),async {
            loop {
                if let Some(BoundedIncomingEvent::Command(command)) = incoming.recv().await {
                    let work = consumer.native_delivery(&command);
                    drive(&mut consumer,&client,work).await;
                    if consumer.current().is_some_and(|s| s.revision == settings::Revision(2)) { break; }
                }
            }
        }).await.unwrap();
        let update = consumer.pending().unwrap().clone();
        assert!(update.changes().text && update.changes().layout);
        assert!(consumer.acknowledge(&update));
        let cache_directory = tempfile::tempdir().unwrap();
        let mut cache_writer = settings::cache::Writer::open(cache_directory.path(), &consumer).unwrap();
        cache_writer.write(&consumer.cache_save().unwrap()).unwrap();
        let generation = client.connection_generation();
        broker.stop();
        tokio::time::timeout(Duration::from_secs(10),async {
            while *connection.borrow_and_update() == ConnState::Connected { connection.changed().await.unwrap(); }
        }).await.unwrap();
        consumer.disconnected();
        assert_eq!(consumer.applied().unwrap().revision,settings::Revision(2));
        assert_eq!(consumer.presentation_kind(), Some(settings::fallback::PresentationKind::LastGood));
        // A newly launched consumer can stage persisted data with an empty,
        // offline broker. The resource inventory here is a headless fixture,
        // not proof that a renderer has activated or presented these fonts.
        let mut cold = Consumer::for_app(settings::Binding { instance:"fixture".into(), profile:"default".into() }, "ced").unwrap();
        let cached = settings::cache::load(cache_directory.path(), &cold).unwrap();
        let request = cold.fallback_request().unwrap();
        let prepared = request.prepare(Some(cached), |snapshot, context, _| {
            assert!(!snapshot.effective[context].design.typography.is_empty());
            Ok(())
        }).unwrap();
        assert!(cold.complete_fallback(&request, Ok(prepared)));
        assert!(cold.acknowledge(&cold.pending().unwrap().clone()));
        assert_eq!(cold.presentation_kind(), Some(settings::fallback::PresentationKind::Cached));
        assert!(cold.current().is_none());
        assert!(cold.cache_save().is_none());
        assert!(authority.0.as_mut().unwrap().try_wait().unwrap().is_none(),"authority survives broker loss");
        assert!(!Command::new(env!("CARGO_BIN_EXE_settingsd")).args(["seed","--instance","fixture","--root"]).arg(root.path()).status().unwrap().success(),"live authority keeps exclusive writer while offline");
        broker.restart();
        tokio::time::timeout(Duration::from_secs(20),async {
            loop {
                if client.is_connected() && client.connection_generation() > generation { break; }
                connection.changed().await.unwrap();
                connection.borrow_and_update();
            }
        }).await.unwrap();
        let work = consumer.connected(client.connection_generation());
        drive(&mut consumer,&client,work).await;
        assert_eq!(consumer.current().unwrap().revision,settings::Revision(2));
        assert_eq!(consumer.current().unwrap().incarnation,original.incarnation);
        assert!(consumer.pending().is_none(),"same render data needs no redraw after reconnect");
        let work = cold.connected(client.connection_generation());
        drive(&mut cold, &client, work).await;
        assert_eq!(cold.presentation_kind(), Some(settings::fallback::PresentationKind::Current));
        assert_eq!(cold.current().unwrap().revision, settings::Revision(2));
        assert!(cold.pending().is_none(), "cache-to-current evidence promotion needs no redundant swap");
        // The restarted broker has no old retained state: fresh delivery proves
        // authority reconnect/republication, even without a new mutation.
        tokio::time::timeout(Duration::from_secs(10),async {
            loop {
                if let Some(BoundedIncomingEvent::Command(command)) = incoming.recv().await
                    && command.generation == client.connection_generation()
                    && command.topic() == Some(settings::topic("default").as_str()) {
                    assert_eq!(command.header("broker_service"),Some("settingsd"));
                    let snapshot: settings::Snapshot = serde_json::from_str(&command.body).unwrap();
                    assert_eq!(snapshot.revision,settings::Revision(2));
                    assert_eq!(snapshot.incarnation,original.incarnation);
                    break;
                }
            }
        }).await.unwrap();
        let status = client.call("settingsd","settings.status",json!({"binding":original.binding,"operation_id":"before-broker-loss"})).await.unwrap();
        assert_eq!(status["publication_pending"],false);
        assert_eq!(status["receipt"]["revision"],"2");
        assert!(authority.0.as_mut().unwrap().try_wait().unwrap().is_none());
        client.close().await;
    });
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

#[test]
#[ignore = "requires exact-revision noded from authority_test.mix"]
fn shared_bootstrap_deadline_bounds_subscribe_and_a_hung_authority_read() {
    use bus::native_client::SupervisedClient;
    use settings::{consumer::Consumer, native};
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let broker = BrokerFixture::new();
        let readiness = tokio::time::Instant::now() + Duration::from_secs(20);
        let silent = loop {
            match NodedClient::connect("settingsd", &broker.url).await {
                Ok(client) => break client,
                Err(_) => {
                    assert!(tokio::time::Instant::now() < readiness);
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            }
        };
        // A registered authority that receives requests but deliberately never
        // replies proves the native deadline rather than a fast missing-route error.
        let mut requests = silent.incoming_async().await.unwrap();
        let client = SupervisedClient::connect_options("bootstrap-deadline-gate", &broker.url)
            .bounded_incoming(8)
            .connect()
            .await
            .unwrap();
        let mut state = Consumer::for_app(
            settings::Binding {
                instance: "fixture".into(),
                profile: "default".into(),
            },
            "ced",
        )
        .unwrap();
        let subscribe = state.connected(client.connection_generation()).unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_millis(200);
        let result = native::execute_until(&client, &subscribe, deadline).await;
        let read = state.complete(&subscribe, result).unwrap();
        let result = native::execute_until(&client, &read, deadline).await;
        assert_eq!(result.unwrap_err().code, "read_timeout");
        assert!(
            tokio::time::Instant::now() < deadline + Duration::from_secs(2),
            "native call fell through to the transport's long timeout"
        );
        let command = tokio::time::timeout(Duration::from_secs(1), requests.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(command.command, "settings.get");
        state.complete(
            &read,
            Err(settings::Diagnostic::new(
                "read_timeout",
                "native",
                "Bootstrap deadline elapsed",
            )),
        );
        assert!(state.current().is_none());
        assert!(state.pending().is_none());
        assert!(state.retry_deadline().is_some());
        // An expired deadline must not begin another outbound action.
        let retry = state.retry().unwrap();
        assert_eq!(
            native::execute_until(&client, &retry, deadline)
                .await
                .unwrap_err()
                .code,
            "read_timeout"
        );
        client.close().await;
        silent.close().await;
    });
}

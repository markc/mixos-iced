// SPDX-License-Identifier: MIT OR Apache-2.0
//! Agent acceptance against a real noded; the fixture compositor only supplies
//! deterministic pixels. Hardware capture is tested separately on the desktop.
use bus::native_client::SupervisedClient;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::Arc,
    time::Duration,
};
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
async fn ready(url: &str) -> Arc<SupervisedClient> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match SupervisedClient::connect_options("cap-test-client", url)
                .connect()
                .await
            {
                Ok(c) => break Arc::new(c),
                Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
    })
    .await
    .expect("broker ready")
}
async fn info(client: &SupervisedClient, service: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(v) = client.call(service, "cap.info", json!({})).await {
                break v;
            }
            tokio::time::sleep(Duration::from_millis(50)).await
        }
    })
    .await
    .expect("Cap registered")
}
#[tokio::test]
#[ignore = "requires the worker-built noded binary; run after cargo build -p noded"]
async fn agent_capture_edit_export_cancel_and_single_instance() {
    let directory = tempfile::tempdir().unwrap();
    let broker = std::env::var_os("NODED_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/noded")
        });
    assert!(
        broker.is_file(),
        "build noded on this worker or set NODED_BIN"
    );
    let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = socket.local_addr().unwrap();
    drop(socket);
    let url = format!("ws://{address}/ws");
    let node_config = directory.path().join("node.conf.mix");
    std::fs::write(&node_config,json!({"node":"cap-test","wg_ip":"127.0.0.1","noded":{"port":address.port(),"unix_socket":directory.path().join("bus.sock")}}).to_string()).unwrap();
    let mut broker_process = Process(
        Command::new(broker)
            .args([
                "serve",
                "--listen",
                &address.to_string(),
                "--node",
                "cap-test",
                "--no-monitor",
                "--no-log",
            ])
            .env("MIXOS_VAR", directory.path().join("broker"))
            .env("MIXOS_NODE_CONFIG", &node_config)
            .env("HOME", directory.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let client = ready(&url).await;
    assert!(broker_process.0.try_wait().unwrap().is_none());
    let comp = Arc::new(
        SupervisedClient::connect_options("comp-cap-test", &url)
            .connect()
            .await
            .unwrap(),
    );
    let mut requests = comp.incoming().unwrap();
    let server = comp.clone();
    let fixture = tokio::spawn(async move {
        while let Some(request) = requests.recv().await {
            if request.command == "comp.capture.frame" {
                let args: Value = serde_json::from_str(&request.body).unwrap();
                let path = args["path"].as_str().unwrap();
                let image =
                    image::RgbaImage::from_pixel(120, 100, image::Rgba([230, 240, 250, 255]));
                image.save(path).unwrap();
                server.respond(&request,0,&json!({"path":path,"width":120,"height":100,"scale":1.0,"output":"test","source":"offscreen"}).to_string()).await.unwrap();
            } else {
                server
                    .respond(
                        &request,
                        10,
                        "{\"error\":\"fixture only handles frame capture\"}",
                    )
                    .await
                    .unwrap();
            }
        }
    });
    let mut cap = Process(
        Command::new(env!("CARGO_BIN_EXE_cap"))
            .args([
                "--headless",
                "--service",
                "cap-test",
                "--comp",
                "comp-cap-test",
                "--noded-url",
                &url,
            ])
            .env("MIXOS_APP_HOME", directory.path().join("cap"))
            .env("MIXOS_NODE_CONFIG", &node_config)
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("DBUS_SESSION_BUS_ADDRESS")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    assert_eq!(info(&client, "cap-test").await["headless"], true);
    let duplicate = Command::new(env!("CARGO_BIN_EXE_cap"))
        .args(["--headless", "--service", "cap-test", "--noded-url", &url])
        .env("MIXOS_APP_HOME", directory.path().join("duplicate"))
        .env("MIXOS_NODE_CONFIG", &node_config)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(!duplicate.success());
    let captured = client
        .call("cap-test", "cap.capture", json!({"cursor":false}))
        .await
        .unwrap();
    assert_eq!(captured["document"]["width"], 120);
    let source = PathBuf::from(captured["path"].as_str().unwrap());
    let original = std::fs::read(&source).unwrap();
    client.call("cap-test","cap.annotate",json!({"kind":"redact","points":[{"x":10,"y":10},{"x":50,"y":40}],"colour":[0,0,0,1],"width":2})).await.unwrap();
    assert!(
        client
            .call("cap-test", "cap.open", json!({"path":source}))
            .await
            .is_err()
    );
    client
        .call(
            "cap-test",
            "cap.crop",
            json!({"crop":{"x":10,"y":10,"width":80,"height":60}}),
        )
        .await
        .unwrap();
    let output = directory.path().join("export.png");
    client
        .call("cap-test", "cap.export", json!({"path":output}))
        .await
        .unwrap();
    let image = image::open(&output).unwrap().to_rgba8();
    assert_eq!(image.dimensions(), (80, 60));
    assert_eq!(image.get_pixel(5, 5).0, [0, 0, 0, 255]);
    assert_eq!(std::fs::read(&source).unwrap(), original);
    let exported = std::fs::read(&output).unwrap();
    assert!(
        client
            .call("cap-test", "cap.export", json!({"path":output}))
            .await
            .is_err()
    );
    assert_eq!(std::fs::read(&output).unwrap(), exported);
    let caller = client.clone();
    let pending = tokio::spawn(async move {
        caller
            .call("cap-test", "cap.capture", json!({"delay":10}))
            .await
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if info(&client, "cap-test").await["busy"] == true {
                break;
            }
            tokio::task::yield_now().await
        }
    })
    .await
    .unwrap();
    client
        .call("cap-test", "cap.cancel", json!({}))
        .await
        .unwrap();
    assert!(
        pending
            .await
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("cancelled")
    );
    assert_eq!(info(&client, "cap-test").await["document"]["width"], 80);
    client
        .call("cap-test", "cap.quit", json!({}))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if cap.0.try_wait().unwrap().is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await
        }
    })
    .await
    .unwrap();
    fixture.abort();
    comp.close().await;
    client.close().await;
}

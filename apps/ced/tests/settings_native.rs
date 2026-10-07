// SPDX-License-Identifier: MIT OR Apache-2.0
//! Real ABP delivery through the borrowed shared worker and Ced's appearance
//! builder. No window/compositor or native first-map timing is claimed.
use application::presentation::native::{Event, Session, Worker};
use bus::native_client::{BoundedIncomingEvent, BoundedIncomingReceiver, ConnState, SupervisedClient};
use settings::{Binding, Revision, consumer::Consumer, fallback::PresentationKind, native::Decoded};
use std::{process::{Child, Command, Stdio}, sync::Arc, time::Duration};

struct Authority(Child);
impl Drop for Authority {
    fn drop(&mut self) { let _ = self.0.kill(); let _ = self.0.wait(); }
}
async fn drive(session: &mut Session<ced::theme::Theme>, worker: &mut Worker<ced::theme::Theme>, client: &SupervisedClient,
    incoming: &mut BoundedIncomingReceiver, state: &mut tokio::sync::watch::Receiver<ConnState>, revision: u64) {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let event = tokio::select! {
                event = worker.next() => event.take().unwrap(),
                incoming = incoming.recv() => match incoming.expect("native receiver") {
                    BoundedIncomingEvent::Command(command) => {
                        let Some(decoded) = Decoded::from_command(session.host().consumer().binding(), &command) else { continue; };
                        Event::Delivery(decoded)
                    }
                    BoundedIncomingEvent::Overflow { .. } => Event::Lost,
                },
                edge = state.changed() => { edge.unwrap(); state.borrow_and_update(); Event::Wake },
            };
            let live = settings::native::live_generation(client);
            let (_, jobs) = session.handle(event, live);
            worker.replace(jobs);
            if session.host().kind() == Some(PresentationKind::Current)
                && session.host().consumer().applied().is_some_and(|s| s.revision == Revision(revision)) { break; }
        }
    }).await.expect("native presentation did not converge");
}
#[tokio::test]
#[ignore = "requires isolated exact-revision noded/settingsd from authority_test.mix"]
async fn real_settings_bus_prepares_and_activates_ced_and_fences_loss() {
    toolkit::fonts::install(toolkit::fonts::FontSet::new().sans(include_bytes!("../../../vendor/font/Inter-VariableFont_opsz,wght.ttf").as_slice()), None).unwrap();
    let root = tempfile::tempdir().unwrap();
    let binary = std::env::var("MIXOS_TEST_SETTINGSD").unwrap();
    assert!(Command::new(&binary).args(["seed", "--allow-create", "--instance", "fixture", "--root"]).arg(root.path()).status().unwrap().success());
    let log = std::fs::File::create(root.path().join("settingsd.log")).unwrap();
    let mut authority = Authority(Command::new(&binary).args(["serve", "--instance", "fixture", "--root"]).arg(root.path())
        .stdout(Stdio::from(log.try_clone().unwrap())).stderr(Stdio::from(log)).spawn().unwrap());
    let url = std::env::var("MIXOS_NODED_URL").unwrap();
    let client = Arc::new(SupervisedClient::connect_options("ced-presentation-gate", &url).bounded_incoming(64).connect().await.unwrap());
    let mut incoming = client.incoming_bounded().unwrap();
    let mut state = client.subscribe_state();
    let mut session = Session::new(Consumer::for_app(Binding { instance: "fixture".into(), profile: "default".into() }, "ced").unwrap());
    let mut worker = Worker::new(Arc::clone(&client), ced::theme::from_settings);
    let (_, jobs) = session.handle(Event::Wake, Some(client.connection_generation()));
    worker.replace(jobs);
    drive(&mut session, &mut worker, &client, &mut incoming, &mut state, 1).await;
    let old = session.host().presentation().unwrap().content().clone();
    let current = session.host().consumer().current().unwrap();
    let changed = client.call("settingsd", "settings.apply", serde_json::json!({"binding":current.binding,"expected_incarnation":current.incarnation,
        "expected_revision":"1","operation_id":"ced-live-fixture","changes":{"appearance.mode":"dark","ui.text_scale":1.5}})).await.unwrap();
    assert_eq!(changed["status"], "changed");
    drive(&mut session, &mut worker, &client, &mut incoming, &mut state, 2).await;
    let live = session.host().presentation().unwrap().content();
    assert_ne!(live.palette.background, old.palette.background);
    assert_eq!(live.ui.1, old.ui.1 * 1.5);
    client.close().await;
    let (change, jobs) = session.handle(Event::Wake, None);
    worker.replace(jobs);
    assert!(change.is_none());
    assert_eq!(session.host().kind(), Some(PresentationKind::LastGood));
    assert_eq!(session.host().consumer().applied().unwrap().revision, Revision(2));
    assert!(authority.0.try_wait().unwrap().is_none());
}

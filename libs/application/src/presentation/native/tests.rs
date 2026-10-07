// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use appearance::resources::{IconRequirement, ResourceHost, ResourceRequirements};
use settings::{Binding, Desktop, Revision};
use sha2::Digest;
use toolkit::fonts::{FontChoice, FontSelection, FontSet};
mod bridge;
#[cfg(feature = "settings-cache")]
mod cache;

fn install_fonts() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        toolkit::fonts::install(
            FontSet::new().sans(
                include_bytes!("../../../../../vendor/font/Inter-VariableFont_opsz,wght.ttf")
                    .as_slice(),
            ),
            None,
        )
        .unwrap();
    });
}

/// A resource host with no approved roots: deterministic in every test
/// environment, exercising the honest no-set rescue path.
fn hermetic_host() -> ResourceHost {
    ResourceHost::new(assets::Lookup::new())
}

/// Write one locked-file entry and its bytes, as the installer does.
fn write_file(dir: &std::path::Path, relative: &str, bytes: &[u8]) -> serde_json::Value {
    let path = dir.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, bytes).unwrap();
    serde_json::json!({
        "path": relative, "bytes": bytes.len(),
        "url": "https://example.org/font", "upstream": "https://example.org/",
        "revision": "pinned", "licence": "OFL-1.1",
        "sha256": hex::encode(sha2::Sha256::digest(bytes)),
        "blake3": blake3::hash(bytes).to_hex().to_string(),
    })
}

fn write_manifest(dir: &std::path::Path, json: &serde_json::Value) {
    let text = strict::encode_pretty(&strict::from_json(json)).unwrap();
    std::fs::write(dir.join(assets::MANIFEST_FILE), text).unwrap();
}

/// A real verified set under `root`, activated through `current`, using the
/// real variable Inter and static Noto Sans fonts so the toolkit registry
/// parses actual bytes and every embedded design record resolves (Inter
/// doubles as the sans/display/mono packaged roles; Noto Sans covers the
/// button records).
fn publish_set(root: &std::path::Path, id: &str) {
    let dir = root.join("sets").join(id);
    let inter = include_bytes!("../../../../../vendor/font/Inter-VariableFont_opsz,wght.ttf");
    let noto = include_bytes!("../../../../../vendor/cosmic-text/fonts/NotoSans-Regular.ttf");
    let entries = vec![
        write_file(&dir, "fonts/Sans.ttf", inter),
        write_file(&dir, "fonts/Noto.ttf", noto),
    ];
    write_manifest(
        &dir,
        &serde_json::json!({
            "schema": assets::SCHEMA, "set_id": id,
            "fonts": {
                "sans": "fonts/Sans.ttf", "display": "fonts/Sans.ttf",
                "mono": "fonts/Sans.ttf", "extra": "fonts/Noto.ttf"
            },
            "font_families": {
                "sans": "Inter", "display": "Inter", "mono": "Inter", "extra": "Noto Sans"
            },
            "files": entries, "web_css": "/* fixture */\n"
        }),
    );
    std::fs::write(dir.join(assets::STYLESHEET_FILE), "/* fixture */\n").unwrap();
    std::os::unix::fs::symlink(
        std::path::Path::new("sets").join(id),
        root.join(assets::CURRENT_LINK),
    )
    .unwrap();
}

/// The same fonts plus one v2 icon catalogue whose declared family is the
/// true intrinsic Inter family: `home` maps to a real glyph ('a') and `emoji`
/// maps to U+1F600, absent from Inter's cmap, so a late icon requirement can
/// fail the atomic batch after the fonts preflight.
fn publish_icon_set(root: &std::path::Path, id: &str) {
    let dir = root.join("sets").join(id);
    let inter = include_bytes!("../../../../../vendor/font/Inter-VariableFont_opsz,wght.ttf");
    let noto = include_bytes!("../../../../../vendor/cosmic-text/fonts/NotoSans-Regular.ttf");
    let entries = vec![
        write_file(&dir, "fonts/Sans.ttf", inter),
        write_file(&dir, "fonts/Noto.ttf", noto),
        write_file(&dir, "icons/Symbols.codepoints", b"home 61\nemoji 1F600\n"),
    ];
    write_manifest(
        &dir,
        &serde_json::json!({
            "schema": assets::SCHEMA_V2, "set_id": id,
            "fonts": {
                "sans": "fonts/Sans.ttf", "display": "fonts/Sans.ttf",
                "mono": "fonts/Sans.ttf", "extra": "fonts/Noto.ttf"
            },
            "font_families": {
                "sans": "Inter", "display": "Inter", "mono": "Inter", "extra": "Noto Sans"
            },
            "files": entries, "web_css": "/* fixture */\n",
            "icon_default": { "family": "Inter", "style": "default", "weight": 400 },
            "icon_catalogues": [{
                "family": "Inter", "style": "default", "font": "fonts/Sans.ttf",
                "face_index": 0, "codepoints": "icons/Symbols.codepoints"
            }],
            "icon_assets": []
        }),
    );
    std::fs::write(dir.join(assets::STYLESHEET_FILE), "/* fixture */\n").unwrap();
    std::os::unix::fs::symlink(
        std::path::Path::new("sets").join(id),
        root.join(assets::CURRENT_LINK),
    )
    .unwrap();
}

fn binding() -> Binding {
    Binding {
        instance: "fixture".into(),
        profile: "default".into(),
    }
}
fn snapshot(revision: u64, dark: bool) -> Snapshot {
    let mut desktop = Desktop::default();
    if dark {
        desktop.appearance.mode = "dark".into();
    }
    Snapshot {
        schema: 1,
        binding: binding(),
        incarnation: "fixture".into(),
        revision: Revision(revision),
        design_revision: Revision(revision),
        source_digest: settings::source_digest(settings::EMBEDDED_DEFAULT_SOURCE),
        effective: settings::resolve(&desktop).unwrap(),
        desktop,
    }
}
fn session() -> Session<u64> {
    let mut consumer = Consumer::for_app(binding(), "ced").unwrap();
    let subscribe = consumer.connected(1).unwrap();
    let read = consumer.complete(&subscribe, Ok(None)).unwrap();
    consumer.complete(&read, Ok(Some(snapshot(1, false))));
    Session::new(consumer)
}
fn ready(request: Request) -> Completion<u64> {
    let rev = request.update().snapshot().revision.0;
    request.prepare(
        |_, _| {
            Ok(FontSelection {
                font: crate::iced::Font::DEFAULT,
                choice: FontChoice::Declared,
            })
        },
        |_| Ok(rev),
    )
}
fn decoded(rev: u64) -> Decoded {
    decoded_snapshot(snapshot(rev, false))
}
fn decoded_snapshot(snapshot: Snapshot) -> Decoded {
    let command = bus::native_client::IncomingCommand {
        generation: 1,
        from: "settingsd".into(),
        command: String::new(),
        id: None,
        args: serde_json::Value::Null,
        body: serde_json::to_string(&snapshot).unwrap(),
        headers: std::collections::BTreeMap::from([
            ("topic".into(), settings::topic("default")),
            ("broker_service".into(), "settingsd".into()),
        ]),
    };
    Decoded::from_command(&binding(), &command).unwrap()
}
#[test]
fn exact_replay_does_not_mark_a_gap_but_same_revision_contradiction_does() {
    let mailbox = Mailbox::<u64>::default();
    mailbox.publish(Event::Delivery(decoded(1)));
    for _ in 0..100 {
        assert!(!mailbox.publish(Event::Delivery(decoded(1))));
    }
    assert_eq!(mailbox.take().len(), 1);
    mailbox.publish(Event::Delivery(decoded(1)));
    mailbox.publish(Event::Delivery(decoded_snapshot(snapshot(1, true))));
    let events = mailbox.take();
    assert!(matches!(events[0], Event::Lost));
}
#[test]
fn foreign_snapshot_binding_cannot_activate_despite_matching_broker_stamp() {
    let mut session = session();
    let mut foreign = snapshot(2, true);
    foreign.binding.instance = "other".into();
    let (change, jobs) = session.handle(Event::Delivery(decoded_snapshot(foreign)), Some(1));
    assert!(change.is_none());
    assert!(jobs.prepare.is_none());
    assert_eq!(
        session.host().consumer().fault().unwrap().code,
        "wrong_target"
    );
    assert!(session.host().presentation().is_none());
}

#[test]
fn session_activation_hook_fences_stale_and_failed_preparations() {
    let mut session = session();
    let (_, jobs) = session.handle(Event::Wake, Some(1));
    let stale = ready(jobs.prepare.unwrap());
    session.host.consumer_mut().observe(1, snapshot(2, true));
    let (_, jobs) = session.handle(Event::Wake, Some(1));
    let current = ready(jobs.prepare.unwrap());
    let mut activated = Vec::new();
    assert!(
        session
            .handle_with(Event::Prepared(stale), Some(1), |p| activated
                .push(*p.content()))
            .0
            .is_none()
    );
    assert!(activated.is_empty());
    assert!(
        session
            .handle_with(Event::Prepared(current), Some(1), |p| activated
                .push(*p.content()))
            .0
            .is_some()
    );
    assert_eq!(activated, [2]);
    session.host.consumer_mut().observe(1, snapshot(3, false));
    let (_, jobs) = session.handle(Event::Wake, Some(1));
    let failed = jobs
        .prepare
        .unwrap()
        .failed(Diagnostic::new("fixture", "policy", "invalid"));
    assert!(
        session
            .handle_with(Event::Prepared(failed), Some(1), |p| activated
                .push(*p.content()))
            .0
            .is_none()
    );
    assert_eq!(activated, [2]);
    assert_eq!(
        session.host.consumer().applied().unwrap().revision,
        Revision(2)
    );
}

#[test]
fn current_embedded_fallback_runs_activation_hook_once() {
    let mut session = Session::<u64>::new(Consumer::for_app(binding(), "ced").unwrap());
    session.bootstrap = Instant::now();
    let (_, jobs) = session.handle(Event::Wake, None);
    let request = jobs.fallback.unwrap();
    let mut presentation = None;
    let fallback = request
        .prepare(None, |snapshot, context, _| {
            let appearance = Projection::new(&snapshot.effective[context])?.prepare(|_, _| {
                Ok(FontSelection {
                    font: crate::iced::Font::DEFAULT,
                    choice: FontChoice::Declared,
                })
            })?;
            presentation = Some(Presentation {
                appearance,
                content: 77,
            });
            Ok(())
        })
        .unwrap();
    let mut activated = Vec::new();
    let (change, _) = session.handle_with(
        Event::Fallback(request, Box::new(Ok((fallback, presentation.unwrap())))),
        None,
        |p| activated.push(*p.content()),
    );
    assert!(change.is_some());
    assert_eq!(activated, [77]);
    assert_eq!(
        session.host().kind(),
        Some(settings::fallback::PresentationKind::Embedded)
    );
    let (change, _) = session.handle_with(Event::Wake, None, |p| activated.push(*p.content()));
    assert!(change.is_none());
    assert_eq!(activated, [77]);
}
#[test]
fn live_stage_fences_a_ready_fallback_on_the_same_connection() {
    let mut session = Session::<u64>::new(Consumer::for_app(binding(), "ced").unwrap());
    session.bootstrap = Instant::now();
    let (_, jobs) = session.handle(Event::Wake, Some(1));
    let request = jobs.fallback.unwrap();
    let mut presentation = None;
    let fallback = request
        .prepare(None, |snapshot, context, _| {
            let appearance = Projection::new(&snapshot.effective[context])?.prepare(|_, _| {
                Ok(FontSelection {
                    font: crate::iced::Font::DEFAULT,
                    choice: FontChoice::Declared,
                })
            })?;
            presentation = Some(Presentation {
                appearance,
                content: 77,
            });
            Ok(())
        })
        .unwrap();
    let subscribe = session.host.consumer().current_work().unwrap().clone();
    let (_, jobs) = session.handle(Event::Rpc(subscribe, Ok(None)), Some(1));
    let (_, jobs) = session.handle(
        Event::Rpc(jobs.work.unwrap(), Ok(Some(snapshot(1, false)))),
        Some(1),
    );
    let ready = ready(jobs.prepare.unwrap());
    let mut activated = Vec::new();
    let (change, jobs) = session.handle_with(
        Event::Fallback(request, Box::new(Ok((fallback, presentation.unwrap())))),
        Some(1),
        |p| activated.push(*p.content()),
    );
    assert!(change.is_none());
    assert!(activated.is_empty());
    assert!(jobs.prepare.is_some());
    let (change, _) = session.handle(Event::Prepared(ready), Some(1));
    assert!(change.is_some());
    assert_eq!(*session.host().presentation().unwrap().content(), 1);
}
#[test]
fn mailbox_bounds_a_delivery_storm_and_marks_the_gap_before_latest() {
    let mailbox = Mailbox::<u64>::default();
    assert!(mailbox.publish(Event::Delivery(decoded(1))));
    for rev in 2..100 {
        assert!(!mailbox.publish(Event::Delivery(decoded(rev))));
    }
    assert!(!mailbox.publish(Event::Wake));
    let events = mailbox.clone().take();
    assert_eq!(events.len(), 3);
    assert!(matches!(events[0], Event::Lost));
    assert!(matches!(events[1], Event::Delivery(_)));
    assert!(mailbox.take().is_empty());
    assert!(mailbox.publish(Event::Wake));
}
#[test]
fn decoded_event_queued_before_loss_cannot_reenter_a_new_generation() {
    let mut session = session();
    let delayed = decoded(99);
    session.handle(Event::Wake, None);
    session.handle(Event::Delivery(delayed), Some(2));
    assert!(session.host().consumer().pending().is_none());
    assert_eq!(session.host().consumer().generation(), Some(2));
}
#[test]
fn coalesced_jobs_retain_capture_and_atomic_loss_rejects_queued_ready() {
    let mut session = session();
    let (_, first) = session.handle(Event::Wake, Some(1));
    let (_, repeated) = session.handle(Event::Wake, Some(1));
    assert!(
        first
            .prepare
            .unwrap()
            .update()
            .same_stage(repeated.prepare.as_ref().unwrap().update())
    );
    let result = ready(repeated.prepare.unwrap());
    let (change, after_loss) = session.handle(Event::Prepared(result), None);
    assert!(change.is_none());
    assert!(session.host().presentation().is_none());
    assert!(after_loss.prepare.is_none());
}
#[test]
fn no_op_revision_advances_evidence_without_a_resource_job() {
    let mut session = session();
    let (_, jobs) = session.handle(Event::Wake, Some(1));
    let (change, _) = session.handle(Event::Prepared(ready(jobs.prepare.unwrap())), Some(1));
    assert!(change.is_some());
    session.host.consumer_mut().observe(1, snapshot(2, false));
    let (change, jobs) = session.handle(Event::Wake, Some(1));
    assert!(change.is_none());
    assert!(jobs.prepare.is_none());
    assert_eq!(
        session.host().consumer().applied().unwrap().revision,
        Revision(2)
    );
    assert_eq!(*session.host().presentation().unwrap().content(), 1);
}
#[test]
fn fallback_attempt_is_fenced_and_does_not_retry_a_failed_resource_in_a_loop() {
    let mut session = Session::<u64>::new(Consumer::for_app(binding(), "ced").unwrap());
    session.bootstrap = Instant::now();
    let (_, jobs) = session.handle(Event::Wake, None);
    let request = jobs.fallback.unwrap();
    let mut activated = false;
    session.handle_with(
        Event::Fallback(
            request,
            Box::new(Err(vec![Diagnostic::new(
                "font_unavailable",
                "fixture",
                "missing",
            )])),
        ),
        None,
        |_| activated = true,
    );
    assert!(!activated);
    let (_, jobs) = session.handle(Event::Wake, None);
    assert!(jobs.fallback.is_none());
    assert!(jobs.wake.is_none());
    assert!(session.host().consumer().fallback_fault().is_some());
    for _ in 0..100 {
        let (_, jobs) = session.handle(Event::Wake, None);
        assert!(jobs.fallback.is_none());
        assert!(jobs.wake.is_none());
    }
    let (_, jobs) = session.handle(Event::Refresh, None);
    assert!(
        jobs.fallback.is_some(),
        "explicit refresh can heal offline resource failure"
    );
}
#[tokio::test]
async fn superseded_blocking_jobs_are_physically_serial_and_keep_only_latest() {
    install_fonts();
    let (entered_tx, mut entered_rx) = tokio::sync::mpsc::unbounded_channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = std::sync::Mutex::new(release_rx);
    let mut worker = Worker::offline_with_host(
        move |_, snapshot| {
            entered_tx.send(snapshot.revision.0).unwrap();
            release_rx.lock().unwrap().recv().unwrap();
            Ok(snapshot.revision.0)
        },
        hermetic_host(),
    );
    let mut session = session();
    let (_, jobs) = session.handle(Event::Wake, Some(1));
    worker.replace(jobs);
    tokio::select! {
        event = worker.next() => panic!("completed before release: {}", event.take().is_some()),
        started = entered_rx.recv() => assert_eq!(started, Some(1)),
        _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => panic!("resource did not start"),
    }
    for (rev, dark) in [(2, true), (3, false)] {
        session.host.consumer_mut().observe(1, snapshot(rev, dark));
        let (_, jobs) = session.handle(Event::Wake, Some(1));
        worker.replace(jobs.clone());
        worker.replace(jobs);
    }
    assert!(entered_rx.try_recv().is_err());
    release_tx.send(()).unwrap();
    let old = tokio::time::timeout(std::time::Duration::from_secs(5), worker.next())
        .await
        .unwrap()
        .take()
        .unwrap();
    let (change, jobs) = session.handle(old, Some(1));
    assert!(change.is_none());
    worker.replace(jobs);
    tokio::select! {
        _ = worker.next() => panic!("latest completed before release"),
        started = entered_rx.recv() => assert_eq!(started, Some(3)),
        _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => panic!("latest did not start"),
    }
    release_tx.send(()).unwrap();
    let fresh = tokio::time::timeout(std::time::Duration::from_secs(5), worker.next())
        .await
        .unwrap()
        .take()
        .unwrap();
    let (change, jobs) = session.handle(fresh, Some(1));
    assert!(change.is_some());
    assert_eq!(*session.host().presentation().unwrap().content(), 3);
    worker.replace(jobs);
    assert!(worker.running.is_none() && worker.queued.is_none());
    assert!(entered_rx.try_recv().is_err());
}

/// The central worker path: a real verified set is read and registered, and
/// the exact binding it produced is what the activation acknowledges, so the
/// captured cache save records precisely what was prepared.
#[cfg(feature = "settings-cache")]
#[tokio::test]
async fn verified_omission_binding_is_acknowledged_and_captured_by_the_save() {
    let directory = tempfile::tempdir().unwrap();
    publish_set(directory.path(), "binding-set");
    let mut session = session();
    let (_, jobs) = session.handle(Event::Wake, Some(1));
    let host = ResourceHost::new(
        std::iter::once(directory.path().to_path_buf()).collect::<assets::Lookup>(),
    );
    let mut worker = Worker::offline_with_cache_and_host(
        directory.path().join("cache"),
        |_, snapshot| Ok(snapshot.revision.0),
        host,
    );
    worker.replace(jobs);
    let event = tokio::time::timeout(std::time::Duration::from_secs(10), worker.next())
        .await
        .unwrap()
        .take()
        .unwrap();
    let change = session.handle(event, Some(1)).0;
    assert!(change.is_some());
    let resources = session
        .host()
        .presentation()
        .unwrap()
        .appearance()
        .resources()
        .unwrap();
    assert_eq!(resources.binding().unwrap().set_id, "binding-set");
    let save = session.host().consumer().cache_save().unwrap();
    assert_eq!(save.binding().map(|binding| binding.set_id.as_str()), Some("binding-set"));
    assert_eq!(
        save.binding().map(|binding| binding.manifest_blake3.as_str()),
        resources.binding().map(|binding| binding.manifest_blake3.as_str()),
        "the save captures exactly the activated binding"
    );
}

/// A late icon failure after the fonts preflight is all-or-nothing: the
/// failing batch registers no fonts and no aliases, so applied state, the
/// activation ACK and the captured cache save all stay exactly where the last
/// successful preparation left them.
#[cfg(feature = "settings-cache")]
#[tokio::test]
async fn late_icon_failure_keeps_applied_ack_and_cache_capture_unchanged() {
    let directory = tempfile::tempdir().unwrap();
    publish_icon_set(directory.path(), "icons");
    let mut session = session();
    let (_, jobs) = session.handle(Event::Wake, Some(1));
    let host = ResourceHost::new(
        std::iter::once(directory.path().to_path_buf()).collect::<assets::Lookup>(),
    );
    let tint = crate::iced::Color::from_rgba8(255, 255, 255, 1.0);
    let worker = Worker::offline_with_cache_and_host(
        directory.path().join("cache"),
        |_, snapshot| Ok(snapshot.revision.0),
        host,
    )
    .with_resource_requirements(move |_, snapshot| {
        if snapshot.revision == Revision(1) {
            return Ok(ResourceRequirements::empty());
        }
        ResourceRequirements::new(vec![IconRequirement {
            key: "emoji".into(),
            name: "emoji".into(),
            logical_size: 16.0,
            scale: 1.0,
            tint,
        }])
    });
    let mut worker = worker;
    worker.replace(jobs);
    let event = tokio::time::timeout(std::time::Duration::from_secs(10), worker.next())
        .await
        .unwrap()
        .take()
        .unwrap();
    assert!(session.handle(event, Some(1)).0.is_some());
    assert_eq!(
        session.host().consumer().applied().unwrap().revision,
        Revision(1)
    );
    let saved = session.host().consumer().cache_save().unwrap();
    // A newer snapshot whose required icon is declared by the catalogue but
    // absent from the face's cmap fails the atomic batch: no activation, no
    // ACK, no new cache capture.
    session.host.consumer_mut().observe(1, snapshot(2, false));
    let (change, jobs) = session.handle(Event::Wake, Some(1));
    assert!(change.is_none());
    worker.replace(jobs);
    let event = tokio::time::timeout(std::time::Duration::from_secs(10), worker.next())
        .await
        .unwrap()
        .take()
        .unwrap();
    let (change, _) = session.handle(event, Some(1));
    assert!(change.is_none());
    assert_eq!(
        session.host().consumer().applied().unwrap().revision,
        Revision(1),
        "a failed late icon cannot replace LastGood"
    );
    assert!(
        session.host().consumer().fault().is_some(),
        "the failure is reported"
    );
    let after = session.host().consumer().cache_save().unwrap();
    assert!(
        after.same_capture(&saved),
        "no new capture: a failed preparation cannot ACK"
    );
}

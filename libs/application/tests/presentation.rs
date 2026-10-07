// SPDX-License-Identifier: MIT OR Apache-2.0
#![cfg(feature = "settings")]
use application::presentation::{Host, Request};
use settings::{Binding, Desktop, Diagnostic, Revision, Snapshot, consumer::Consumer};
use toolkit::fonts::{FontChoice, FontSelection};

fn snapshot(rev: u64, dark: bool) -> Snapshot {
    let mut desktop = Desktop::default();
    if dark {
        desktop.appearance.mode = "dark".into();
    }
    let source_digest = settings::source_digest(settings::EMBEDDED_DEFAULT_SOURCE);
    Snapshot {
        schema: 1,
        binding: binding(),
        incarnation: "fixture".into(),
        revision: Revision(rev),
        design_revision: Revision(rev),
        source_digest,
        effective: settings::resolve(&desktop).unwrap(),
        desktop,
    }
}
fn binding() -> Binding {
    Binding {
        instance: "fixture".into(),
        profile: "default".into(),
    }
}
fn host() -> Host<String> {
    let mut consumer = Consumer::for_app(binding(), "ced").unwrap();
    let subscribe = consumer.connected(1).unwrap();
    let read = consumer.complete(&subscribe, Ok(None)).unwrap();
    consumer.complete(&read, Ok(Some(snapshot(1, false))));
    Host::new(consumer)
}
fn prepare(request: Request, content: &str) -> application::presentation::Completion<String> {
    request.prepare(
        |_, _| {
            Ok(FontSelection {
                font: application::iced::Font::DEFAULT,
                choice: FontChoice::Declared,
            })
        },
        |_| Ok(content.to_owned()),
    )
}
#[test]
fn whole_presentation_and_content_activate_then_acknowledge() {
    let mut host = host();
    assert!(host.consumer().applied().is_none());
    let completion = prepare(host.request().unwrap(), "document styling");
    let plan = host.complete(completion).unwrap();
    assert!(plan.paint && plan.text && plan.layout);
    assert_eq!(host.presentation().unwrap().content(), "document styling");
    assert_eq!(host.consumer().applied().unwrap().revision, Revision(1));
    assert!(host.request().is_none());
}

#[test]
fn activation_hook_receives_only_current_successful_resources() {
    let mut host = host();
    let stale = prepare(host.request().unwrap(), "stale");
    host.consumer_mut().observe(1, snapshot(2, true));
    let current = prepare(host.request().unwrap(), "current");
    let mut activated = Vec::new();
    assert!(host.complete_with(stale, |p| activated.push(p.content().clone())).is_none());
    assert!(activated.is_empty());
    assert!(host.complete_with(current, |p| activated.push(p.content().clone())).is_some());
    assert_eq!(activated, ["current"]);
    assert_eq!(host.consumer().applied().unwrap().revision, Revision(2));

    host.consumer_mut().observe(1, snapshot(3, false));
    let failed = host.request().unwrap().failed(Diagnostic::new("test", "panel", "invalid"));
    assert!(host.complete_with(failed, |p| activated.push(p.content().clone())).is_none());
    assert_eq!(activated, ["current"]);
    assert_eq!(host.consumer().applied().unwrap().revision, Revision(2));

    let work = host.consumer_mut().refresh().unwrap();
    host.consumer_mut().complete(&work, Ok(Some(snapshot(3, false))));
    let lost = prepare(host.request().unwrap(), "lost");
    host.consumer_mut().disconnected();
    assert!(host.complete_with(lost, |p| activated.push(p.content().clone())).is_none());
    assert_eq!(activated, ["current"]);
}
#[test]
fn superseded_worker_completion_cannot_swap_or_acknowledge() {
    let mut host = host();
    let stale = prepare(host.request().unwrap(), "stale");
    host.consumer_mut().observe(1, snapshot(2, true));
    let fresh = prepare(host.request().unwrap(), "fresh");
    assert!(host.complete(stale).is_none());
    assert!(host.presentation().is_none());
    assert!(host.complete(fresh).is_some());
    assert_eq!(host.presentation().unwrap().content(), "fresh");
    assert_eq!(host.consumer().applied().unwrap().revision, Revision(2));
}
#[test]
fn failed_resources_or_content_preserve_last_applied_presentation() {
    let mut host = host();
    let initial = prepare(host.request().unwrap(), "kept");
    host.complete(initial).unwrap();
    let tokens = host.presentation().unwrap().appearance().tokens();
    host.consumer_mut().observe(1, snapshot(2, true));
    let error = host.request().unwrap().prepare(
        |name, _| Err(Diagnostic::new("font_unavailable", name, "missing")),
        |_| Ok("bad".to_owned()),
    );
    assert!(host.complete(error).is_none());
    assert_eq!(host.consumer().applied().unwrap().revision, Revision(1));
    assert_eq!(host.presentation().unwrap().appearance().tokens(), tokens);
    assert_eq!(host.presentation().unwrap().content(), "kept");
    let refresh = host.consumer_mut().refresh().unwrap();
    host.consumer_mut()
        .complete(&refresh, Ok(Some(snapshot(2, true))));
    let error = host.request().unwrap().prepare(
        |_, _| {
            Ok(FontSelection {
                font: application::iced::Font::DEFAULT,
                choice: FontChoice::Declared,
            })
        },
        |_| Err(Diagnostic::new("content", "editor", "cannot prepare")),
    );
    assert!(host.complete(error).is_none());
    assert_eq!(host.presentation().unwrap().content(), "kept");
}
#[test]
fn foreign_host_and_disconnected_completion_are_fenced() {
    let mut source = host();
    let mut target = host();
    assert!(
        target
            .complete(prepare(source.request().unwrap(), "foreign"))
            .is_none()
    );
    let captured = prepare(target.request().unwrap(), "disconnected");
    target.consumer_mut().disconnected();
    assert!(target.complete(captured).is_none());
    assert!(target.presentation().is_none());
}
#[test]
fn revision_only_readback_keeps_the_existing_presentation() {
    let mut host = host();
    let initial = prepare(host.request().unwrap(), "same");
    host.complete(initial).unwrap();
    host.consumer_mut().observe(1, snapshot(2, false));
    assert!(host.request().is_none());
    assert_eq!(host.consumer().applied().unwrap().revision, Revision(2));
    assert_eq!(host.presentation().unwrap().content(), "same");
}
#[test]
fn repeated_inputs_do_not_duplicate_resource_preparation() {
    let mut host = host();
    let request = host.request().unwrap();
    assert!(host.request().is_none());
    host.consumer_mut().observe(1, snapshot(2, true));
    assert!(host.request().is_some());
    assert!(host.request().is_none());
    assert!(host.complete(prepare(request, "stale")).is_none());
    assert!(host.request().is_none());
}

#[test]
fn host_worker_failure_does_not_strand_a_preparing_stage() {
    let mut host = host();
    let request = host.request().unwrap();
    let failure = request.failed(Diagnostic::new(
        "worker_failed",
        "resources",
        "worker exited",
    ));
    assert!(host.complete(failure).is_none());
    assert_eq!(host.consumer().fault().unwrap().code, "worker_failed");
    assert!(host.presentation().is_none());
    let work = host.consumer_mut().refresh().unwrap();
    host.consumer_mut()
        .complete(&work, Ok(Some(snapshot(1, false))));
    let completion = prepare(host.request().unwrap(), "recovered");
    assert!(host.complete(completion).is_some());
}

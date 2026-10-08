// SPDX-License-Identifier: MIT OR Apache-2.0
//! One post-start native actor for both finite headless and offline GUI startup.
use super::*;

type Gui = Outbox<Delivery, 4>;
type SettingsLane =
    application::presentation::native::Lane<crate::app::Content, crate::app::PreparationContext>;
type Ready = Result<
    (
        Arc<SupervisedClient>,
        Option<SettingsUi<crate::app::Content, crate::app::PreparationContext>>,
        Option<appearance::settings::Prepared>,
    ),
    StartError,
>;

fn offer(gui: &mut Gui, delivery: Delivery) -> bool {
    let slot = match &delivery {
        Delivery::Settings => Some(0),
        Delivery::Connected | Delivery::Disconnected => Some(1),
        Delivery::ThemeApplied(_) => Some(2),
        Delivery::Stopped { .. } => Some(3),
        Delivery::Command(_)
        | Delivery::Registered
        | Delivery::RegistrationFailed(_)
        | Delivery::Forwarded(_) => None,
    };
    match slot {
        Some(slot) => gui.replace(slot, delivery).is_ok(),
        None => gui.push(delivery).is_ok(),
    }
}
fn wake(gui: &mut Gui, needed: bool) {
    if needed {
        let _ = offer(gui, Delivery::Settings);
    }
}
fn flush(gui: &mut Gui, send: &mut Sender<Delivery>) -> Flush {
    gui.flush_with(|delivery| {
        send.try_send(delivery).map_err(|error| {
            if error.is_full() {
                SendError::Full(error.into_inner())
            } else {
                SendError::Closed(error.into_inner())
            }
        })
    })
}
async fn progress(lane: &mut Option<SettingsLane>) -> Progress {
    match lane {
        Some(lane) => lane.drive().await,
        None => std::future::pending().await,
    }
}
fn publish(
    lane: &mut Option<SettingsLane>,
    gui: &mut Gui,
    event: SettingsEvent<crate::app::Content>,
) {
    if let Some(lane) = lane {
        wake(gui, lane.publish(event));
    }
}

pub(super) fn start(
    service: &str,
    url: &str,
    settings: bool,
    capacity: usize,
    #[cfg(test)] probe: Option<tokio::sync::watch::Sender<ActorProbe>>,
) -> Result<(BusHandle, Receiver<Delivery>), StartError> {
    start_configured(
        service,
        url,
        settings,
        capacity,
        #[cfg(feature = "acceptance")]
        None,
        #[cfg(test)]
        probe,
    )
}

pub(super) fn start_configured(
    service: &str,
    url: &str,
    settings: bool,
    capacity: usize,
    #[cfg(feature = "acceptance")] fixture: Option<application::acceptance::Fixture>,
    #[cfg(test)] probe: Option<tokio::sync::watch::Sender<ActorProbe>>,
) -> Result<(BusHandle, Receiver<Delivery>), StartError> {
    let frames = application::frames::Handle::new();
    let worker_frames = frames.clone();
    #[cfg(feature = "acceptance")]
    let fixture_frames = fixture
        .as_ref()
        .map(|_| application::acceptance::frames::Endpoint::new(frames.clone()));
    #[cfg(feature = "acceptance")]
    let worker_fixture_frames = fixture_frames.clone();
    let (send, events) = channel(capacity);
    let (tx, effects) = tokio::sync::mpsc::unbounded_channel();
    let (ready, receive) = std::sync::mpsc::channel();
    let done = Arc::new((
        std::sync::Mutex::new(Done::default()),
        std::sync::Condvar::new(),
    ));
    let owner = done.clone();
    let service = service.to_owned();
    let url = url.to_owned();
    std::thread::Builder::new()
        .name(format!("{service}-bus"))
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = ready.send(Err(StartError::Unreachable(format!(
                        "Bus runtime: {error}"
                    ))));
                    return;
                }
            };
            let faults = runtime.block_on(worker(
                service,
                url,
                settings,
                send,
                effects,
                ready,
                Presentation {
                    frames: worker_frames,
                    #[cfg(feature = "acceptance")]
                    fixture_frames: worker_fixture_frames,
                    #[cfg(feature = "acceptance")]
                    fixture,
                    #[cfg(test)]
                    probe,
                },
            ));
            runtime.shutdown_timeout(Duration::from_millis(100));
            let (lock, notified) = &*owner;
            let mut done = lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            done.finished = true;
            done.faults = faults.recent().iter().cloned().collect();
            done.fault_count = faults.count();
            notified.notify_all();
        })
        .map_err(|error| StartError::Unreachable(format!("Bus thread: {error}")))?;
    let (client, settings, bootstrap) = receive
        .recv()
        .map_err(|_| StartError::Unreachable("the Bus thread exited".into()))??;
    Ok((
        BusHandle {
            frames,
            #[cfg(feature = "acceptance")]
            fixture_frames,
            tx,
            done,
            client: Some(client),
            settings,
            bootstrap,
            themes: Admission::new(THEME_QUEUE_BOUND),
            quitting: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            forwarded: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        },
        events,
    ))
}

struct Outcome {
    delivery: Option<Delivery>,
    fault: Option<String>,
    extra: Option<Permit>,
}
fn record(outcome: Outcome, gui: &mut Gui, faults: &mut Faults) {
    if let Some(delivery) = outcome.delivery
        && !offer(gui, delivery)
    {
        faults.push("operation result retention invariant failed".into());
    }
    if let Some(error) = outcome.fault {
        faults.push(error);
    }
    if let Some(permit) = outcome.extra {
        permit.finish();
    }
}
fn reply_record(result: Result<(), String>, faults: &mut Faults) {
    if let Err(error) = result {
        faults.push(error);
    }
}

pub(super) fn retire_unsent(effect: Effect) {
    match effect {
        Effect::ThemeApply { permit, .. } => permit.finish(),
        Effect::ForwardOpen { permit, .. } => permit.finish(),
        Effect::Respond { .. } | Effect::Quit => {}
    }
}

enum Origin {
    Local,
    Bus(Accepted),
}
struct ThemeWork {
    request: ThemeRequest,
    origin: Origin,
    generation: Option<u64>,
    deadline: Instant,
    permit: Permit,
}
fn theme_task(
    work: Box<ThemeWork>,
    client: Arc<SupervisedClient>,
) -> (
    Permit,
    impl std::future::Future<Output = Outcome> + Send + 'static,
) {
    let ThemeWork {
        request,
        origin,
        generation,
        deadline,
        permit,
    } = *work;
    let (primary, native, extra) = match origin {
        Origin::Local => (permit, None, None),
        Origin::Bus(accepted) => {
            let (credit, native) =
                accepted.into_task(|client, command, _| async move { (client, command) });
            (credit, Some(native), Some(permit))
        }
    };
    (primary, async move {
        let native = match native {
            Some(native) => Some(native.await),
            None => None,
        };
        let (origin, generation) = match &native {
            Some((client, command)) => (client.clone(), Some(command.generation)),
            None => (client, generation),
        };
        let (rc, body, result) = theme_apply(&origin, generation, deadline, &request).await;
        let mut fault = None;
        if let Some((client, command)) = native {
            // The original accepted owner persists through mutation AND reply.
            match tokio::time::timeout(SHUTDOWN_BUDGET, client.respond(&command, rc, &body)).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => fault = Some(format!("theme reply: {error}")),
                Err(_) => fault = Some("theme reply timed out; delivery unconfirmed".into()),
            }
        }
        Outcome {
            delivery: Some(Delivery::ThemeApplied(result)),
            fault,
            extra,
        }
    })
}

fn queue_reply(
    pending: &mut HashMap<u64, Accepted>,
    retained: &mut Outbox<NativeReply, 0>,
    client: &Arc<SupervisedClient>,
    id: u64,
    rc: u8,
    body: String,
    faults: &mut Faults,
) {
    let Some(accepted) = pending.remove(&id) else {
        return;
    };
    if !accepted.is_current(client) {
        accepted.retire().finish();
        faults.push("accepted reply retired after connection loss".into());
        return;
    }
    if let Err(reply) = retained.push(accepted.reply(rc, body, Instant::now() + SHUTDOWN_BUDGET)) {
        reply.retire().finish();
        faults.push("reply retention invariant failed".into());
    }
}
fn refusal(
    client: &Arc<SupervisedClient>,
    command: IncomingCommand,
    tasks: &mut TaskSet<Result<(), String>>,
    admission: &Admission,
    faults: &mut Faults,
    shed: &mut u64,
) {
    refusal_body(
        client,
        command,
        tasks,
        admission,
        faults,
        shed,
        "{\"error_code\":\"BUSY\",\"message\":\"too many pending commands\"}".into(),
    );
}
fn refusal_body(
    client: &Arc<SupervisedClient>,
    command: IncomingCommand,
    tasks: &mut TaskSet<Result<(), String>>,
    admission: &Admission,
    faults: &mut Faults,
    shed: &mut u64,
    body: String,
) {
    if command.id.is_none() {
        return;
    }
    let Some(permit) = admission.try_acquire() else {
        *shed = shed.saturating_add(1);
        return;
    };
    let now = Instant::now();
    let reply =
        Accepted::new(client.clone(), command, permit, now).reply(10, body, now + SHUTDOWN_BUDGET);
    if let Err(reply) = tasks.try_spawn_with(reply, NativeReply::into_task) {
        reply.retire().finish();
        faults.push("refusal task invariant failed".into());
    }
}
fn submit_themes(
    retained: &mut Outbox<Box<ThemeWork>, 0>,
    tasks: &mut TaskSet<Outcome>,
    client: &Arc<SupervisedClient>,
) {
    retained.flush_with(|work| {
        tasks
            .try_spawn_with(work, |work| theme_task(work, client.clone()))
            .map_err(SendError::Full)
    });
}

struct Presentation {
    frames: application::frames::Handle,
    #[cfg(feature = "acceptance")]
    fixture_frames: Option<application::acceptance::frames::Endpoint>,
    #[cfg(feature = "acceptance")]
    fixture: Option<application::acceptance::Fixture>,
    #[cfg(test)]
    probe: Option<tokio::sync::watch::Sender<ActorProbe>>,
}

async fn worker(
    service: String,
    url: String,
    settings: bool,
    send: Sender<Delivery>,
    effects: tokio::sync::mpsc::UnboundedReceiver<Effect>,
    ready: std::sync::mpsc::Sender<Ready>,
    presentation: Presentation,
) -> Faults {
    let faults = Faults::default();
    let options = match ::bus::client_helpers::local_supervised_options(&service, &url) {
        Ok(options) => options,
        Err(error) => {
            let _ = ready.send(Err(StartError::Unreachable(error.to_string())));
            return faults;
        }
    }
        .fatal_on_registration_rejection(true)
        .bounded_incoming(INCOMING_BOUND);
    let client = if settings {
        Arc::new(options.start())
    } else {
        match tokio::time::timeout(CONNECT_TIMEOUT, options.connect()).await {
            Ok(Ok(client)) => Arc::new(client),
            Ok(Err(error)) => {
                let classification = match error.registration_rejection_typed() {
                    Some(rejection) if rejection.kind() == RegistrationRejectionKind::NameTaken => {
                        StartError::NameTaken
                    }
                    Some(_) => StartError::Rejected(error.to_string()),
                    None => StartError::Unreachable(error.to_string()),
                };
                let _ = ready.send(Err(classification));
                return faults;
            }
            Err(_) => {
                let _ = ready.send(Err(StartError::Unreachable("connect timed out".into())));
                return faults;
            }
        }
    };
    let prepared = if settings {
        settings::session::binding()
            .and_then(|binding| settings::consumer::Consumer::for_app(binding, "dopus"))
            .and_then(|consumer| {
                appearance::settings::bootstrap().map(|bootstrap| (consumer, bootstrap))
            })
    } else {
        // Headless startup owns no unused appearance consumer.
        if ready.send(Ok((client.clone(), None, None))).is_err() {
            client.close().await;
            return faults;
        }
        return run(client, service, url, send, effects, None, presentation).await;
    };
    let (consumer, bootstrap) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            let _ = ready.send(Err(StartError::SettingsBinding(error.message)));
            client.close().await;
            return faults;
        }
    };
    #[cfg(feature = "acceptance")]
    let prepare_hook = presentation
        .fixture
        .as_ref()
        .map(|fixture| fixture.hook.clone());
    let worker = SettingsWorker::contextual(
        move |appearance,
              snapshot: &settings::Snapshot,
              context: &crate::app::PreparationContext| {
            #[cfg(feature = "acceptance")]
            if let Some(hook) = &prepare_hook {
                let fault = |message| {
                    settings::Diagnostic::new("fixture_prepare_cancelled", "dopus.prepare", message)
                };
                let observation = application::acceptance::barrier::Observation::try_new(format!(
                    "revision={} scale={}",
                    snapshot.revision.0,
                    context.scale()
                ))
                .map_err(|error| fault(format!("{error:?}")))?;
                if let Some(permit) = hook
                    .reach("dopus.prepare", observation)
                    .map_err(|error| fault(format!("{error:?}")))?
                {
                    permit
                        .wait_blocking()
                        .map_err(|error| fault(format!("{error:?}")))?;
                }
            }
            crate::app::Content::build_contextual(appearance, snapshot, context)
        },
    )
    .with_contextual_resource_requirements(crate::icons::requirements);
    let worker = match crate::dirs::AppDirs::resolve(crate::dirs::COMPONENT) {
        Some(dirs) => worker.with_cache_directory(dirs.settings_cache_dir()),
        None => worker,
    };
    let (mut ui, lane) = bridge(
        Session::with_context(consumer, crate::app::PreparationContext::default()),
        worker,
    );
    ui.bind_frames(presentation.frames.clone());
    if ready
        .send(Ok((client.clone(), Some(ui), Some(bootstrap))))
        .is_err()
    {
        client.close().await;
        return faults;
    }
    run(
        client,
        service,
        url,
        send,
        effects,
        Some(lane),
        presentation,
    )
    .await
}

async fn run(
    client: Arc<SupervisedClient>,
    service: String,
    url: String,
    mut send: Sender<Delivery>,
    mut effects: tokio::sync::mpsc::UnboundedReceiver<Effect>,
    mut lane: Option<SettingsLane>,
    presentation: Presentation,
) -> Faults {
    let Presentation {
        frames,
        #[cfg(feature = "acceptance")]
        fixture_frames,
        #[cfg(feature = "acceptance")]
        mut fixture,
        #[cfg(test)]
        probe,
    } = presentation;
    // incoming_bounded is consuming; the sole worker takes it here.
    let mut incoming = client
        .incoming_bounded()
        .expect("sole native actor receiver");
    let mut connection = client.subscribe_state();
    let mut gui = Gui::new(PENDING_BOUND + 3);
    if let Some(lane) = &mut lane {
        wake(&mut gui, lane.connect(client.clone()));
    }
    let admission = Admission::new(PENDING_BOUND);
    let refusal_admission = Admission::new(JOB_BOUND);
    let mut pending = HashMap::<u64, Accepted>::new();
    let mut retained = Outbox::<NativeReply, 0>::new(PENDING_BOUND);
    let mut replies = TaskSet::new(JOB_BOUND);
    let mut refusals = TaskSet::new(JOB_BOUND);
    let mut theme_queue = Outbox::<Box<ThemeWork>, 0>::new(THEME_QUEUE_BOUND);
    let mut themes = TaskSet::<Outcome>::new(THEME_QUEUE_BOUND);
    let mut forwards = TaskSet::<Outcome>::new(1);
    let mut faults = Faults::default();
    let mut fixture_controls = TaskSet::<Result<(), String>>::new(4);
    let mut fixture_waits = TaskSet::<Result<(), String>>::new(2);
    #[cfg(feature = "acceptance")]
    let fixture_admission = Admission::new(6);
    let (mut incoming_open, mut connection_open, mut registered) = (true, true, false);
    let mut lifecycle = None;
    let mut next_id = 0u64;
    let mut shed = 0u64;
    loop {
        let state = *connection.borrow_and_update();
        let generation = client.connection_generation();
        if lifecycle != Some((state, generation)) {
            frames.set_live_generation(settings::native::live_generation(&client));
            #[cfg(feature = "acceptance")]
            if lifecycle.is_some()
                && let Some(fixture) = &fixture
            {
                fixture.close(application::acceptance::barrier::ClosedReason::LostGeneration);
            }
            lifecycle = Some((state, generation));
            let stale: Vec<_> = pending
                .iter()
                .filter_map(|(id, accepted)| (!accepted.is_current(&client)).then_some(*id))
                .collect();
            for id in stale {
                pending.remove(&id).unwrap().retire().finish();
                faults.push("command retired on lifecycle change".into());
            }
            gui.retain(|delivery| match delivery {
                Delivery::Command(command) => pending.contains_key(&command.id.id),
                _ => true,
            });
            publish(&mut lane, &mut gui, SettingsEvent::Wake);
            match state {
                ConnState::Connected => {
                    if !registered {
                        registered = true;
                        let _ = offer(&mut gui, Delivery::Registered);
                    }
                    let _ = offer(&mut gui, Delivery::Connected);
                }
                ConnState::Disconnected => {
                    let _ = offer(&mut gui, Delivery::Disconnected);
                }
                ConnState::Fatal | ConnState::ShuttingDown => {
                    let _ = offer(
                        &mut gui,
                        Delivery::RegistrationFailed(registration_error(&client)),
                    );
                }
                ConnState::Connecting => {}
            }
        }
        if flush(&mut gui, &mut send) == Flush::Closed {
            break;
        }
        submit_replies(&mut retained, &mut replies);
        submit_themes(&mut theme_queue, &mut themes, &client);
        #[cfg(test)]
        if let Some(probe) = &probe {
            probe.send_replace(ActorProbe {
                #[cfg(feature = "acceptance")]
                fixture_waits: fixture_waits.len(),
                #[cfg(feature = "acceptance")]
                fixture_controls: fixture_controls.len(),
                #[cfg(feature = "acceptance")]
                fixture_active: fixture_admission.counts().active,
                generation,
                connected: settings::native::live_generation(&client).is_some(),
                pending: pending.len(),
                active: admission.counts().active,
                reliable: gui.reliable_len(),
                reply_tasks: replies.len(),
                replies: retained.len(),
                themes: themes.len() + theme_queue.len(),
                invariant_faults: faults
                    .recent()
                    .iter()
                    .filter(|fault| fault.contains("invariant"))
                    .count(),
            });
        }
        tokio::select! {
            _ = async {
                #[cfg(feature = "acceptance")]
                if let Some(fixture) = fixture.as_mut() { fixture.controller.drive().await; return; }
                std::future::pending::<()>().await;
            } => {},
            result = fixture_controls.join_next(), if !fixture_controls.is_empty() => { if let Some(result) = result { reap("fixture control", result, &mut faults, reply_record); } },
            result = fixture_waits.join_next(), if !fixture_waits.is_empty() => { if let Some(result) = result { reap("fixture wait", result, &mut faults, reply_record); } },
            effect = effects.recv() => match effect {
                Some(Effect::Respond { id, rc, body }) => queue_reply(&mut pending, &mut retained, &client, id, rc, body, &mut faults),
                Some(Effect::ThemeApply { request, generation, deadline, permit }) => {
                    let origin = match request.reply_id.as_ref() {
                        None => Origin::Local,
                        Some(request) => {
                            let Some(accepted) = pending.remove(&request.id) else { permit.finish(); faults.push("missing Bus theme origin retired without mutation".into()); continue; };
                            if !accepted.is_current(&client) { accepted.retire().finish(); permit.finish(); faults.push("stale Bus theme origin retired without mutation".into()); continue; }
                            Origin::Bus(accepted)
                        }
                    };
                    if let Err(work) = theme_queue.push(Box::new(ThemeWork { request: *request, origin, generation, deadline, permit })) {
                        if let Origin::Bus(accepted) = work.origin { accepted.retire().finish(); }
                        work.permit.finish(); faults.push("theme queue invariant failed".into());
                    }
                }
                Some(Effect::ForwardOpen { paths, permit }) => {
                    if client.connection_generation() != 0 || !matches!(registration_error(&client), StartError::NameTaken) {
                        permit.finish(); let _ = offer(&mut gui, Delivery::Forwarded(Err("forward requires initial typed name collision".into()))); continue;
                    }
                    let url = url.clone(); let service = service.clone();
                    if let Err(permit) = forwards.try_spawn_with(permit, |permit| (permit, async move { Outcome { delivery: Some(Delivery::Forwarded(forward_open_async(&url, &service, &paths).await)), fault: None, extra: None } })) {
                        permit.finish(); faults.push("forward task invariant failed".into());
                    }
                }
                Some(Effect::Quit) | None => break,
            },
            update = progress(&mut lane) => match update { Progress::Wake => wake(&mut gui, true), Progress::Updated => {}, Progress::UiClosed => break },
            ready = std::future::poll_fn(|cx| send.poll_ready(cx)), if !gui.is_empty() => { if ready.is_err() { break; } },
            result = replies.join_next(), if !replies.is_empty() => { if let Some(result) = result { reap("reply", result, &mut faults, reply_record); } },
            result = refusals.join_next(), if !refusals.is_empty() => { if let Some(result) = result { reap("refusal", result, &mut faults, reply_record); } },
            result = themes.join_next(), if !themes.is_empty() => { if let Some(result) = result { reap("theme", result, &mut faults, |outcome, faults| record(outcome, &mut gui, faults)); } },
            result = forwards.join_next(), if !forwards.is_empty() => { if let Some(result) = result { reap("forward", result, &mut faults, |outcome, faults| record(outcome, &mut gui, faults)); } },
            delivery = incoming.recv(), if incoming_open => match delivery {
                Some(BoundedIncomingEvent::Command(command)) => {
                    if let Some(lane) = &mut lane && let Some(needed) = lane.delivery(&command) { wake(&mut gui, needed); continue; }
                    if command.topic().is_some() || command.command.is_empty() { continue; }
                    if settings::native::live_generation(&client) != Some(command.generation) { faults.push("stale command retired before frontend admission".into()); continue; }
                    #[cfg(feature = "acceptance")]
                    if let Some(fixture) = &fixture && let Some(class) = application::acceptance::classify(&command.command) {
                        if command.id.is_none() { continue; }
                        let tasks = if class == application::acceptance::Class::Wait { &mut fixture_waits } else { &mut fixture_controls };
                        let permit = if tasks.is_full() { None } else { fixture_admission.try_acquire() };
                        let Some(permit) = permit else {
                            refusal_body(&client, command, &mut refusals, &refusal_admission, &mut faults, &mut shed, "{\"error\":\"acceptance capacity exhausted\"}".into()); continue;
                        };
                        let accepted = Accepted::new(client.clone(), command, permit, Instant::now());
                        if let Err(accepted) = tasks.try_spawn_with(accepted, |accepted| accepted.into_task(|client, command, _| {
                            let future = application::acceptance::track_result(client, command, &fixture.describe, &fixture.inspector, &fixture.controller, fixture_frames.as_ref()).expect("exact fixture verb");
                            async move { future.await.map_err(|error| format!("acceptance: {error:?}")) }
                        })) { accepted.retire().finish(); faults.push("fixture task capacity invariant failed".into()); }
                        continue;
                    }
                    if command.command == "app.describe" && let Err(error) = application::describe::validate_request(&command.body) {
                        refusal_body(&client, command, &mut refusals, &refusal_admission, &mut faults, &mut shed, crate::verbs::describe_refusal(&error));
                        continue;
                    }
                    let Some(permit) = admission.try_acquire() else { refusal(&client, command, &mut refusals, &refusal_admission, &mut faults, &mut shed); continue; };
                    let Some(id) = next_id.checked_add(1) else { permit.finish(); refusal(&client, command, &mut refusals, &refusal_admission, &mut faults, &mut shed); continue; };
                    next_id = id;
                    let delivery = Delivery::Command(Command { id: Request::new(id, command.generation), verb: command.command.clone(), body: if command.body.trim().is_empty() { "{}".into() } else { command.body.clone() }, caller_key: caller_key(&command) });
                    pending.insert(id, Accepted::new(client.clone(), command, permit, Instant::now()));
                    if !offer(&mut gui, delivery) { pending.remove(&id).unwrap().retire().finish(); faults.push("command retention invariant failed".into()); break; }
                }
                Some(BoundedIncomingEvent::Overflow { .. }) => publish(&mut lane, &mut gui, SettingsEvent::Lost),
                None => { incoming_open = false; publish(&mut lane, &mut gui, SettingsEvent::Wake); }
            },
            changed = connection.changed(), if connection_open => { if changed.is_err() { connection_open = false; } },
        }
    }
    let deadline = Instant::now() + SHUTDOWN_BUDGET;
    frames.close();
    #[cfg(feature = "acceptance")]
    if let Some(fixture) = &fixture {
        fixture.close(application::acceptance::barrier::ClosedReason::Shutdown);
    }
    for (label, tasks) in [
        ("fixture control", &mut fixture_controls),
        ("fixture wait", &mut fixture_waits),
    ] {
        while !tasks.is_empty() {
            match tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                tasks.join_next(),
            )
            .await
            {
                Ok(Some(result)) => reap(label, result, &mut faults, reply_record),
                Ok(None) | Err(_) => break,
            }
        }
    }
    cancel(
        "fixture control",
        fixture_controls,
        &mut faults,
        reply_record,
    );
    cancel("fixture wait", fixture_waits, &mut faults, reply_record);
    effects.close();
    while let Ok(effect) = effects.try_recv() {
        match effect {
            Effect::Respond { id, rc, body } => queue_reply(
                &mut pending,
                &mut retained,
                &client,
                id,
                rc,
                body,
                &mut faults,
            ),
            other => retire_unsent(other),
        }
    }
    for (_, accepted) in pending.drain() {
        accepted.retire().finish();
        faults.push("accepted command retired without frontend reply".into());
    }
    // Freeze mutation admission on shutdown; an unsent CAS must not start later.
    for work in theme_queue.drain() {
        if let Origin::Bus(accepted) = work.origin {
            accepted.retire().finish();
        }
        work.permit.finish();
        faults.push("queued theme mutation retired unsent".into());
    }
    while !replies.is_empty() || !retained.is_empty() {
        if Instant::now() >= deadline {
            faults.push("accepted reply drain timed out".into());
            break;
        }
        submit_replies(&mut retained, &mut replies);
        match tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            replies.join_next(),
        )
        .await
        {
            Ok(Some(result)) => reap("reply", result, &mut faults, reply_record),
            Ok(None) => break,
            Err(_) => {
                faults.push("accepted reply drain timed out".into());
                break;
            }
        }
    }
    for reply in retained.drain() {
        reply.retire().finish();
        faults.push("reply retired unsent".into());
    }
    for (label, tasks) in [("theme", &mut themes), ("forward", &mut forwards)] {
        while !tasks.is_empty() {
            match tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                tasks.join_next(),
            )
            .await
            {
                Ok(Some(result)) => reap(label, result, &mut faults, |outcome, faults| {
                    record(outcome, &mut gui, faults)
                }),
                Ok(None) | Err(_) => break,
            }
        }
    }
    while !refusals.is_empty() {
        match tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            refusals.join_next(),
        )
        .await
        {
            Ok(Some(result)) => reap("refusal", result, &mut faults, reply_record),
            Ok(None) | Err(_) => break,
        }
    }
    cancel("reply", replies, &mut faults, reply_record);
    cancel("refusal", refusals, &mut faults, reply_record);
    cancel("theme", themes, &mut faults, |outcome, faults| {
        record(outcome, &mut gui, faults)
    });
    cancel("forward", forwards, &mut faults, |outcome, faults| {
        record(outcome, &mut gui, faults)
    });
    if let Some(lane) = &mut lane
        && let Err(error) = lane.flush_cache(deadline).await
    {
        faults.push(format!("settings cache: {}: {}", error.code, error.message));
    }
    if tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), client.close())
        .await
        .is_err()
    {
        faults.push("Bus close timed out".into());
    }
    let _ = offer(
        &mut gui,
        Delivery::Stopped {
            faults: faults.recent().iter().cloned().collect(),
        },
    );
    let _ = flush(&mut gui, &mut send);
    let counts = admission.counts();
    #[cfg(feature = "acceptance")]
    let fixture_accounting = {
        let counts = fixture_admission.counts();
        serde_json::json!({"active":counts.active,"finished":counts.finished,"abandoned":counts.abandoned})
    };
    #[cfg(not(feature = "acceptance"))]
    let fixture_accounting = serde_json::Value::Null;
    eprintln!(
        "DOPUS_SHUTDOWN {}",
        serde_json::json!({"faults":faults,"shed_refusals":shed,"fixture_admission":fixture_accounting,"admission":{"active":counts.active,"finished":counts.finished,"abandoned":counts.abandoned}})
    );
    faults
}

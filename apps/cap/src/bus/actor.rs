// SPDX-License-Identifier: MIT OR Apache-2.0
//! Cap's existing worker loop using shared retained admission and native tasks.
use super::*;

type Gui = Outbox<Delivery, 4>;

#[cfg(test)]
#[tokio::test]
async fn long_capture_gets_its_reply_budget_when_the_answer_exists() {
    let broker = term_test_broker::Broker::start_stable();
    let client = Arc::new(
        SupervisedClient::connect_options("cap-long-answer", &broker.url)
            .bounded_incoming(1)
            .connect()
            .await
            .unwrap(),
    );
    let mut incoming = client.incoming_bounded().unwrap();
    let caller = Arc::new(NodedClient::connect_anonymous(&broker.url).await.unwrap());
    let calling = caller.clone();
    let call = tokio::spawn(async move {
        calling
            .call_with_headers_raw("cap-long-answer", "cap.capture", &BTreeMap::new(), "{}")
            .await
    });
    let Some(BoundedIncomingEvent::Command(command)) =
        tokio::time::timeout(Duration::from_secs(5), incoming.recv())
            .await
            .expect("actual capture admission deadline")
    else {
        panic!("actual capture command missing")
    };
    let admission = Admission::new(1);
    let mut pending = HashMap::from([(
        1,
        Accepted::new(
            client.clone(),
            command,
            admission.try_acquire().unwrap(),
            Instant::now() - Duration::from_secs(60),
        ),
    )]);
    let mut retained = Outbox::new(1);
    let mut tasks = TaskSet::new(1);
    let mut faults = Faults::default();
    queue_reply(
        &mut pending,
        &mut retained,
        &client,
        1,
        0,
        "{\"captured\":true}".into(),
        &mut faults,
    );
    submit_replies(&mut retained, &mut tasks);
    reap(
        "capture reply",
        tasks.join_next().await.unwrap(),
        &mut faults,
        record,
    );
    assert_eq!(faults.count(), 0);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), call)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .1,
        "{\"captured\":true}"
    );
    assert_eq!(admission.counts().finished, 1);
    caller.close().await;
    client.close().await;
}

fn offer(gui: &mut Gui, delivery: Delivery) -> bool {
    let slot = match &delivery {
        Delivery::Settings => Some(0),
        Delivery::Changed => Some(1),
        Delivery::Connected | Delivery::Disconnected => Some(2),
        Delivery::Refused { .. } => Some(3),
        Delivery::Command(_) | Delivery::Forwarded(_) => None,
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

fn flush(gui: &mut Gui, send: &mut mpsc::Sender<Delivery>) -> Flush {
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

pub(super) fn retire_unsent(effect: Effect) {
    match effect {
        Effect::Call { reply, permit, .. } => {
            let _ = reply.send(Err("Bus shutdown; queued call not sent".into()));
            permit.finish();
        }
        Effect::Delay { reply, permit, .. } => {
            drop(reply);
            permit.finish();
        }
        Effect::Respond { .. } | Effect::Quit => {}
    }
}

fn operation(
    effect: Effect,
    client: Arc<SupervisedClient>,
) -> (
    Permit,
    impl std::future::Future<Output = ()> + Send + 'static,
) {
    enum Work {
        Call(
            String,
            String,
            Value,
            Instant,
            Option<u64>,
            oneshot::Sender<Result<Value, String>>,
        ),
        Delay(Instant, oneshot::Sender<()>),
    }
    let (permit, work) = match effect {
        Effect::Call {
            service,
            verb,
            args,
            deadline,
            generation,
            reply,
            permit,
        } => (
            permit,
            Work::Call(service, verb, args, deadline, generation, reply),
        ),
        Effect::Delay {
            when,
            reply,
            permit,
        } => (permit, Work::Delay(when, reply)),
        _ => unreachable!("only owned operations enter this task set"),
    };
    (permit, async move {
        match work {
            Work::Call(service, verb, args, deadline, generation, reply) => {
                if reply.is_canceled()
                    || Instant::now() >= deadline
                    || generation.is_none()
                    || settings::native::live_generation(&client) != generation
                {
                    let _ = reply.send(Err(
                        "queued Bus call retired or expired; no call sent".into()
                    ));
                    return;
                }
                let result = tokio::time::timeout_at(
                    tokio::time::Instant::from_std(deadline),
                    client.call_with_headers_raw_at_generation(
                        generation.expect("checked generation"),
                        &service,
                        &verb,
                        &BTreeMap::new(),
                        &args.to_string(),
                    ),
                )
                .await
                .map_err(|_| "Bus request timed out; outcome unknown".to_owned())
                .and_then(|result| result.map_err(|error| error.to_string()))
                .and_then(|(rc, body, _)| {
                    if rc == 0 {
                        serde_json::from_str(&body).map_err(|error| error.to_string())
                    } else {
                        Err(body)
                    }
                });
                let _ = reply.send(result);
            }
            Work::Delay(when, reply) => {
                if reply.is_canceled() {
                    return;
                }
                tokio::time::sleep_until(tokio::time::Instant::from_std(when)).await;
                let _ = reply.send(());
            }
        }
    })
}

fn record(result: Result<(), String>, faults: &mut Faults) {
    if let Err(error) = result {
        faults.push(error);
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
    refuse_body(
        client,
        command,
        tasks,
        admission,
        faults,
        shed,
        BUSY_BODY.into(),
    );
}

fn refuse_body(
    client: &Arc<SupervisedClient>,
    command: IncomingCommand,
    tasks: &mut TaskSet<Result<(), String>>,
    admission: &Admission,
    faults: &mut Faults,
    shed: &mut u64,
    body: String,
) {
    let Some(permit) = admission.try_acquire() else {
        *shed = shed.saturating_add(1);
        return;
    };
    let now = Instant::now();
    let reply =
        Accepted::new(client.clone(), command, permit, now).reply(10, body, now + SHUTDOWN_BUDGET);
    if let Err(reply) = tasks.try_spawn_with(reply, NativeReply::into_task) {
        reply.retire().finish();
        *shed = shed.saturating_add(1);
        faults.push("refusal task capacity invariant failed".into());
    }
}

fn queue_reply(
    pending: &mut HashMap<u64, Accepted>,
    accepted: &mut Outbox<NativeReply, 0>,
    client: &Arc<SupervisedClient>,
    id: u64,
    rc: u8,
    body: String,
    faults: &mut Faults,
) {
    let Some(entry) = pending.remove(&id) else {
        return;
    };
    if !entry.is_current(client) {
        faults.push("accepted reply retired after connection loss".into());
        entry.retire().finish();
        return;
    }
    // Capture includes user delay, region selection and restoration. The send
    // budget starts when its answer exists; it includes retained reply delay.
    let deadline = Instant::now() + Duration::from_secs(5);
    if let Err(reply) = accepted.push(entry.reply(rc, body, deadline)) {
        reply.retire().finish();
        faults.push("accepted reply retention invariant failed".into());
    }
}

pub(super) async fn worker(
    service: String,
    url: String,
    mut handoff: Option<Vec<String>>,
    mut send: mpsc::Sender<Delivery>,
    mut effects: tokio::sync::mpsc::UnboundedReceiver<Effect>,
    ready: Ready,
    #[cfg(test)] probe: Option<tokio::sync::watch::Sender<ActorProbe>>,
) {
    let consumer = match settings::session::binding()
        .and_then(|binding| settings::consumer::Consumer::for_app(binding, "cap"))
    {
        Ok(consumer) => consumer,
        Err(error) => {
            let _ = ready.send(Err(error.message));
            return;
        }
    };
    let bootstrap = match appearance::settings::bootstrap() {
        Ok(bootstrap) => bootstrap,
        Err(error) => {
            let _ = ready.send(Err(error.message));
            return;
        }
    };
    let client = Arc::new(
        SupervisedClient::connect_options(&service, &url)
            .fatal_on_registration_rejection(true)
            .bounded_incoming(64)
            .start(),
    );
    let Some(mut incoming) = client.incoming_bounded() else {
        let _ = ready.send(Err("no incoming Bus channel".into()));
        return;
    };
    let mut connection = client.subscribe_state();
    let build = |_: &appearance::settings::Prepared, _: &settings::Snapshot| Ok(());
    let settings_worker = match config::AppDirs::resolve("cap") {
        Some(dirs) => Worker::offline_with_cache(dirs.cache().join("settings"), build),
        None => Worker::offline(build),
    };
    let (ui, mut lane) = bridge(Session::new(consumer), settings_worker);
    let mut gui = Gui::new(ACCEPTED_CAP + 1);
    wake(&mut gui, lane.connect(client.clone()));
    if ready.send(Ok((client.clone(), ui, bootstrap))).is_err() {
        client.close().await;
        return;
    }
    if let Err(error) = appearance::fonts::register_installed() {
        eprintln!("cap: static assets: {error}");
    }
    let admission = Admission::new(ACCEPTED_CAP);
    let refusal_admission = Admission::new(REFUSAL_CAP);
    let forward_admission = Admission::new(1);
    let mut pending = HashMap::<u64, Accepted>::new();
    let mut accepted = Outbox::<NativeReply, 0>::new(ACCEPTED_CAP);
    // Cap intentionally serialises native replies. Retained queue delay is
    // included in each absolute deadline; completed work retains its credit.
    let mut replies = TaskSet::<Result<(), String>>::new(1);
    let mut refusals = TaskSet::<Result<(), String>>::new(REFUSAL_CAP);
    let mut operations = TaskSet::<()>::new(OPERATION_CAP + CLEANUP_CAP);
    let mut forwards = TaskSet::<Result<(), String>>::new(1);
    let mut faults = Faults::default();
    let (mut incoming_open, mut connection_open) = (true, true);
    let mut lifecycle = None;
    let mut next_id = 0u64;
    let mut shed_refusals = 0u64;
    loop {
        let state = *connection.borrow_and_update();
        let generation = client.connection_generation();
        if lifecycle != Some((state, generation)) {
            lifecycle = Some((state, generation));
            let stale: Vec<_> = pending
                .iter()
                .filter_map(|(id, entry)| (!entry.is_current(&client)).then_some(*id))
                .collect();
            for id in stale {
                if let Some(entry) = pending.remove(&id) {
                    faults.push("accepted command retired on lifecycle change".into());
                    entry.retire().finish();
                }
            }
            gui.retain(|delivery| match delivery {
                Delivery::Command(command) => pending.contains_key(&command.id.id),
                _ => true,
            });
            wake(&mut gui, lane.publish(SettingsEvent::Wake));
            match state {
                ConnState::Connected => {
                    let _ = offer(&mut gui, Delivery::Connected);
                }
                ConnState::Disconnected => {
                    let _ = offer(&mut gui, Delivery::Disconnected);
                }
                ConnState::Fatal | ConnState::ShuttingDown => {
                    let rejection = client.registration_rejection();
                    let message = rejection
                        .clone()
                        .map_or_else(|| "connection stopped".into(), |reason| reason.message);
                    let _ = offer(&mut gui, Delivery::Refused { message });
                    if rejection
                        .as_ref()
                        .is_some_and(|reason| handoff_gate(reason.kind(), generation, &handoff))
                        && let Some(paths) = handoff.take()
                    {
                        let url = url.clone();
                        let service = service.clone();
                        let permit = forward_admission
                            .try_acquire()
                            .expect("one lifetime handoff");
                        if let Err(permit) = forwards.try_spawn_with(permit, |permit| {
                            (permit, async move {
                                forward_async(&url, &service, &paths).await
                            })
                        }) {
                            permit.finish();
                            faults.push("forward task capacity invariant failed".into());
                            let _ = offer(
                                &mut gui,
                                Delivery::Forwarded(Err("handoff capacity exhausted".into())),
                            );
                        }
                    }
                }
                ConnState::Connecting => {}
            }
        }
        if flush(&mut gui, &mut send) == Flush::Closed {
            break;
        }
        submit_replies(&mut accepted, &mut replies);
        #[cfg(test)]
        if let Some(probe) = &probe {
            probe.send_replace(ActorProbe {
                generation,
                connected: settings::native::live_generation(&client).is_some(),
                pending: pending.len(),
                active: admission.counts().active,
                reliable: gui.reliable_len(),
                replies: accepted.len(),
                reply_tasks: replies.len(),
                operations: operations.len(),
                invariant_faults: faults
                    .recent()
                    .iter()
                    .filter(|error| error.contains("invariant"))
                    .count(),
            });
        }
        tokio::select! {
            effect = effects.recv() => match effect {
                Some(Effect::Respond { id, rc, body }) => queue_reply(&mut pending, &mut accepted, &client, id, rc, body, &mut faults),
                Some(effect @ (Effect::Call { .. } | Effect::Delay { .. })) => {
                    if let Err(effect) = operations.try_spawn_with(effect, |effect| operation(effect, client.clone())) {
                        retire_unsent(effect); faults.push("outgoing task capacity invariant failed".into());
                    }
                }
                Some(Effect::Quit) | None => break,
            },
            progress = lane.drive() => match progress { Progress::Wake => wake(&mut gui, true), Progress::UiClosed => break, Progress::Updated => {} },
            ready = std::future::poll_fn(|cx| send.poll_ready(cx)), if !gui.is_empty() => {
                if ready.is_err() || flush(&mut gui, &mut send) == Flush::Closed { break; }
            }
            result = replies.join_next(), if !replies.is_empty() => { if let Some(result) = result { reap("accepted reply", result, &mut faults, record); } }
            result = refusals.join_next(), if !refusals.is_empty() => { if let Some(result) = result { reap("refusal", result, &mut faults, record); } }
            result = operations.join_next(), if !operations.is_empty() => { if let Some(result) = result { reap("Bus operation", result, &mut faults, |(), _| {}); } }
            result = forwards.join_next(), if !forwards.is_empty() => {
                if let Some(Ok(Completed { permit, value })) = result {
                    if !offer(&mut gui, Delivery::Forwarded(value)) { faults.push("handoff retention invariant failed".into()); }
                    permit.finish();
                } else if let Some(Err(error)) = result { faults.push(format!("handoff task: {error}")); }
            }
            command = incoming.recv(), if incoming_open => match command {
                Some(BoundedIncomingEvent::Command(command)) => {
                    if let Some(needed) = lane.delivery(&command) { wake(&mut gui, needed); continue; }
                    if command.topic().is_some() || command.command.is_empty() { continue; }
                    if settings::native::live_generation(&client) != Some(command.generation) { faults.push("stale command retired before frontend admission".into()); continue; }
                    if command.command == application::describe::VERB
                        && let Err(error) = application::describe::validate_request(&command.body) {
                        refuse_body(&client, command, &mut refusals, &refusal_admission, &mut faults,
                            &mut shed_refusals, crate::verbs::describe_refusal(&error).to_string());
                        continue;
                    }
                    let Some(permit) = admission.try_acquire() else { refusal(&client, command, &mut refusals, &refusal_admission, &mut faults, &mut shed_refusals); continue; };
                    let Some(id) = next_id.checked_add(1) else { permit.finish(); refusal(&client, command, &mut refusals, &refusal_admission, &mut faults, &mut shed_refusals); continue; };
                    next_id = id;
                    let delivery = Delivery::Command(Command { id: Request::new(id, command.generation), verb: command.command.clone(), body: if command.body.trim().is_empty() { "{}".into() } else { command.body.clone() }, caller_key: caller_key(&command) });
                    pending.insert(id, Accepted::new(client.clone(), command, permit, Instant::now()));
                    if !offer(&mut gui, delivery) { pending.remove(&id).expect("just admitted").retire().finish(); faults.push("frontend retention invariant failed".into()); break; }
                }
                Some(BoundedIncomingEvent::Overflow { .. }) => { wake(&mut gui, lane.publish(SettingsEvent::Lost)); let _ = offer(&mut gui, Delivery::Changed); }
                None => { incoming_open = false; wake(&mut gui, lane.publish(SettingsEvent::Wake)); }
            },
            changed = connection.changed(), if connection_open => { if changed.is_err() { connection_open = false; } }
        }
    }
    let deadline = Instant::now() + SHUTDOWN_BUDGET;
    effects.close();
    while let Ok(effect) = effects.try_recv() {
        match effect {
            Effect::Respond { id, rc, body } => queue_reply(
                &mut pending,
                &mut accepted,
                &client,
                id,
                rc,
                body,
                &mut faults,
            ),
            effect => retire_unsent(effect),
        }
    }
    for (_, entry) in pending.drain() {
        faults.push("accepted command retired without frontend reply".into());
        entry.retire().finish();
    }
    for _ in gui.drain() {}
    while !replies.is_empty() || !accepted.is_empty() {
        if Instant::now() >= deadline {
            faults.push("accepted reply drain timed out; delivery unconfirmed".into());
            break;
        }
        submit_replies(&mut accepted, &mut replies);
        match tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            replies.join_next(),
        )
        .await
        {
            Ok(Some(result)) => reap("accepted reply", result, &mut faults, record),
            Ok(None) => break,
            Err(_) => {
                faults.push("accepted reply drain timed out; delivery unconfirmed".into());
                break;
            }
        }
    }
    let queued_accepted = accepted.len();
    for reply in accepted.drain() {
        reply.retire().finish();
        faults.push("accepted reply retired unsent".into());
    }
    while !refusals.is_empty() {
        match tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            refusals.join_next(),
        )
        .await
        {
            Ok(Some(result)) => reap("refusal", result, &mut faults, record),
            Ok(None) | Err(_) => break,
        }
    }
    // Running restoration calls retain their reserved credit and are allowed
    // the remaining common deadline before cancellation is requested.
    while !operations.is_empty() {
        match tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            operations.join_next(),
        )
        .await
        {
            Ok(Some(result)) => reap("Bus operation", result, &mut faults, |(), _| {}),
            Ok(None) | Err(_) => break,
        }
    }
    while !forwards.is_empty() {
        match tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            forwards.join_next(),
        )
        .await
        {
            Ok(Some(result)) => reap("handoff", result, &mut faults, record),
            Ok(None) | Err(_) => break,
        }
    }
    cancel("accepted reply", replies, &mut faults, record);
    cancel("refusal", refusals, &mut faults, record);
    cancel("Bus operation", operations, &mut faults, |(), _| {});
    cancel("handoff", forwards, &mut faults, record);
    if let Err(error) = lane.flush_cache(deadline).await {
        faults.push(format!("settings cache: {}: {}", error.code, error.message));
    }
    if tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), client.close())
        .await
        .is_err()
    {
        faults.push("Bus close timed out".into());
    }
    let counts = admission.counts();
    eprintln!(
        "CAP_SHUTDOWN {}",
        json!({"faults": faults, "queued_accepted": queued_accepted, "shed_refusals": shed_refusals, "admission": {"active": counts.active, "finished": counts.finished, "abandoned": counts.abandoned}})
    );
}

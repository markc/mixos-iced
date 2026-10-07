//! No input, Wayland traffic, vblank or watchdog is available to help these
//! requests. Timeouts only bound test failure; the loop must wake on its source.

use std::cell::Cell;
use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use comp_service::{CompEngine, LongReply, PortService};
use comp_model::observation::PanelRequest;
use comp_model::reply::ControlReply;
use comp_model::request::{InputOp, LongOp, SelectionIdentity, WindowOp, WorkspaceIndex};
use comp_model::snapshot::{CompSnapshot, ReadScopes};
use dispatcher::state::state::{Dispatch, RedrawReason};
use policy::workspaces::{DefaultOutput, WorkspaceState, service_switch};
use surfaces::Registry;
use protocols::tearing::gate::gate::{self, Gate};
use serde_json::Value;
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::calloop::ping::make_ping;
use smithay::reexports::wayland_server::Display;

struct Engine {
    dispatch: Dispatch,
    workspaces: WorkspaceState,
    frames: u32,
    reason: RedrawReason,
}

impl CompEngine for Engine {
    fn window(&mut self, op: &WindowOp) -> ControlReply {
        let WindowOp::SwitchWorkspace {
            output,
            index,
            wrap,
        } = op
        else {
            panic!("unexpected control")
        };
        let default = DefaultOutput {
            key: "o_kms".into(),
            name: "kms".into(),
        };
        let (reply, effects) = service_switch(
            &mut self.workspaces,
            &Registry::<()>::new(),
            Some(&default),
            output.as_deref(),
            *index,
            *wrap,
        );
        assert!(!effects.is_empty(), "the request changed workspace state");
        // The same production redraw entry point used by control::execute.
        self.dispatch.schedule_redraw(RedrawReason::Workspace);
        reply
    }

    fn snapshot(&mut self, _: &ReadScopes) -> Option<CompSnapshot> {
        unreachable!()
    }
    fn set(&mut self, _: &str, _: &Value, _: Option<u64>) -> ControlReply {
        unreachable!()
    }
    fn input(&mut self, _: &InputOp) -> ControlReply {
        unreachable!()
    }
    fn panel(&mut self, _: &PanelRequest) -> ControlReply {
        unreachable!()
    }
    fn region_cancel(&mut self, _: &SelectionIdentity) -> ControlReply {
        unreachable!()
    }
    fn start_long(&mut self, _: LongOp, _: LongReply, _: Instant) {
        unreachable!()
    }
    fn services_live(&mut self, _: &BTreeSet<String>) {
        unreachable!()
    }
    fn watch_props(&mut self, _: bool) -> bool {
        unreachable!()
    }
    fn renew_pointer_lease(&mut self) {
        unreachable!()
    }
}

struct Data {
    engine: Engine,
    port: PortService,
}

/// One test owns the process-global gate, so changing it cannot race another
/// case in this integration-test executable.
#[test]
fn bus_and_iced_wakes_schedule_frames_without_input_even_when_paced() {
    struct RestoreGate(Gate);
    impl Drop for RestoreGate {
        fn drop(&mut self) {
            gate::set(self.0);
        }
    }
    let _restore = RestoreGate(gate::get());
    let mut event_loop: EventLoop<'static, Data> = EventLoop::try_new().unwrap();
    let display: Display<Dispatch> = Display::new().unwrap();
    let mut dispatch =
        dispatcher::wire::wire::new_dispatch(&display.handle(), None, event_loop.handle());
    let (redraw, source) = make_ping().unwrap();
    dispatch.redraw.set_ping(redraw);
    dispatch.redraw.rendering("o_kms");
    dispatch.redraw.frame("o_kms", false);
    event_loop
        .handle()
        .insert_source(source, |_, _, data: &mut Data| {
            let engine = &mut data.engine;
            assert!(engine.dispatch.redraw.pending(), "the wake must owe pixels");
            engine.dispatch.redraw.rendering("o_kms");
            assert!(
                engine
                    .dispatch
                    .redraw
                    .frame("o_kms", false)
                    .contains(engine.reason)
            );
            engine.frames += 1;
        })
        .unwrap();

    // Match compd's worker Ping -> pending flag -> post-dispatch service path.
    let (ping, source) = make_ping().unwrap();
    let pending = Rc::new(Cell::new(false));
    let flag = Rc::clone(&pending);
    event_loop
        .handle()
        .insert_source(source, move |_, _, _| flag.set(true))
        .unwrap();
    let (wiring, starter) = comp_service::prepare(
        comp_service::PortIdentity {
            service: "comp-wake-test".into(),
            version: "test".into(),
            backend: "kms-live",
            engine: "test",
            noded_url: "unused".into(),
        },
        Arc::new(move || ping.ping()),
    )
    .unwrap();
    let ingress = starter.ingress().clone();
    let mut data = Data {
        engine: Engine {
            dispatch,
            workspaces: WorkspaceState::default(),
            frames: 0,
            reason: RedrawReason::Workspace,
        },
        port: wiring.service,
    };

    for (gate, index) in [(Gate::Off, 2), (Gate::Tagged, 3)] {
        gate::set(gate);
        let ingress = ingress.clone();
        let (sent, reply) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let admission = ingress
                    .request_window(WindowOp::SwitchWorkspace {
                        output: None,
                        index: WorkspaceIndex::Absolute(index),
                        wrap: false,
                    })
                    .unwrap();
                sent.send(admission.receive().await.unwrap().wire_json())
                    .unwrap();
            });
        });
        let frames = data.engine.frames;
        event_loop
            .dispatch(Duration::from_secs(1), &mut data)
            .unwrap();
        assert!(
            pending.replace(false),
            "Bus ingress must wake a blocked loop"
        );
        data.port.service(&mut data.engine);
        assert_eq!(data.engine.workspaces.current_for(Some("o_kms")), index);
        // A loop reply wakes the Bus worker before the next render/loop turn.
        assert_eq!(
            reply.recv_timeout(Duration::from_secs(1)).unwrap()["to"],
            index
        );
        worker.join().unwrap();
        assert!(data.engine.dispatch.redraw.needs("o_kms"));
        event_loop
            .dispatch(Duration::from_secs(1), &mut data)
            .unwrap();
        assert_eq!(data.engine.frames, frames + 1);
    }

    // The renderer notifier must wake through the installed iced source, then
    // schedule pixels independently of the still-engaged client cadence gate.
    data.engine.reason = RedrawReason::Iced;
    ui::engine::wake::register(&event_loop.handle(), |data: &mut Data| {
        data.engine.dispatch.schedule_redraw(RedrawReason::Iced);
    })
    .unwrap();
    let flags = ui::engine::DirtyFlags::new();
    let worker_flags = flags.clone();
    let worker = std::thread::spawn(move || worker_flags.request_redraw());
    let frames = data.engine.frames;
    event_loop
        .dispatch(Duration::from_secs(1), &mut data)
        .unwrap();
    assert!(flags.redraw_pending());
    event_loop
        .dispatch(Duration::from_secs(1), &mut data)
        .unwrap();
    worker.join().unwrap();
    assert_eq!(data.engine.frames, frames + 1);

    // A page rasterised into a pipelined iced texture may still owe publication
    // after that frame. Preserve its continuation under the same cadence gate.
    data.engine
        .dispatch
        .schedule_redraw_post_vblank(RedrawReason::Iced);
    assert!(data.engine.dispatch.redraw.needs("o_kms"));
    assert!(
        data.engine
            .dispatch
            .redraw
            .ledger()
            .pending("o_kms")
            .contains(RedrawReason::Iced)
    );
    data.engine.dispatch.redraw.rendering("o_kms");
    data.engine.dispatch.redraw.frame("o_kms", false);

    // An exact iced deadline is also work, even with no paced client commits.
    data.engine.dispatch.redraw.request_at(
        &event_loop.handle(),
        Instant::now() + Duration::from_millis(1),
        RedrawReason::Iced,
        |data| &mut data.engine.dispatch.redraw,
    );
    let frames = data.engine.frames;
    event_loop
        .dispatch(Duration::from_secs(1), &mut data)
        .unwrap();
    event_loop
        .dispatch(Duration::from_secs(1), &mut data)
        .unwrap();
    assert_eq!(data.engine.frames, frames + 1);
    assert_eq!(
        data.engine
            .dispatch
            .redraw
            .deadline_armed(Some(RedrawReason::Iced)),
        None
    );
}

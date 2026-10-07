// SPDX-License-Identifier: MIT OR Apache-2.0
#![cfg(all(feature = "native-inspect", feature = "test-support", feature = "tiny-skia"))]
//! Native layout queries run through the headless test runtime: they read the
//! retained cached layout, produce no application message, and report real
//! geometry with per-alias outcomes. The no-redraw guarantee is structural on
//! the native side (the query action has no redraw branch); the native
//! acceptance gate asserts the frame evidence.

use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};

use application::iced::futures::StreamExt;
use application::iced::futures::executor::{ThreadPool, block_on};
use application::iced::futures::task::noop_waker_ref;
use application::iced::{Element, Length, Rectangle, Size, Task, Theme, widget};
use application::inspect::{
    AliasResult, AliasStatus, Error, Handle, Kind, Layer, Limits, Request, Snapshot, Target, Window,
    channel,
};
use application::test::core::{Point, Settings, window};
use application::test::emulator::{self, Emulator};
use application::test::program::Program;
use application::test::runtime::futures::futures::channel::mpsc;

#[derive(Debug, Clone)]
enum Msg {}

struct App {
    handle: Handle,
    task: Mutex<Option<Task<Msg>>>,
    updates: AtomicUsize,
}

impl Program for App {
    type State = ();
    type Message = Msg;
    type Theme = Theme;
    type Renderer = application::cpu::Renderer;
    type Executor = ThreadPool;

    fn name() -> &'static str {
        "application_inspect_test"
    }

    fn settings(&self) -> Settings {
        Settings::default()
    }

    fn boot(&self) -> (Self::State, Task<Self::Message>) {
        let task = self.task.lock().unwrap().take().expect("boots once");
        ((), task)
    }

    fn update(&self, _state: &mut Self::State, _message: Self::Message) -> Task<Self::Message> {
        self.updates.fetch_add(1, Ordering::SeqCst);
        Task::none()
    }

    fn view<'a>(
        &self,
        _state: &'a Self::State,
        _window: window::Id,
    ) -> Element<'a, Self::Message, Self::Theme, Self::Renderer> {
        widget::container(widget::text("probe"))
            .width(Length::Fill)
            .height(Length::Fill)
            .id(widget::Id::new("probe"))
            .into()
    }
}

type QueryFuture = Pin<Box<dyn Future<Output = Result<Snapshot, Error>>>>;

/// Polls `query` once so it sends its request, then drives the emulator
/// until the query resolves.
fn drive(
    emulator: &mut Emulator<App>,
    app: &App,
    receiver: &mut mpsc::Receiver<emulator::Event<App>>,
    query: &mut QueryFuture,
    context: &mut Context<'_>,
) -> Result<Snapshot, Error> {
    for _ in 0..64 {
        if let Some(event) = block_on(receiver.next()) {
            match event {
                emulator::Event::Action(action) => emulator.perform(app, action),
                emulator::Event::Failed(_) => panic!("no instruction should run"),
                emulator::Event::Ready => {}
            }
        }
        match query.as_mut().poll(context) {
            Poll::Ready(result) => return result,
            Poll::Pending => {}
        }
    }
    panic!("the query did not complete");
}

fn harness() -> (App, Emulator<App>, mpsc::Receiver<emulator::Event<App>>) {
    let (handle, task) = channel::<Msg>(
        vec![Target::new("probe", widget::Id::new("probe"))],
        Limits::new(),
    )
    .expect("targets are valid");

    let app = App {
        handle,
        task: Mutex::new(Some(task)),
        updates: AtomicUsize::new(0),
    };

    let (sender, receiver) = mpsc::channel(100);
    let emulator = Emulator::new(
        sender,
        &app,
        emulator::Mode::Immediate,
        Size::new(800.0, 600.0),
    );

    (app, emulator, receiver)
}

#[test]
fn a_query_reads_real_geometry_without_an_application_message() {
    let (app, mut emulator, mut receiver) = harness();
    let waker = noop_waker_ref();
    let mut context = Context::from_waker(waker);

    let mut query = Box::pin(app.handle.query(Request::new(Window::Only)));
    assert!(matches!(query.as_mut().poll(&mut context), Poll::Pending));
    let snapshot = drive(&mut emulator, &app, &mut receiver, &mut query, &mut context)
        .expect("the query succeeds");

    assert_eq!(snapshot.layer, Layer::Base);
    assert_eq!(snapshot.logical_size, Size::new(800.0, 600.0));
    assert_eq!(
        snapshot.aliases,
        vec![AliasResult {
            alias: "probe".to_owned(),
            status: AliasStatus::Found,
        }]
    );

    let record = snapshot
        .records
        .iter()
        .find(|record| record.kind == Kind::Container)
        .expect("the container candidate is recorded");
    assert_eq!(
        record.layout_bounds,
        Rectangle::new(Point::ORIGIN, Size::new(800.0, 600.0))
    );
    assert_eq!(
        record.visible_bounds,
        Some(Rectangle::new(Point::ORIGIN, Size::new(800.0, 600.0)))
    );
    // No application message was produced for the request or its response.
    assert_eq!(app.updates.load(Ordering::SeqCst), 0);
}

#[test]
fn retained_queries_preserve_tree_geometry() {
    let (app, mut emulator, mut receiver) = harness();
    let waker = noop_waker_ref();
    let mut context = Context::from_waker(waker);

    let mut first = Box::pin(app.handle.query(Request::new(Window::Only)));
    assert!(matches!(first.as_mut().poll(&mut context), Poll::Pending));
    let first =
        drive(&mut emulator, &app, &mut receiver, &mut first, &mut context).expect("query");

    let mut second = Box::pin(app.handle.query(Request::new(Window::Only)));
    assert!(matches!(second.as_mut().poll(&mut context), Poll::Pending));
    let second = drive(&mut emulator, &app, &mut receiver, &mut second, &mut context)
        .expect("query");

    assert_eq!(first, second);
    assert_eq!(app.updates.load(Ordering::SeqCst), 0);
}

#[test]
fn an_unlaid_out_overlay_is_not_ready() {
    let (app, mut emulator, mut receiver) = harness();
    let waker = noop_waker_ref();
    let mut context = Context::from_waker(waker);

    let mut query = Box::pin(
        app.handle
            .query(Request::new(Window::Only).layer(Layer::Overlay)),
    );
    assert!(matches!(query.as_mut().poll(&mut context), Poll::Pending));
    let result = drive(&mut emulator, &app, &mut receiver, &mut query, &mut context);

    assert_eq!(result, Err(Error::NotReady));
    assert_eq!(app.updates.load(Ordering::SeqCst), 0);
}

#[test]
fn one_query_in_flight_and_closing_resolve_the_slot() {
    let (app, mut emulator, mut receiver) = harness();
    let waker = noop_waker_ref();
    let mut context = Context::from_waker(waker);

    // A pending query occupies the single slot; a second is busy, even
    // before the first is serviced.
    let mut first = Box::pin(app.handle.query(Request::new(Window::Only)));
    assert!(matches!(first.as_mut().poll(&mut context), Poll::Pending));
    let second = block_on(app.handle.query(Request::new(Window::Only)));
    assert_eq!(second, Err(Error::Busy));

    // The first query still completes afterwards.
    drive(&mut emulator, &app, &mut receiver, &mut first, &mut context).expect("query");

    // Unknown aliases fail with a reply, not a hang.
    let mut bad = Box::pin(
        app.handle
            .query(Request::new(Window::Only).aliases(["nope".to_owned()])),
    );
    assert!(matches!(bad.as_mut().poll(&mut context), Poll::Pending));
    let result = drive(&mut emulator, &app, &mut receiver, &mut bad, &mut context);
    assert_eq!(result, Err(Error::UnknownAlias("nope".to_owned())));

    // Closing rejects new queries.
    app.handle.close();
    let mut closed = Box::pin(app.handle.query(Request::new(Window::Only)));
    assert_eq!(
        closed.as_mut().poll(&mut context),
        Poll::Ready(Err(Error::Closed))
    );
}

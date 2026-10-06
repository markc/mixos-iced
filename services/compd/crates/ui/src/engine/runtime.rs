//! `IcedRuntime<U>`: one Iced UI instance, decoupled from any window system.
//!
//! Generalized from the layer-shell-backed `IcedDriver` in the reference
//! integration. Differences:
//!
//! - No `wgpu::Surface`. We render into a caller-supplied `wgpu::TextureView`
//!   per frame.
//! - No Wayland. Events come in pre-translated as `iced_core::Event`.
//! - Generic over the UI type `U: IcedUi`. The runtime statically knows
//!   `U::Message`, so message dispatch is type-safe end-to-end.
//! - The wgpu device/queue/Engine/Renderer are shared via `SharedEngine`,
//!   not owned per-instance.

use std::sync::Arc;

use crate::engine::shared::SharedEngine;
use crate::engine::ui::IcedUi;
use iced_core::renderer::Style;
use iced_core::time::Instant;
use iced_core::window::{self, RedrawRequest};
use iced_core::{Color, Event as IcedEvent, Size, mouse};
use iced_runtime::user_interface::{Cache, State, UserInterface};
use iced_wgpu::graphics::Viewport;

/// Per-instance message handler. The compositor can install one of these
/// to observe messages flowing out of the UI (for custom protocol dispatch,
/// logging, etc.) without intercepting `update`.
///
/// Called *before* `ui.update(message)`. The handler sees every message and
/// can use it for side effects, but cannot prevent `update` from running or
/// modify the message.
pub trait MessageHandler<M>: Send + 'static {
    fn handle(&mut self, message: &M);
}

/// The colour each frame is cleared to before iced draws: transparent, or
/// opaque red when `COMPD_ICED_DEBUG_CLEAR` is set (read once). A diagnosis
/// switch: with it set, a compd-owned iced surface that shows red has its
/// clear land in the texture the compositor samples while iced's primitives
/// do not; one that stays black never reaches that texture at all.
fn clear_colour() -> Color {
    static DEBUG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *DEBUG.get_or_init(|| std::env::var_os("COMPD_ICED_DEBUG_CLEAR").is_some()) {
        Color::from_rgb(1.0, 0.0, 0.0)
    } else {
        Color::TRANSPARENT
    }
}

impl<M, F> MessageHandler<M> for F
where
    F: FnMut(&M) + Send + 'static,
{
    fn handle(&mut self, message: &M) {
        (self)(message)
    }
}

fn dispatch_messages<U: IcedUi>(
    ui: &mut U,
    handler: &mut Option<Box<dyn MessageHandler<U::Message>>>,
    messages: Vec<U::Message>,
) {
    let mut messages: std::collections::VecDeque<_> = messages.into();
    while let Some(message) = messages.pop_front() {
        if let Some(handler) = handler.as_mut() {
            handler.handle(&message);
        }
        ui.update(message.clone());
        messages.extend(ui.process(&message));
    }
}

/// One Iced UI instance.
///
/// Generic over `U: IcedUi`. Holds the UI state itself, the Iced render
/// caches (layout cache + viewport + cursor), and event/message queues.
///
/// ## Lifecycle
///
/// - `new`: construct with a `SharedEngine`, a `U`, and an initial size.
/// - Each tick of your loop:
///   1. (Compositor) Call `queue_event` and/or `queue_message` for any
///      input that's arrived since the last tick.
///   2. Call `tick()` — drains queues, runs `view`+`update`+`draw`'s
///      precursors, returns true if anything changed.
///   3. If `tick()` returned true OR `is_dirty()` returns true, call
///      `render_into(&texture_view)` to paint the latest frame.
/// - `resize`: change the logical viewport. The caller is responsible for
///   also reallocating the underlying `IcedSurface` (crate 1) — this
///   method only updates Iced's view of the size.
pub struct IcedRuntime<U: IcedUi> {
    /// The UI state. Pub to allow direct mutation when needed (e.g.,
    /// snapshot updates that bypass the message channel).
    pub ui: U,

    /// Cached layout/text-shaping work, persisted across frames.
    cache: Cache,
    /// Logical/physical size + scale factor.
    viewport: Viewport,
    /// Last known cursor position. Updated by `queue_event` so `tick` and
    /// `render_into` see consistent state.
    cursor: mouse::Cursor,

    /// Events pending dispatch on next `tick`.
    queued_events: Vec<IcedEvent>,
    /// Messages pending application on next `tick`.
    queued_messages: Vec<U::Message>,

    /// Reference to the shared wgpu/iced_wgpu resources.
    engine: SharedEngine,

    /// Optional per-instance message handler.
    message_handler: Option<Box<dyn MessageHandler<U::Message>>>,

    /// Per-instance dirty bit. Independently tracked from the global
    /// `SharedEngine` dirty flags, so each instance redraws only its own
    /// activity.
    dirty: bool,

    /// What iced asked us to redraw next, captured from the last
    /// `UserInterface::update`. `Wait` means idle; `NextFrame`/`At(_)` means a
    /// time-based animation is in progress and wants more frames. The runtime
    /// keeps injecting `RedrawRequested(now)` and reporting dirty until a
    /// widget settles back to `Wait`. Previously this value was discarded, so
    /// animations never advanced.
    redraw_request: RedrawRequest,
    wake: super::wake::RuntimeWake,
}

impl<U: IcedUi> std::fmt::Debug for IcedRuntime<U> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IcedRuntime")
            .field("viewport", &self.viewport.logical_size())
            .field("queued_events", &self.queued_events.len())
            .field("queued_messages", &self.queued_messages.len())
            .field("dirty", &self.dirty)
            .finish()
    }
}

impl<U: IcedUi> IcedRuntime<U> {
    /// Create a new runtime.
    ///
    /// `size` is in physical pixels. `scale_factor` is 1.0 for the single-output
    /// case (per spec); pass a real value if you wire up multi-output later.
    pub fn new(ui: U, engine: SharedEngine, size_px: (u32, u32), scale_factor: f32) -> Self {
        // iced now splits scale into the OS window factor and an application
        // factor. This `scale_factor` is the window one; the compositor applies
        // its own zoom through the camera, not through iced.
        let viewport = Viewport::with_physical_size(
            Size::new(size_px.0.max(1), size_px.1.max(1)),
            iced_core::renderer::Scale { window: scale_factor, application: 1.0 },
        );

        Self {
            ui,
            cache: Cache::default(),
            viewport,
            cursor: mouse::Cursor::Unavailable,
            queued_events: Vec::new(),
            queued_messages: Vec::new(),
            engine,
            message_handler: None,
            // Force the first render even if no events arrive — otherwise
            // a freshly-created instance shows nothing until the user
            // interacts with it.
            dirty: true,
            redraw_request: RedrawRequest::Wait,
            wake: super::wake::RuntimeWake::new(),
        }
    }

    /// Install a message handler. Replaces any prior handler.
    pub fn set_message_handler<H: MessageHandler<U::Message>>(&mut self, handler: H) {
        self.message_handler = Some(Box::new(handler));
    }

    /// Remove the installed message handler.
    pub fn clear_message_handler(&mut self) {
        self.message_handler = None;
    }

    // ── Ingest ────────────────────────────────────────────────────────

    /// Queue an Iced event for dispatch on the next `tick`.
    ///
    /// Cursor positions are tracked internally for `Cursor::Available` /
    /// `Cursor::Unavailable` state, so callers don't need to track separately.
    pub fn queue_event(&mut self, event: IcedEvent) {
        self.dirty |= super::frame::visual_input(&event, self.cursor);
        if let IcedEvent::Mouse(mouse::Event::CursorMoved { position }) = &event {
            self.cursor = mouse::Cursor::Available(*position);
        }
        if matches!(event, IcedEvent::Mouse(mouse::Event::CursorLeft)) {
            self.cursor = mouse::Cursor::Unavailable;
        }
        self.queued_events.push(event);
    }

    /// Whether events or messages wait for the next `tick`. Only a frame ticks
    /// the runtime, so the host owes one while this is true (input reaches a
    /// compositor-owned surface without anything else asking for a frame when
    /// the cursor is the host's).
    pub fn has_queued(&self) -> bool {
        self.wake.pending() || !self.queued_events.is_empty() || !self.queued_messages.is_empty()
    }

    /// Queue a UI message for application on the next `tick`. Bypasses the
    /// widget tree; useful for compositor-originated state updates.
    pub fn queue_message(&mut self, message: U::Message) {
        self.queued_messages.push(message);
    }

    // ── Tick ──────────────────────────────────────────────────────────

    /// Drain queues; run a build/update cycle.
    ///
    /// Returns `true` if anything changed and a render is wanted. The
    /// returned `true` also corresponds to "the cache was updated"; the
    /// caller should call `render_into` before the next tick if `true`.
    ///
    /// Order of operations (this is contract — preserve when modifying):
    ///   1. If both queues are empty, return false immediately.
    ///   2. Take ownership of the queues + cache, build a `UserInterface`.
    ///   3. `UserInterface::update` with the events; collects any messages
    ///      the UI produced into `messages`.
    ///   4. Drop the `UserInterface` (releases the immutable borrow of
    ///      `self.ui`).
    ///   5. For each message:
    ///      a. Call the installed `message_handler` if present.
    ///      b. Call `ui.update(message)`.
    ///   6. Set `self.dirty = true` to ensure a render happens.
    pub fn tick(&mut self) -> bool {
        if self.wake.take() {
            self.queued_events.push(IcedEvent::Waken);
            self.dirty = true;
        }
        if self.engine.global_dirty.take_invalidate() {
            self.cache = Cache::default();
            self.dirty = true;
        }
        let now = Instant::now();
        // A time-based animation frame is "due" when iced asked for the next
        // frame, or the instant it asked for has arrived. The `RedrawRequested`
        // event pushed below is the ONLY channel by which iced widgets receive
        // "now" and advance an animation — without it, re-rendering just
        // repaints the same frame.
        let animation_due = match self.redraw_request {
            RedrawRequest::NextFrame => true,
            RedrawRequest::At(at) => now >= at,
            RedrawRequest::Wait => false,
        };

        if self.queued_events.is_empty() && self.queued_messages.is_empty() && !animation_due {
            // Nothing to process this frame. A render is wanted only for real
            // dirt: an animation waiting on a FUTURE instant is not due, and
            // reporting it here re-rasterised the same frame every vblank until
            // the instant arrived. The host wakes for that
            // instant from `deadline()` instead.
            return self.dirty || self.engine.global_dirty.redraw_pending();
        }

        let bounds = self.viewport.logical_size();
        let bounds = Size::new(bounds.width, bounds.height);

        let mut events = std::mem::take(&mut self.queued_events);
        let mut messages = std::mem::take(&mut self.queued_messages);

        // Feed the current time into the widget tree so time-based animations
        // advance. Pushed before phase 0/1 so the event flows through the same
        // update cycle as everything else.
        if animation_due {
            events.push(IcedEvent::Window(window::Event::RedrawRequested(now)));
        }

        
        // ── Phase 0: subscribe + event_process ─────────────────────────
        //
        // Ask the UI which event categories it cares about. For every
        // matching event, run `event_process` and prepend the returned
        // messages so they're processed alongside whatever was already in
        // `queued_messages`.
        {
            let flags = self.ui.subscribe();
            if !flags.is_empty() {
                for event in &events {
                    if flags.matches(event) {
                        messages.extend(self.ui.event_process(event));
                    }
                }
            }
        }
        // Events still flow into phase 1 as before — iced widgets see them.

        // ── Phase 1: build UI, collect widget-produced msgs, and capture the
        // returned `State` (previously discarded) so we know what iced wants to
        // redraw next — the basis for driving animation.
        let (new_cache, state) = {
            let mut renderer_guard = self.engine.renderer_borrow();
            let cache = std::mem::take(&mut self.cache);
            let mut ui = UserInterface::build(
                self.ui.view(),
                bounds,
                cache,
                &mut renderer_guard,
            );
            // The surface is headless, but widget work may finish on another
            // thread. Its live waker records Waken and pings the compositor.
            let mut bus = iced_core::shell::Bus::new();
            for message in messages.drain(..) {
                let _ = bus.push(message);
            }
            let (state, _statuses) = ui.update(
                &iced_core::window::Headless,
                self.wake.waker(),
                &events,
                self.cursor,
                &mut renderer_guard,
                &mut bus,
            );
            messages.extend(bus.drain());
            (ui.into_cache(), state)
        };
        self.cache = new_cache;

        // Pointer/touch damage was recorded by queue_event: transient widget
        // status is rebuilt here and cannot compare with its last drawn status.
        // Other event delivery alone is not damage; reducers, widget requests
        // and due animations decide whether their pixels changed.
        let changed = !messages.is_empty() || animation_due || match &state {
            State::Outdated => true,
            State::Updated { redraw_request, .. } => !matches!(redraw_request, RedrawRequest::Wait),
        };

        // Record what iced wants next. `Outdated` => the cache is stale and a
        // rebuild is needed (ask for a frame). `Updated` carries the widgets'
        // redraw request, which is what keeps an animation going.
        let drew = events.iter().any(|event| {
            matches!(event, IcedEvent::Window(window::Event::RedrawRequested(_)))
        });
        self.redraw_request = super::frame::after_update(self.redraw_request, state, now, drew);

        // ── Phase 2: handler + reducer + process derivations ───────────
        //
        // `messages` becomes a VecDeque so `process()` follow-ups can be
        // pushed onto the back while we pop from the front. The handler
        // sees every message, including derived ones.
        dispatch_messages(&mut self.ui, &mut self.message_handler, messages);

        self.dirty |= changed;
        self.dirty || self.engine.global_dirty.redraw_pending()
        //

        //
        // // Phase 1: scoped block so the UserInterface (and the immutable
        // // borrow of self.ui via view()) is dropped before phase 2.
        // let new_cache = {
        //     let mut renderer_guard = self.engine.renderer_borrow();
        //     let cache = std::mem::take(&mut self.cache);
        //     let mut ui = UserInterface::build(self.ui.view(), bounds, cache, &mut renderer_guard);
        //
        //     let (_state, _statuses) =
        //         ui.update(&events, self.cursor, &mut renderer_guard, &mut messages);
        //
        //     ui.into_cache()
        // };
        // self.cache = new_cache;
        //
        // // Phase 2: mutate self.ui.
        // // for message in messages.drain(..) {
        // //     if let Some(handler) = self.message_handler.as_mut() {
        // //         handler.handle(&message);
        // //     }
        // //     self.ui.update(message);
        // // }
        //
        // let mut messages: std::collections::VecDeque<_> = messages.into();
        // while let Some(message) = messages.pop_front() {
        //     if let Some(handler) = self.message_handler.as_mut() {
        //         handler.handle(&message);
        //     }
        //     self.ui.update(message.clone());
        //     for follow_up in self.ui.process(&message) {
        //         messages.push_back(follow_up);
        //     }
        // }
        //
        //
        // self.dirty = true;
        // true
    }

    // ── Render ────────────────────────────────────────────────────────

    /// Render the current UI state into the given texture view.
    ///
    /// `view` must reference a texture whose format matches
    /// `engine.target_format`. The texture's size must match
    /// `self.viewport`'s physical size (caller's responsibility — this is
    /// why `resize` updates both the viewport and the underlying texture
    /// allocation in lockstep).
    ///
    /// Clears the per-instance and engine-global dirty flags. Idempotent
    /// if called twice without state change (Iced redraws to the same
    /// pixels, no harm done).
    pub fn render_into(&mut self, view: &wgpu::TextureView) {
        // Consume the notification this frame answers BEFORE drawing. A worker
        // notification racing the render remains pending for the frame it wakes.
        self.engine.global_dirty.take_redraw();
        let bounds = self.viewport.logical_size();
        let bounds = Size::new(bounds.width, bounds.height);

        let mut renderer_guard = self.engine.renderer_borrow();

        let mut cache = std::mem::take(&mut self.cache);
        let now = Instant::now();
        let mut attempt = 0;
        loop {
            let mut ui = UserInterface::build(self.ui.view(), bounds, cache, &mut renderer_guard);

            // Re-establish the overlay layout before drawing. `UserInterface::draw`
            // only renders an overlay (pick_list / combo_box dropdown, `tooltip`
            // widget) when its `overlay` field is populated — and that field is set
            // by `update`, NOT carried in the `Cache` (which holds only the widget
            // tree). We build a FRESH `UserInterface` here, separate from the one
            // `tick()` updated, so without this call `self.overlay` is `None` and
            // `draw` early-returns: an *open* dropdown would never appear. The
            // fresh view also needs RedrawRequested to establish hover/pressed
            // styles; these live in the widget, not in Cache. Apply frame messages
            // before painting and retry at the SAME instant, so an on_redraw
            // callback does not perpetually generate another frame.
            let mut messages = iced_core::shell::Bus::new();
            let state = super::frame::prepare(
                &mut ui, self.cursor, &mut *renderer_guard, self.wake.waker(),
                &mut messages, now,
            );
            let messages: Vec<_> = messages.drain().collect();
            let pass = super::frame::next_pass(attempt, &state, !messages.is_empty());
            self.redraw_request = super::frame::after_update(self.redraw_request, state, now, true);
            match pass {
                super::frame::Pass::Retry => {
                    cache = ui.into_cache();
                    dispatch_messages(&mut self.ui, &mut self.message_handler, messages);
                    attempt += 1;
                    continue;
                }
                super::frame::Pass::Defer => {
                    // Keep the final pass's messages for the next tick and paint
                    // now. Continuous publishers/invalidators must yield.
                    self.queued_messages.extend(messages);
                    self.redraw_request = RedrawRequest::NextFrame;
                    super::wake::notify();
                }
                super::frame::Pass::Draw => {}
            }

            ui.draw(
                &mut renderer_guard,
                &self.ui.theme(),
                &Style {
                    text_color: Color::WHITE,
                },
                self.cursor,
            );

            self.cache = ui.into_cache();
            break;
        }

        renderer_guard.present(
            Some(clear_colour()),
            self.engine.target_format,
            view,
            &self.viewport,
        );

        // iced_wgpu's staging belt requires a poll between frames to recall its
        // buffers. With a wgpu::Surface, the surface present implicitly does
        // this; we render to a plain texture view, so we must poll ourselves.
        //
        // Non-blocking; just drains any work the device has completed since the
        // last poll. Safe to call every frame even when nothing is in flight.
        // let _ = self.engine.device.poll(wgpu::PollType::Poll);
        // Force submit. If present already submitted, this is a no-op submit
        // of an empty encoder list, which is harmless. If present didn't submit,
        // this is what makes the GPU actually run the commands.
        // self.engine.queue.submit(std::iter::empty());
        // self.engine.queue.submit(std::iter::empty());
        // self.engine.device.poll(PollType::Wait{
        //     submission_index: None,
        //     timeout: None,
        // });  // block until GPU is done
        // Renderer::present submits its command encoder to the queue
        // internally on most iced_wgpu versions. We don't call queue.submit
        // ourselves — iced does it.

        self.dirty = false;
    }

    // ── State ──────────────────────────────────────────────────────────

    /// Whether a frame is wanted NOW: dirty, or an animation frame that is due
    /// (`NextFrame`, or an `At` instant that has arrived). An animation waiting
    /// on a later instant is not dirty; [`Self::deadline`] says when it will be,
    /// so the host can sleep until then instead of spinning a frame per vblank.
    pub fn is_dirty(&self) -> bool {
        self.wake.pending() || self.dirty || self.engine.global_dirty.redraw_pending() || self.animation_due(Instant::now())
    }

    /// The animation frame iced asked for has arrived at `now`.
    pub fn animation_due(&self, now: Instant) -> bool {
        match self.redraw_request {
            RedrawRequest::NextFrame => true,
            RedrawRequest::At(at) => now >= at,
            RedrawRequest::Wait => false,
        }
    }

    /// The future instant iced asked to be redrawn at, if any. `None` both when
    /// idle and when a frame is already due (see [`Self::is_dirty`]).
    pub fn deadline(&self) -> Option<Instant> {
        match self.redraw_request {
            RedrawRequest::At(at) if at > Instant::now() => Some(at),
            _ => None,
        }
    }

    /// True while a time-based animation is in progress: iced asked for a
    /// future redraw (via `shell.request_redraw[_at]` inside some widget) and
    /// hasn't settled back to `Wait`. The host should keep ticking + rendering
    /// this instance until this returns false.
    pub fn is_animating(&self) -> bool {
        !matches!(self.redraw_request, RedrawRequest::Wait)
    }

    /// The redraw iced asked for after the last tick. Exposed for a host that
    /// wants to honor `At(Instant)` precisely (sleep until then) rather than
    /// re-tick every frame; the default dirty-driven loop does not consult it.
    pub fn next_redraw(&self) -> RedrawRequest {
        self.redraw_request
    }

    /// Force a redraw on the next render pass without queueing any event.
    pub fn request_redraw(&mut self) {
        self.dirty = true;
    }

    /// Acknowledge a frame WITHOUT rasterizing: clear the dirty flag so an
    /// off-screen surface stops reporting `is_dirty()` (and thus stops pinning
    /// the host's redraw loop) even though we never called `render_into`.
    ///
    /// Unlike `render_into`, this deliberately does NOT touch
    /// `engine.global_dirty` — a global redraw request belongs to whichever
    /// instance raised it and must still be honored by an on-screen render.
    /// `tick()` continues to advance animations/messages independently, and
    /// `is_animating()` still keeps the loop alive while an animation is live,
    /// so nothing driven by the runtime is frozen — only pixel output is
    /// skipped. The caller must re-render when the surface becomes visible
    /// (the registry tracks this with a per-item `stale` flag).
    pub fn acknowledge_frame(&mut self) {
        self.dirty = false;
    }

    /// Drop the cached layout/text-shaping work. Forces a fresh build on
    /// the next tick. Use sparingly — Iced's cache is the whole point of
    /// the deferred runtime.
    pub fn invalidate_layout(&mut self) {
        self.cache = Cache::default();
        self.dirty = true;
    }

    // ── Resize ─────────────────────────────────────────────────────────

    /// Update the viewport size. Caller must also reallocate the underlying
    /// `IcedSurface` (crate 1) — this method only changes Iced's bookkeeping.
    pub fn resize(&mut self, new_size_px: (u32, u32), scale_factor: f32) {
        self.viewport = Viewport::with_physical_size(
            Size::new(new_size_px.0.max(1), new_size_px.1.max(1)),
            iced_core::renderer::Scale { window: scale_factor, application: 1.0 },
        );

        // Layout depends on size; invalidate.
        self.invalidate_layout();
    }

    pub fn physical_size(&self) -> Size<u32> {
        self.viewport.physical_size()
    }

    pub fn scale_factor(&self) -> f32 {
        self.viewport.scale_factor()
    }

    // ── Read-back ──────────────────────────────────────────────────────

    /// Run a widget `operation` over the current view at the current size:
    /// layout read-back (widget bounds by id, in logical px from the
    /// instance's top-left). Builds from the cache and draws nothing; the
    /// state it reads is what the last `tick` applied.
    pub fn operate(&mut self, operation: &mut dyn iced_core::widget::Operation) {
        let bounds = self.viewport.logical_size();
        let mut renderer_guard = self.engine.renderer_borrow();
        let cache = std::mem::take(&mut self.cache);
        let mut ui = UserInterface::build(
            self.ui.view(),
            Size::new(bounds.width, bounds.height),
            cache,
            &mut renderer_guard,
        );
        ui.operate(&renderer_guard, operation);
        self.cache = ui.into_cache();
    }
}

/// Helper: borrow the wgpu queue from the runtime for explicit `submit` calls.
/// Most users won't need this — `render_into` handles submission internally.
pub fn queue_of<U: IcedUi>(rt: &IcedRuntime<U>) -> &Arc<wgpu::Queue> {
    &rt.engine.queue
}

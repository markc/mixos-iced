//! The winit frame path. Pass presence comes from the frame plan,
//! callbacks/housekeeping from `frames::draw::present::callbacks`; nothing is
//! duplicated between backends.

use frames::draw::plan::frame::frame::{plan, FramePass};
use frames::draw::plan::tap::tap::{TapSubscriptions, POST_SCENE};
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::{Element, RenderElement};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::{Bind, Color32F, Frame, ImportDma, Renderer};
use smithay::backend::winit::WinitGraphicsBackend;
use smithay::desktop::Window;
use smithay::output::Output;
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::utils::{Physical, Rectangle, Scale, Size, Transform};
use dispatcher::frame::frame::SceneDispatch;
use frames::scene::scene::Scene;
use world::state::Loop;
use graphics::capture::registry::{CaptureRegistry, OutputId};

/// Keep the damage tracker at the output's current mode and scale, rendering
/// through Flipped180: the window surface is a GL default framebuffer (row 0
/// is the picture's bottom). The output itself advertises Transform::Normal
/// (window factory), so the flip lives here, in a STATIC tracker rebuilt only
/// when the size or scale moves (a resize repaints in full anyway).
fn sync_tracker(tracker: &mut OutputDamageTracker, output: &Output) {
    let size = output.current_mode().map(|mode| mode.size).unwrap_or_default();
    let scale = output.current_scale().fractional_scale();
    let current = matches!(
        tracker.mode(),
        smithay::output::OutputModeSource::Static { size: s, scale: k, transform: Transform::Flipped180 }
            if *s == size && k.x == scale && k.y == scale
    );
    if !current {
        *tracker = OutputDamageTracker::new(size, scale, Transform::Flipped180);
    }
}

/// The winit render context (ex winit.draw/draw.context, folded into the
/// scene composer that owns it).
pub struct WinitRenderContext {
    pub display_handle: DisplayHandle,
    pub output: Output,
    pub winit_backend: WinitGraphicsBackend<GlesRenderer>,
    pub damage_tracker: OutputDamageTracker,
    /// Taps fire only for active subscribers; registry presence IS the
    /// subscription (set when the capture registry initializes).
    pub tap_subscriptions: TapSubscriptions,
    /// The windows whose frame callbacks an EMPTY frame owes: sent
    /// by a one-shot timer one refresh later, so a client committing without
    /// damage is paced like a real frame instead of looping on its callback.
    pub owed_frames: Option<Vec<Window>>,
    /// This context, for that timer (set by `wire` once the context is shared).
    pub this: std::rc::Weak<std::cell::RefCell<WinitRenderContext>>,
    /// Consecutive failed swaps. While nonzero the next frame is rendered in
    /// full (buffer age 0): what the back buffers hold after a failed swap is
    /// unknown. Logged on the first failure and on recovery, not per frame.
    pub swap_failures: u64,
    /// Consecutive failed renders (`render_output` errors: a shader uniform, a
    /// lost texture). Like `swap_failures`: logged on the first and on
    /// recovery, the next frame rendered in full, never a panic.
    pub render_failures: u64,
}

/// One explicit file-capture scene on the redraw ping. Does not depend on the host's
/// window frame callback (withheld for hidden/minimised Wayland windows).
pub fn capture_offscreen(state: &mut Loop, context: &mut WinitRenderContext) {
    if !screencopy::file::pending(&context.output) {
        return;
    }
    let scale = context.output.current_scale().fractional_scale();
    screencopy::offscreen::capture_windows(context.winit_backend.renderer(), &context.output, |renderer, target| {
        let window = world::window::draw::frame::scene::capture_window(state, target.id, target.generation)
            .map_err(|error| screencopy::file::ControlReply::WindowTarget { id: target.id, error })?;
        world::window::draw::frame::scene::capture(renderer, &window, scale)
            .map_err(screencopy::file::capture_failed)
    });
    if !screencopy::file::pending_output(&context.output) {
        if let Err(err) = crate::frame::submit::submit::ensure_surface_current(&mut context.winit_backend) {
            warn!("winit: restore window after capture failed ({err})");
        }
        return;
    }
    let size = context.output.current_mode().map(|mode| mode.size)
        .unwrap_or_else(|| context.winit_backend.window_size());
    if size.w <= 0 || size.h <= 0 || !scale.is_finite() || scale <= 0.0 {
        screencopy::file::fail_output(&context.output, "output cannot be rendered");
        return;
    }
    let previous_output = state.inner.render_output.clone();
    state.inner.render_output = Some(world::state::state::output_key(&context.output));
    let renderer = context.winit_backend.renderer();
    let prepared = frames::scene::scene::prepare(state, renderer, size);
    let scene = frames::scene::scene::scene(state, renderer, size, prepared);
    screencopy::offscreen::capture(
        renderer, &scene.Element, |element| element.is_cursor(), &context.output, size, scale, false,
    );
    state.inner.render_output = previous_output;
    // The next window frame's age query and swap require its EGL surface.
    if let Err(err) = crate::frame::submit::submit::ensure_surface_current(&mut context.winit_backend) {
        warn!("winit: restore window after capture failed ({err})");
    }
}

/// One nested frame, if one is owed.
///
/// Nothing renders unless the redraw schedule says this output is behind a
/// request: winit's own redraws (a host expose, a frame callback) do not draw by
/// themselves, and nothing here asks for the next frame unconditionally. A
/// request reaches winit through the schedule's ping (`wire` maps it to
/// `request_redraw`), which winit holds until the host's frame callback.
///
/// Damage comes from the tracker with the EGL buffer age, and the host gets
/// only that damage: a frame with nothing new is not submitted at all.
pub fn draw(state: &mut Loop, context: &mut WinitRenderContext) {
    // Tag the output being drawn so per-output consumers (e.g. the FPS overlay's
    // hook, run inside `compose`) resolve the same key the present bump uses.
    let output_key = world::state::state::output_key(&context.output);
    if !state.state.redraw.needs(&output_key) {
        return;
    }
    // The ced gate's stall check reads each frame update as a `comp_update`
    // span; this is the nested frame, one span over the compose, submit and
    // present below.
    let _frame = ledger::frame_trace::span("comp_update", 0);
    // Current as of NOW: a request made while this renders leaves the output
    // behind and is serviced by the redraw requested below.
    state.state.redraw.rendering(&output_key);
    state.inner.render_output = Some(output_key.clone());

    // Round-1 finding B: a request made inside the render (capture polls,
    // effects) must not ping at once — the decision below paces it.
    state.state.redraw.begin_render();
    let (damage, visible, captures, offscreen) = compose(context, state);
    let mut submitted = damage.is_some();
    // A cursorless screencopy render left an offscreen texture current; the swap
    // needs the window surface current again (EGL_BAD_SURFACE otherwise).
    if offscreen {
        if let Err(err) = crate::frame::submit::submit::make_surface_current(&mut context.winit_backend) {
            warn!("winit: window surface not made current after an offscreen render ({err})");
        }
    }
    match damage {
        Some(damage) => {
            match crate::frame::submit::submit::submit(&mut context.winit_backend, &damage) {
                Ok(()) => {
                    if context.swap_failures > 0 {
                        info!("winit: swap recovered after {} failed frames", context.swap_failures);
                        context.swap_failures = 0;
                    }
                    // A submitted frame: the ledger attributes it to what was pending.
                    state.state.redraw.frame(&output_key, false);
                    present(context, state, visible);

                    // One presented frame on this output (winit vsyncs to the host compositor).
                    model::stats::registry::base::present(&output_key);

                    // READY=1 once the first frame has been
                    // swapped to the host. A no-op after the first call and outside systemd.
                    if slots::sdnotify::ready_once("first frame presented (nested)") {
                        info!("sd_notify: READY=1 after the first nested frame");
                    }
                }
                Err(err) => {
                    // Nothing was shown. Repaint in full on a later frame, paced like
                    // an empty one (the wake below): never a spin, never a panic.
                    submitted = false;
                    if context.swap_failures == 0 {
                        warn!("winit: swap failed ({err}); repainting in full");
                    }
                    context.swap_failures += 1;
                    state.state.redraw.request_for(
                        protocols::redraw::schedule::schedule::RedrawReason::Rescue,
                    );
                    state.state.redraw.frame(&output_key, true);
                    owe_frames(context, state, visible);
                }
            }
        }
        None => {
            // Rendered (or skipped by the tracker) with nothing to show.
            state.state.redraw.frame(&output_key, true);
            owe_frames(context, state, visible);
        }
    }
    // wlr-screencopy: write the readbacks this frame started, now that it has
    // been swapped. Mapping them makes the GL context current WITHOUT the window
    // surface, which must not happen before the swap (EGL_BAD_SURFACE), and is
    // restored by the next frame's `ensure_surface_current` (compose), like any
    // other surfaceless use between frames.
    if let Some(captures) = captures.filter(|c| !c.is_empty()) {
        captures.finish(context.winit_backend.renderer());
    }

    // The held wake (if any) is answered by the `needs` check below, which already
    // chooses between the host-paced redraw and a vblank-paced wake.
    let _ = state.state.redraw.end_render();
    // A request made during this frame — a silent continuation from inside the
    // render (iced, effects) included — still owes a frame. After a submit,
    // winit holds the redraw until the host's frame callback. After an empty
    // frame nothing would pace it, so wake one refresh later instead: a source
    // that asks every frame must not spin the loop on frames that come out empty.
    if state.state.redraw.needs(&output_key) {
        if submitted {
            crate::frame::submit::submit::request_redraw(&mut context.winit_backend);
        } else {
            let handle = state.loop_handle.clone();
            state.state.redraw.wake_at(&handle, std::time::Instant::now() + refresh_of(context), schedule_of);
        }
    }
    // A producer that needs a frame at a later instant (an iced
    // animation's `RedrawRequest::At`) offered it during this frame.
    if let Some(at) = graphics::bridge::publish::wake::wake::take_deadline() {
        let handle = state.loop_handle.clone();
        state.state.redraw.request_at(
            &handle,
            at,
            protocols::redraw::schedule::schedule::RedrawReason::Iced,
            schedule_of,
        );
    }
    // Capture's time-based keep-alive (round-1 finding D): its own deadline, its
    // own reason.
    if let Some(at) = graphics::bridge::publish::wake::wake::take_capture_deadline() {
        let handle = state.loop_handle.clone();
        state.state.redraw.request_at(
            &handle,
            at,
            protocols::redraw::schedule::schedule::RedrawReason::Capture,
            schedule_of,
        );
    }
}

/// Where the redraw schedule lives in the loop data, for its deadline timers.
fn schedule_of(state: &mut Loop) -> &mut protocols::redraw::schedule::schedule::Schedule {
    &mut state.state.redraw
}

/// The nested output's refresh interval (60 Hz when the mode does not say).
fn refresh_of(context: &WinitRenderContext) -> std::time::Duration {
    let refresh_mhz = context
        .output
        .current_mode()
        .map(|mode| mode.refresh)
        .filter(|refresh| *refresh > 0)
        .unwrap_or(60_000);
    std::time::Duration::from_micros(1_000_000_000 / refresh_mhz as u64)
}

/// An empty frame still owes its frame callbacks, but sending them now would let
/// a client that commits without damage loop as fast as it can. Send them one
/// refresh interval later instead, from a one-shot timer that exists only while
/// callbacks are owed.
fn owe_frames(context: &mut WinitRenderContext, state: &mut Loop, visible: Vec<Window>) {
    frames::draw::present::callbacks::callbacks::housekeeping(state);
    if let Some(owed) = context.owed_frames.as_mut() {
        // A timer is already armed: it sends to these too.
        for window in visible {
            if !owed.contains(&window) {
                owed.push(window);
            }
        }
        return;
    }
    context.owed_frames = Some(visible);
    let interval = refresh_of(context);
    let this = context.this.clone();
    let armed = state.loop_handle.insert_source(
        smithay::reexports::calloop::timer::Timer::from_duration(interval),
        move |_, _, state: &mut Loop| {
            if let Some(context) = this.upgrade() {
                let mut context = context.borrow_mut();
                let visible = context.owed_frames.take().unwrap_or_default();
                let output = context.output.clone();
                drop(context);
                frames::draw::present::callbacks::callbacks::send_window_frames(
                    state, &output, &visible,
                );
                frames::draw::present::callbacks::callbacks::send_layer_frames(state, &output);
                frames::draw::present::cursor::cursor::send_frames(state, &output);
            }
            smithay::reexports::calloop::timer::TimeoutAction::Drop
        },
    );
    if let Err(err) = armed {
        // Without the timer the callbacks would never go: send them now instead.
        warn!("winit: frame-callback timer not armed ({err}); sending now");
        let visible = context.owed_frames.take().unwrap_or_default();
        frames::draw::present::callbacks::callbacks::send_window_frames(
            state, &context.output, &visible,
        );
        frames::draw::present::callbacks::callbacks::send_layer_frames(state, &context.output);
        frames::draw::present::cursor::cursor::send_frames(state, &context.output);
    }
}

fn compose(
    context: &mut WinitRenderContext,
    state: &mut Loop,
) -> (
    Option<Vec<Rectangle<i32, Physical>>>,
    Vec<Window>,
    Option<screencopy::Captures<GlesRenderer>>,
    bool,
) {
    // Single source of truth for the output size: the static, scale-1 mode `route.rs` set
    // from the *logical* window size — NOT the raw physical `window_size()`, so the render
    // stays consistent with the compositor's coordinate system and doesn't drift when the
    // host DPI changes. The EGL framebuffer from `bind()` stays physical, so under a
    // fractional host the nested view under-fills (dev-only cosmetic); under a scale-1 host
    // / udev logical == physical and it's exact.
    let monitor_size = context
        .output
        .current_mode()
        .map(|m| m.size)
        .unwrap_or_else(|| context.winit_backend.window_size());
    // What this frame changed, from the damage tracker; `None` when nothing did
    // (or the frame plan drew no scene), which `draw` turns into "submit nothing".
    let mut damage: Option<Vec<Rectangle<i32, Physical>>> = None;
    // wlr-screencopy readbacks this frame started, finished by `draw` after the swap.
    let mut captures = None;
    // Whether this frame rendered offscreen (a cursorless screencopy frame).
    let mut offscreen = false;
    // How stale the back buffer is, so the tracker repaints only what changed
    // since it was last shown. 0 (no age support) means a full repaint.
    // A screencopy owed on this output reads the whole picture back after the
    // render, so this frame is rendered in full (age 0): a partial repaint or a
    // skipped frame would leave a back buffer that is not the current picture.
    // After a failed swap the back buffers are unknown: in full too.
    //
    // EGL answers the buffer-age query only for the CURRENT draw surface
    // (EGL_BAD_SURFACE otherwise, then a needless full repaint), and anything
    // between frames may have made the context current without it: a commit
    // handler's import or a new surface's setup, screencopy's mapping, startup
    // prewarm. Restore it here, once, for all of them (no GL work when current).
    if let Err(err) = crate::frame::submit::submit::ensure_surface_current(&mut context.winit_backend) {
        warn!("winit: window surface not made current before the frame ({err})");
    }
    let age = if context.swap_failures > 0
        || context.render_failures > 0
        || screencopy::wants_full_frame(&context.output)
        || screencopy::file::pending_output(&context.output)
    {
        0
    } else {
        context.winit_backend.buffer_age().unwrap_or(0)
    };
    // The compositor renders at the logical `monitor_size`, but the winit EGL framebuffer is
    // the host's PHYSICAL window size. Present by stretching the logical render up to fill
    // it, so the nested window occupies the whole host window at any host DPI while the
    // compositor stays at the logical scale-1 output. Under a scale-1 host these are equal.
    let present_size = context.winit_backend.window_size();
    let present_full = Rectangle::<i32, Physical>::from_loc_and_size((0, 0), present_size);
    // Stable capture id for this output — the SAME EDID-derived `OutputId::from_key`
    // the rim's capture requests use (they key off `active_output()` on every
    // backend), so entries match on winit too. A hardcoded `OutputId(0)` here never
    // matched the request's keyed id → capture silently produced nothing.
    let capture_output = OutputId::from_key(
        &world::state::state::output_key(&context.output),
    );

    let (gles_renderer, mut gles_framebuffer) = context.winit_backend.bind().unwrap();

    // The capture registry is created by the scene's GPU phase, never built
    // mid-render here. Its tap subscription lives on
    // this backend's render context (created during render), so subscribe exactly
    // once here: registry presence IS the tap.
    if state.inner.kernel.get(&world::driver::capture::base::CAPTURE_REGISTRY).is_some()
        && !context.tap_subscriptions.is_active(POST_SCENE)
    {
        context.tap_subscriptions.subscribe(POST_SCENE);
        info!("winit: POST_SCENE tap subscribed");
    }

    // The compositor decides what this frame contains. The session-lock and
    // picker passes are not part of this plan; the renderer keeps its default
    // (no-bundle) facts.
    let frame_plan = plan(&state.inner.status);
    let render_scene = frame_plan.has_pass(FramePass::Scene);
    let tap_post_scene =
        frame_plan.has_tap(POST_SCENE) && context.tap_subscriptions.is_active(POST_SCENE);

    let mut visible_window: Vec<Window> = Vec::new();
    if render_scene {
        // GLES prepare phase (builds iced/bevy/parallax resources) — always on
        // the winit GlesRenderer, regardless of which renderer composes.
        let prepared =
            frames::scene::scene::prepare(state, gles_renderer, monitor_size);

        // GLES only: there is no Vulkan present path.
        {
            // --- GLES present path ---
            let scene = frames::scene::scene::scene::<GlesRenderer>(
                state,
                gles_renderer,
                monitor_size,
                prepared,
            );
            visible_window = scene.visible_window;

            sync_tracker(&mut context.damage_tracker, &context.output);
            damage = match context.damage_tracker.render_output(
                gles_renderer,
                &mut gles_framebuffer,
                age,
                &scene.Element,
                [0.1, 0.1, 0.1, 1.0],
            ) {
                Ok(result) => {
                    if context.render_failures > 0 {
                        info!("winit: render recovered after {} failed frames", context.render_failures);
                        context.render_failures = 0;
                    }
                    result.damage.filter(|damage| !damage.is_empty()).cloned()
                }
                Err(err) => {
                    // A render error must not kill compd: nothing of this frame is
                    // shown (no damage: `draw` treats it as an empty frame, paced
                    // one refresh later), and the next renders in full (age 0).
                    if context.render_failures == 0 {
                        warn!("winit: render failed ({err:?}); repainting in full");
                    }
                    context.render_failures += 1;
                    state.state.redraw.request_for(
                        protocols::redraw::schedule::schedule::RedrawReason::Rescue,
                    );
                    None
                }
            };

            // Bus screenshots use the just-rendered window framebuffer. The
            // wire vocabulary uses "offscreen" for the nested render source.
            if screencopy::file::pending_output(&context.output) {
                offscreen = true; // readback/map can change the current GL target
                if context.render_failures == 0 {
                    screencopy::file::service(gles_renderer, screencopy::Source {
                        framebuffer: &gles_framebuffer,
                        readback: screencopy::Readback { size: present_size, origin_bottom_left: true },
                    }, &context.output, screencopy::file::CaptureSource::Offscreen);
                } else {
                    screencopy::file::fail_output(&context.output, "window render failed");
                }
            }

            // wlr-screencopy: copies owed on this output start reading this frame
            // back before it is swapped; `draw` finishes them
            // after the swap. Every rendered frame goes
            // through here so damage copies see its damage. The EGL window
            // surface is a GL default framebuffer: row 0 is the picture's bottom.
            //
            // A copy that asked for no cursor (`overlay_cursor = 0`) reads a second
            // render of this frame without its cursor elements (`is_cursor`: the
            // sprite, the drag icon, the canvas cursor box) into an offscreen texture, made only when such a copy is due.
            // Rendering it leaves the texture current, so `draw` makes the window
            // surface current again before the swap (`offscreen`).
            let mut cursorless_texture = None;
            if screencopy::sources_due(&context.output, damage.as_deref()).cursorless {
                offscreen = true;
                match screencopy::render_offscreen(
                    gles_renderer,
                    &scene.Element,
                    |element| !element.is_cursor(),
                    present_size,
                    context.output.current_scale().fractional_scale(),
                ) {
                    Ok(texture) => cursorless_texture = Some(texture),
                    Err(err) => warn!("screencopy: cursorless render failed ({err})"),
                }
            }
            let cursorless_framebuffer = cursorless_texture.as_mut().and_then(|texture| {
                gles_renderer
                    .bind(texture)
                    .map_err(|err| warn!("screencopy: cursorless texture not bound ({err})"))
                    .ok()
            });
            captures = Some(screencopy::service(
                gles_renderer,
                Some(screencopy::Source {
                    framebuffer: &gles_framebuffer,
                    readback: screencopy::Readback { size: present_size, origin_bottom_left: true },
                }),
                // An FBO rendered untransformed is top-down, as a KMS buffer is.
                cursorless_framebuffer.as_ref().map(|framebuffer| screencopy::Source {
                    framebuffer,
                    readback: screencopy::Readback { size: present_size, origin_bottom_left: false },
                }),
                &context.output,
                damage.as_deref(),
            ));

            // Post-scene tap (GLES winit path). Window/world targets render
            // their windows into the entry (off-screen capable, chrome-free);
            // screen/full-screen targets blit the framebuffer.
            if tap_post_scene {
                if let Some(job) =
                    recorder::interface::render::window_render_job(state)
                {
                    if let Some(mut dmabuf) = state
                        .inner.kernel.get(&world::driver::capture::base::CAPTURE_REGISTRY)
                        .as_ref()
                        .and_then(|r| r.entry_dmabuf(job.entry_id))
                    {
                        recorder::interface::render::draw_windows_into(
                            gles_renderer,
                            &mut dmabuf,
                            job.size,
                            &job.windows,
                            job.scale,
                        );
                    }
                } else if let Some(registry) = &mut state.inner.kernel.get(&world::driver::capture::base::CAPTURE_REGISTRY) {
                    registry.tick(
                        &state.inner.environment.GPU.as_str(),
                        gles_renderer,
                        capture_output,
                        &gles_framebuffer,
                        monitor_size,
                    );
                }
            }
        }
    }

    (damage, visible_window, captures, offscreen)
}

/// Presentation feedback + frame callbacks + housekeeping via the shared compositor
/// crates, then ask winit for the next redraw (winit has no hardware page-flip;
/// presentation is immediate).
fn present(context: &mut WinitRenderContext, state: &mut Loop, visible: Vec<Window>) {
    // Presentation feedback, which this path used to skip entirely. Frame callbacks
    // and `wp_presentation` are different protocols: sending only the former left every
    // feedback request to be destroyed unanswered, and an unanswered feedback reaches
    // the client as `discarded` — "never shown" — for frames that WERE shown. Collect
    // and report in one step, since nested presentation completes at submit and there
    // is no flip event to come back to. See `presented_now`.
    {
        // `None`: the nested backend composites into the host's surface and never
        // promotes a client buffer to a plane, so no surface here is ever
        // zero-copy. Passing states would be work to derive a constant `false`.
        let mut feedback = frames::draw::present::callbacks::callbacks::collect_feedback(
            &context.output,
            &visible,
            None,
        );
        frames::draw::present::software::software::presented_now(
            &mut feedback,
            &context.output,
        );
    }
    // Nested presents at submit (no flip event).
    world::comp::presentation::frame_queued(state, &context.output, &visible);
    {
        let now: std::time::Duration =
            smithay::utils::Clock::<smithay::utils::Monotonic>::new().now().into();
        // Refresh::Unknown: the host gives no vblank, so the stats' refresh is
        // unknown and `missed` / `refresh_us` read null on nested, not a count
        // against a mode rate.
        world::comp::presentation::presented(
            state,
            &context.output,
            now,
            None,
            frames::draw::present::software::software::software_present_kind().bits(),
        );
    }
    frames::draw::present::callbacks::callbacks::send_window_frames(
        state,
        &context.output,
        &visible,
    );
    frames::draw::present::callbacks::callbacks::send_layer_frames(state, &context.output);
    frames::draw::present::cursor::cursor::send_frames(state, &context.output);
    frames::draw::present::callbacks::callbacks::housekeeping(state);
    // No unconditional request for the next frame: `draw` asks for
    // one only when a request is still outstanding.
}

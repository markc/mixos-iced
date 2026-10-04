use smithay::backend::renderer::gles::GlesRenderer;
use dispatcher::state::state::RedrawReason;
use smithay::utils::{Physical, Size};
use world::state::Loop;
use world::state::state::CoordinateTrait;
use slots::world::frame::base::FramePlan;
use world::notify::present::base::NotifyFrame;

/// What one output's systems tick produced: the active world's remaining draw
/// plan (the pass bridges what it knows), and the kernel host's notification
/// pill for this output, already taken out of it.
pub struct Ticked {
    pub plan: FramePlan,
    pub notify: Option<NotifyFrame>,
}

/// A per-frame hook a crate above frames installs: run once per output
/// inside the GLES prepare pass, with the rim hooks.
pub type FrameHook = fn(&mut Loop, &mut GlesRenderer, Size<i32, Physical>);

thread_local! {
    /// Installed by compd main (the Mix Scenes host, scene-host, layer 5),
    /// which frames (layer 4) cannot name.
    static FRAME_HOOKS: std::cell::RefCell<Vec<FrameHook>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Install a [`FrameHook`]. Compositor thread only; hooks run in install order.
pub fn register_frame_hook(hook: FrameHook) {
    FRAME_HOOKS.with_borrow_mut(|hooks| hooks.push(hook));
}

/// The main scene's per-frame rim hooks, then the active world's systems tick.
pub fn hooks(state: &mut Loop, renderer: &mut GlesRenderer, size: Size<i32, Physical>) -> Ticked {
    crate::hook::window::interface::hook(state, renderer);
    crate::hook::surface::wgpu::hook(state, renderer, size);
    recorder::interface::interface::per_frame(state, renderer, size);
    // Debug FPS overlay (top-right): measures the composited-frame rate.
    world::surface::draw::fps::fps::per_frame(state, renderer, size);
    // The installed hooks (copied out: a hook may itself touch the list).
    for hook in FRAME_HOOKS.with_borrow(Clone::clone) {
        hook(state, renderer, size);
    }
    world::bus::legacy::legacy::drain(state, |l| &mut l.inner.bus);
    let ticked = world_tick(state, renderer, size);

    // compd: the finger-glide cursor re-seat and the absolute edge-pan tick
    // served camera controls, which are cut with the camera pinned to identity.

    // Frame-end persistence commit — PATH 2 (rim catch-all): a mutation outside
    // `buffer()` flags its world via `mark_world`; here we commit the marked worlds
    // whose debounce is due (immediate, or batched up to 1s), e.g. an overlay world
    // that has stopped being flushed. Buffer transacts commit at their own buffer
    // boundary (`flow::flush`, path 1) and never reach here. Only marked worlds are
    // touched — no per-frame all-world poll; the changed-only diff is in the engine.
    for world_id in slots::persist::mark::base::due_worlds() {
        if !state.inner.worlds.contains(world_id) {
            continue;
        }
        let world = state.inner.worlds.get(world_id);
        slots::persist::flush::base::commit_world(
            world_id, world.storage(), &world.systems,
        );
    }

    // DEFERRED (plan): DrawOrder GC of destroyed drawables. The proper form is
    // event-driven — unregister on a drawable-destruction event (DrawOrder.remove
    // at each destroy path) rather than a per-frame live-set scan. Until then a
    // destroyed iced surface leaves a stale order entry, which is harmless
    // (element_of / hit_iced_one return None for it).
    ticked
}

/// The systems tick for one output's frame: publish this output's screen
/// context, then dispatch → `update()` → `draw()` the ACTIVE world and, after
/// it, the KERNEL system host (`WorldManager::kernel`) — the systems that run
/// whatever world is active — into one plan.
///
/// The one tick for the scene and picker passes — each owns a prepare path, and
/// a world that is on screen must tick whichever pass draws it, or a system that
/// only exists to be ticked (the notification pill, the parallax animation)
/// stops the moment the frame plan picks another pass. The LOCK pass deliberately
/// does NOT tick: the lock screen shows no notifications, and not ticking the
/// kernel host is what keeps a queued message waiting for the unlock instead of
/// being consumed unseen.
pub fn world_tick(state: &mut Loop, renderer: &mut GlesRenderer, size: Size<i32, Physical>) -> Ticked {
    // Per-frame screen context for systems (KernelData). Background systems read
    // physical output size from here (SCREEN) — the former background.shared
    // OUTPUT_SIZE world token is gone.
    {
        let scale = state.size_ctx_all().scale;
        let output = std::sync::Arc::from(state.inner.current_output_key().as_str());
        world::smithay_glue::data::data::update_screen(
            &mut state.inner.kernel,
            world::smithay_glue::data::data::ScreenContext { size, scale, output },
        );
    }
    state.inner.pilot_tick += 1;
    let tick = slots::world::frame::base::FrameTick {
        index: state.inner.pilot_tick,
        delta: std::time::Duration::ZERO,
    };
    {
        let (worlds, kernel) = (&mut state.inner.worlds, &state.inner.kernel);
        worlds.active_mut().dispatch(kernel);
        worlds.kernel_mut().dispatch(kernel);
    }
    let gpu = state.inner.environment.GPU.clone();
    let mut plan = FramePlan::new();
    {
        // Lend systems the live renderer + window Space via the Platform hatch.
        // SAFETY: platform is dropped at the end of this block; the driver does
        // not touch state.inner.space_state() or the renderer during the calls.
        let mut platform = unsafe {
            world::scene::platform::platform::Platform::new(
                Some(renderer),
                &mut state.inner.space_state_mut().state,
                &gpu,
            )
        };
        let kernel = &state.inner.kernel;
        // Lend the seat (the wayland `Dispatch`) to the update path DISJOINTLY from
        // the world (`&mut state.state` is a different field than `state.inner`), so
        // the navigator system warps the pointer directly via `cx.seat` — no
        // `pending_pointer_warp` round-trip.
        let seat: &mut dyn std::any::Any = &mut state.state;
        let world = state.inner.worlds.active_mut();
        world.update(kernel, &tick, Some(&mut platform), Some(seat));
        world.draw(kernel, &mut plan, Some(&mut platform));
        let host = state.inner.worlds.kernel_mut();
        host.update(kernel, &tick, Some(&mut platform), Some(seat));
        host.draw(kernel, &mut plan, Some(&mut platform));
    }
    // The kernel host's node, bridged here for every pass: the slide is
    // time-driven, so it needs frames even when nothing else changed.
    let notify = plan.take::<NotifyFrame>();
    if notify.as_ref().is_some_and(|n| n.animating) {
        state.schedule_redraw(RedrawReason::Animation);
    }
    Ticked { plan, notify }
}

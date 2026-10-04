#![allow(irrefutable_let_patterns)]
#[macro_use]
extern crate model;
mod cli;
mod comp;
mod event_loop;
mod scenes;
mod shutdown;
mod wayland;
mod xwayland;

use model::{info, trace, warn};

use smithay::reexports::calloop::channel as cl_channel;
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::{Interest, Mode, PostAction};
use smithay::reexports::{calloop::EventLoop, wayland_server::Display};
use std::time::Instant;
use world::state::Loop;
use world::state::state::{Loader, Orchestrator as State};
use dispatcher::wire::trait_::wire_trait::WireTrait;
// App-launch executor (kernel.execution driver) — all worker/reaper/channel
// wiring is encapsulated behind `install`.
use crate::execution::driver::executor::install::install as launch_executor;

/// compd's semver (the package version). The full provenance line — sha, dirty
/// bit, build time — comes from `buildinfo::build_info!()`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Set once the idle hook has reported the active world diverging from the
/// spawn target (see the queued-iced check), so the error is logged once.
static WORLD_DIVERGENCE_REPORTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Answer `--version`/`-V` (and `--version --json`) as
    // `compd <semver> (<sha12>, built <ts>)` BEFORE any side effect, then exit 0.
    buildinfo::exit_on_version!();

    // Capture $NOTIFY_SOCKET while still single-threaded (it is removed from
    // the environment so launched clients do not inherit it), then the command
    // line — before config, so --help/--version need no settings file.
    slots::sdnotify::init();
    let cli = cli::parse(&buildinfo::build_info!().line());
    let backend = cli::resolve(&cli);
    if let Some(device) = cli.device.clone() {
        model::environment::config::base::override_scanout_node(device);
    }
    if let Some(connector) = cli.connector.clone() {
        kms::connector::select::select::force(connector);
    }
    slots::sdnotify::status(&format!("starting ({} backend)", backend.label()));

    // FIRST: parse the single COMPOSITOR_ENVIRONMENT JSON into the process-global
    // config. This must run before anything else — including logging, which reads
    // `log_level` from it — and panics immediately if the var is unset or any
    // required field is missing/malformed. It is the ONLY place env config is read.
    model::environment::config::base::init();

    // Aggregate the opt-in experimental `gpu_*` flags (experimental.json). Lenient:
    // a missing/invalid file leaves the flag set empty, so defaults are unchanged.
    // Read here, right after config; unrecognized flags are warned once below.
    model::environment::experimental::base::init();

    // Before the log threads below, and before every other spawn: a `signalfd` only ever
    // receives signals that are BLOCKED, and the mask is inherited by threads created
    // afterwards. See `shutdown::block_signals`. SIGCHLD is deliberately NOT blocked —
    // launched children are collected through their own exit descriptors now, so nothing
    // consumes that signal (`child.pidfd`).
    shutdown::block_signals();

    let environment = world::environment::type_::base::Get();
    slots::library::debug::client::init_logging();

    // Start the developer logging process (fan-in buffer + drain/print + gRPC stream).
    // Levels come from COMPOSITOR_LOG_LEVEL.
    model::log::process::main::spawn();
    info!("{}: {} backend", buildinfo::build_info!().line(), backend.label());
    for var in cli::UNSUPPORTED_ENV {
        if std::env::var_os(var).is_some() {
            warn!("{var} is set but compd does not implement it; ignored");
        }
    }

    // Now that logging is up, surface the experimental flag state (the crate itself
    // has no logging dep, so it can't warn at parse time).
    {
        use model::environment::experimental::base as experimental;
        let gpu_flags = experimental::get();
        if !gpu_flags.is_empty() {
            info!("experimental gpu flags active: {gpu_flags:?}");
        }
        if gpu_flags.contains(experimental::GpuFlags::NEGOTIATE_FORMATS) {
            // Reserved: the bridge's wgpu engine format is fixed (Bgra8UnormSrgb) and
            // the content is alpha-blended, so Argb8888 is the only correct fourcc.
            warn!("gpu_negotiate_formats has no effect: the bridge render format is fixed");
        }
        for f in experimental::unknown() {
            warn!("ignoring unrecognized experimental flag: {f:?}");
        }
    }

    // KNOWN-INVALID SPIR-V, stated at boot so it is never rediscovered from a silent
    // validation log. naga interns SPIR-V types by handle alone and decorates struct
    // members unconditionally, so one `OpTypeStruct` is shared between a push-constant
    // block and a function-local and carries `Offset`/`ArrayStride`/`MatrixStride` where
    // SPIR-V allows none. Every shader we emit through naga is affected — ours, iced's
    // and bevy's.
    //
    // Harmless in practice: those storage classes have no defined layout, nothing reads
    // the decoration, and the laid-out uses of the same type keep theirs, so addressing
    // is unaffected. upstream wgpu suppresses the same VUID in its own debug callback
    // (gfx-rs/wgpu#7696). We filter it in `environment/vk_layer_settings.txt` because at
    // `duplicate_message_limit = 0` it buries sync validation.
    //
    // The reason this is a boot WARNING and not a comment somewhere: the filter means a
    // future reader sees a clean validation log and concludes the SPIR-V is valid. It is
    // not. Anything that genuinely validates — `spirv-val`, `spirv-opt`, GPU-AV shader
    // instrumentation — may still refuse these modules, and this line is what connects
    // that failure to its cause.
    warn!(
        "naga emits SPIR-V that violates VUID-StandaloneSpirv-None-10684 (explicit layout \
         decorations on non-laid-out types); inert at runtime and FILTERED in \
         environment/vk_layer_settings.txt, so the validation log understates it — see \
         environment/patches/README.md"
    );

    // Install the UI fonts (the pinned asset set, else the embedded Inter) into
    // iced's lazy global font system while it is still untouched — before any
    // engine/surface exists — so the generic families resolve to the set's
    // faces even on systems with no fonts.
    ui::font::default::base::install();

    // Arm the persistence engine (spawn its writer thread) before any world flushes.
    slots::persist::engine::base::init();
    info!(
        "developer log online: gpu={} desktop={}",
        environment.GPU, environment.DesktopName
    );

    info!("Environment: {:?}", environment);

    // Record the renderer/sync-relevant config for the developer Statistics tab.
    // Built from the parsed config (not the environment), keeping the COMPOSITOR_*
    // display names the viewer already shows.
    let e = model::environment::config::base::get();

    // CPU scheduling boost (settings.json `priority`): applied here — logging
    // is up (the ladder logs which mechanism succeeded), and no worker threads
    // that shouldn't inherit the nice value have spawned yet.
    crate::priority::base::apply(&e.priority);

    let env_flags: Vec<(String, String)> = vec![
        ("COMPOSITOR_PRIORITY".to_string(), e.priority.clone()),
        ("COMPOSITOR_RENDERER_SYNC".to_string(), e.renderer_sync.clone()),
        ("COMPOSITOR_HDR".to_string(), e.hdr.to_string()),
        ("COMPOSITOR_DEPTH".to_string(), e.depth.to_string()),
        ("COMPOSITOR_VRR".to_string(), e.vrr.to_string()),
        ("COMPOSITOR_RENDER_NODE".to_string(), e.render_node.clone()),
        ("COMPOSITOR_LOG_LEVEL".to_string(), e.log_level.clone()),
        (
            "EXPERIMENTAL_GPU_FLAGS".to_string(),
            format!("{:?}", model::environment::experimental::base::get()),
        ),
    ];
    model::stats::registry::base::set_env_flags(env_flags);

    info!("Create an event loop");
    // Creates Smithay event loop
    let (mut event_loop, display) = event_loop::create()?;

    info!("Create the wayland socket");
    // Create a wayland socket
    let wayland_socket = wayland::create_socket(cli.socket.as_deref());
    // let wayland_socket_proprietary = wayland::create_socket_proprietary();

    info!(
        "Create the wayland socket {:?}",
        wayland_socket.name.clone(),
        // wayland_socket_proprietary.name.clone()
    );

    // The backend is chosen at runtime (`cli::resolve`); a `backend-all` build
    // carries both.
    let nested = backend == cli::Backend::Nested;

    // Session compositor (native backend): scrub WAYLAND_DISPLAY / DISPLAY from
    // OUR OWN environment before ANY GPU init. The Mesa `VK_LAYER_MESA_device_select`
    // Vulkan layer, inside `vkEnumeratePhysicalDevices`, connects to $WAYLAND_DISPLAY
    // and does a BLOCKING wl_display roundtrip to discover the "current" compositor's
    // GPU — while holding the Vulkan loader's global mutex. We are the compositor, not
    // a client; worse, an inherited *stale* WAYLAND_DISPLAY (leaked into the systemd
    // user-manager env by a prior session's `announce_session` and reused as our own
    // not-yet-listening socket name) makes that roundtrip block forever. Two Vulkan
    // instances come up concurrently at startup — the `VulkanRenderer` on this thread
    // and the wgpu context on a worker — so the worker stalls inside the layer holding
    // the loader mutex and this thread's `vkCreateInstance` deadlocks on it: the
    // compositor hangs BEFORE the event loop starts (no seat/DRM-master involvement —
    // confirmed by backtrace). First-boot login has no WAYLAND_DISPLAY so it works;
    // every login after a successful session inherits the stale one and hangs. We
    // re-export WAYLAND_DISPLAY to our REAL socket for children later, after the
    // backend is wired. Nested/winit dev keeps it: there the host display is live, so
    // the layer's roundtrip is both wanted and non-blocking.
    if !nested {
        unsafe {
            std::env::remove_var("WAYLAND_DISPLAY");
            std::env::remove_var("DISPLAY");
        }
        info!("cleared inherited WAYLAND_DISPLAY/DISPLAY before GPU init (native session compositor)");
    }

    info!("Creating the loop loader");
    // Create loader properties to add to state
    let state_loader = Loader {
        socket_name: wayland_socket.name.clone(),
        // socket_name_proprietary: wayland_socket_proprietary.name.clone(),
        loop_signal: event_loop.get_signal(),
        display_handle: display.handle(),
    };

    info!("Initializing loop state");

    // The format registrar is created HERE, before anything that registers a
    // device capability or asks for one, and the handle is what travels: into the
    // wgpu init thread below (which registers what each adapter can import) and
    // into the kernel store further down, for everything on the compositor thread.
    let formats = render_gles::format::registrar::registrar::Registrar::new();

    // The injection point: the loader assembles the world set and
    // hands it to the orchestrator. KernelData is populated after Loop::new.
    let mut kernel_data = slots::storage::slot::base::Storage::new();
    // Shared iced GPU context lives in the kernel (driver data). Seeded `None`
    // here so the slot exists. wgpu runs on GL over the renderer's own EGL
    // context, which only exists with that context current, so the scene's GLES
    // phase fills this slot on the first frame (`scene.frame` `gpu::ensure_gpu`)
    // along with the engine, the capture registry and every world's iced
    // registry.
    kernel_data.insert(&world::surface::system::base::ICED_CONTEXT, None);
    // The one iced renderer, filled by `ensure_engine` in the same pass that
    // fills the context above. Seeded here for the same reason: a slot must exist
    // before it can be written.
    kernel_data.insert(&world::surface::system::base::ICED_ENGINE, None);
    // The effects host. `NullEffects` by default: the scene asks
    // it for elements every output frame and gets none. An effects crate (Bevy, or
    // anything else) installs its own host here instead.
    kernel_data.insert(
        &effects::EFFECTS,
        Box::new(effects::NullEffects)
            as Box<dyn effects::EffectHost>,
    );
    let worlds = {
        // World kinds: the main world is SPATIAL (owns the window Space + is the
        // spawn-target).
        // The loader injects the concrete system set; the builder stamps the kind.
        //
        // ONE world: no navigator, no overlay worlds for the lock or the picker,
        // no persisted-world restore. CameraSystem only owns the viewport slot
        // (camera pinned to identity).
        world::world::manager::manager::WorldManager::new(
            world::kind::build::base::spatial(
                world::world::manager::manager::MAIN_WORLD,
                "main",
                vec![
                    Box::new(world::camera::system::base::CameraSystem::default()),
                    Box::new(world::window::system::base::WindowSystem),
                    Box::new(world::surface::system::base::SurfaceSystem),
                    Box::new(world::canvas::system::base::CanvasSystem),
                    Box::new(world::seat::system::pointer::base::PointerSystem),
                ],
                &kernel_data,
            ),
            // The KERNEL system host: systems that run every frame whatever world
            // is active. OVERLAY-class (no Space, never a spawn-target, nothing
            // persisted) and outside the world set — nothing here is per world.
            world::kind::build::base::overlay(
                world::world::manager::manager::KERNEL,
                "kernel",
                vec![
                    // The notification pill: queued, drawn above everything.
                    Box::new(world::notify::system::base::NotifySystem::default()),
                ],
                &kernel_data,
            ),
            &kernel_data,
        )
    };

    let inner = State::new(
        environment.clone(),
        nested,
        state_loader,
        kernel_data,
        worlds,
    );
    // Initialize loop state
    let mut state = Loop::new(inner, &display.handle(), None, event_loop.handle());

    // KernelData: hand systems the smithay wiring handles (read-only tokens).
    {
        let pointer = state.state.seat.seat.get_pointer().expect("seat factory adds a pointer");
        let keyboard = state.state.seat.seat.get_keyboard().expect("seat factory adds a keyboard");
        world::smithay_glue::data::data::populate(
            &mut state.inner.kernel,
            display.handle(),
            event_loop.handle(),
            pointer,
            keyboard,
        );
        // Kernel data, beside GPU_BINDING: created above, before anything that
        // registers a capability, and inserted here for everything on the
        // compositor thread. Producers hold their own clone.
        state.inner.kernel.insert(
            &render_gles::format::registrar::registrar::FORMATS,
            formats.clone(),
        );
    }

    // Re-advertise per-world foreign-toplevels when the spawn-target world changes —
    // event-driven (replaces the old per-iteration generation poll).
    state.inner.bus.register(
        &world::state::state::WORLD_SWITCHED,
        |l, _event| l.on_world_switched(),
    );

    let wayland_socket_name_default_subprocess = wayland_socket.name.clone();
    let wayland_socket_name_default_subprocess_2 = wayland_socket.name.clone();
    let wayland_socket_name_for_children = wayland_socket.name.clone();

    info!("Hooking up wayland to the loop");
    // Register wayland socket in Smithay event loop.
    wayland::register(wayland_socket, &mut event_loop);
    // wayland::register(wayland_socket_proprietary, &mut event_loop, true);

    // You also need to add the display itself to the event loop, so that client events will be processed by wayland-server.
    event_loop
        .handle()
        .insert_source(
            Generic::new(display, Interest::READ, Mode::Level),
            |_, display, state| {
                // Safety: we don't drop the display
                unsafe {
                    // `D = Dispatch`: the wayland dispatch type is the protocol
                    // state field, not the whole Loop.
                    display.get_mut().dispatch_clients(&mut state.state).unwrap();
                }
                // No drain here. Dispatching only QUEUES onto the protocol outboxes;
                // applying them is the loop's job, once per iteration, after every
                // source has had its turn (see `event_loop.run` below). It used to be a
                // statement right here, and could not stay: X11 arrives on its own
                // source, and two sources that each drain make the order a frame's
                // events are applied in a function of which fd calloop polled first.
                //
                // Nothing is lost by deferring it. calloop's `run` is
                // `while !stop { dispatch(); cb(); }`, so the drain follows every
                // dispatch in the SAME iteration — this callback cannot run without one
                // behind it. `dispatch_clients` appears nowhere else in the tree, and
                // the only nested loop (the xwm pump) is X11-only and is itself driven
                // by a source of this loop, so there is no wayland dispatch that escapes
                // the drain.
                //
                // The marker is what keeps that callback from draining on wakes with no
                // protocol traffic behind them — see `Dispatch::protocol_pending`.
                state.state.protocol_pending = true;
                Ok(PostAction::Continue)
            },
        )
        .unwrap();

    info!("Creating renderer");

    // The DRM device, kept reachable for the ONE thing that has to happen after the
    // event loop stops: re-committing the mode the console was using, so the getty is
    // visible again (`DrmDevice::restore_state`).
    //
    // Held here because nothing else can hold it usefully. Every other strong reference
    // lives inside a calloop source, and the loop never frees its sources — several
    // source closures captured a clone of the loop's own handle, which is a refcount back
    // to the loop, so the loop and its callbacks keep each other alive and `drop` of the
    // `EventLoop` frees nothing. That is why the restore is CALLED rather than left to a
    // destructor; see the note above the call site at the end of `main`.
    #[cfg(feature = "backend-native")]
    let mut drm_restore = None;
    match backend {
        #[cfg(feature = "backend-native")]
        cli::Backend::Kms => {
            info!("starting loader...");
            // The returned handles are the backend's integration surface (see
            // compositor_kernel_native_device_interface_base): the main project applies
            // runtime device settings through them.
            // KMS: the one process-wide output scale, before any output exists.
            kms::output::physical::physical::set_scale(cli::output_scale(&cli, cli::Backend::Kms).unwrap_or(1.0));
            let backend_handles = native::wire::entry::entry::wire(
                &mut state,
                wayland_socket_name_default_subprocess,
                &mut event_loop,
            );
            info!("starting loader OK");
            let manager = backend_handles.ctx.borrow().drm_output_manager.clone();
            drm_restore = Some(manager);
        }
        #[cfg(feature = "backend-winit")]
        cli::Backend::Nested => {
            // `--scale` overrides the host window's scale for the nested output.
            nested::window::factory::factory::set_scale(cli::output_scale(&cli, cli::Backend::Nested));
            nested::wire::entry::entry::wire(
                &mut state,
                wayland_socket_name_default_subprocess,
                &mut event_loop,
            );
        }
        // `cli::resolve` only returns a backend that is compiled in.
        #[allow(unreachable_patterns)]
        other => unreachable!("backend {other:?} is not compiled into this build"),
    }

    // After udev and winit have initialized, activate the environment
    //
    info!("Backend initialization - Complete");
    slots::sdnotify::status(&format!(
        "{} backend up on {:?}; waiting for the first presented frame",
        backend.label(),
        wayland_socket_name_for_children
    ));

    // THE REGISTRAR IS COMPLETE — advertise.
    //
    // Every role is now registered (the backend published its own; whatever this
    // machine has none of was registered empty), so this is the first point at which the dmabuf feedback
    // can be computed from every term the machine imposes. It used to be built
    // inside `lifecycle::initialize`, during backend wire, while those adapters
    // were still probing — the advertisement was decided by a registrar that was
    // not finished, and the layer now refuses to answer in that state at all.
    //
    // Late costs nothing: the event loop has not begun dispatching, so no client
    // has seen the registry.
    outputs::advertise::advertise::advertise_dmabuf(&mut state);

    // Initial seat-pointer placement. The cursor renders at the seat's world
    // location (`(0,0)` -> screen center), but the relative-motion accumulator
    // (`PointerState.motion`) defaults to physical top-left; without this the
    // first mouse move would accumulate from the corner and the cursor would jump
    // there. This must run AFTER the backend maps the output — `apply_pointer`
    // re-projects world `(0,0)` through the live camera + output geometry, which
    // don't exist when the seat is constructed. Single-output, so it runs once.
    state.inner.apply_pointer(smithay::utils::Point::from((0.0, 0.0)));


    // Now that the backend is wired, advertise OUR socket as WAYLAND_DISPLAY for
    // child processes. This must happen AFTER the backend wire(): the winit
    // backend nests into the host compositor at init and reads WAYLAND_DISPLAY
    // to find it, so overwriting it earlier would point winit at our own
    // (not-yet-serving) socket. The native backend doesn't nest, so post-wire is
    // correct for both. Children spawn below (announce_session), after this.
    unsafe { std::env::set_var("WAYLAND_DISPLAY", &wayland_socket_name_for_children) };
    info!("WAYLAND_DISPLAY set to {:?} for child processes", wayland_socket_name_for_children);

    // Native XWayland. AFTER the WAYLAND_DISPLAY export above, because the X server is
    // a wayland client of ours and smithay hands it the socket it must connect back
    // on; and after the backend wire, so the first X11 window has an output to be
    // placed against. The server comes up asynchronously — `DISPLAY` is published from
    // its ready event, not here.
    {
        let dh = state.inner.loader.display_handle.clone();
        xwayland::register(&dh, &mut event_loop);
    }

    // SIGTERM / SIGINT -> stop the loop, so the teardown below actually runs. Registered
    // late, with everything it will tear down already wired.
    shutdown::register(&mut event_loop);

    // Widget and renderer worker notifications must restart an idle output as
    // well as wake calloop. Independent of the Bus and GPU publish wake paths.
    ui::engine::wake::register(&event_loop.handle(), |state: &mut world::state::Loop| {
        state.state.schedule_redraw(dispatcher::state::state::RedrawReason::Iced);
    })?;

    // The native `comp` Bus service. Its worker dials noded on
    // its own thread and retries with backoff, so a missing broker never holds up
    // the compositor; a failure here (bad name, no wake source) is logged and the
    // compositor runs without a Bus port rather than not at all.
    let mut bus = match comp::Bus::start(
        comp::service_name(backend, cli.bus_service.as_deref()),
        backend,
        &event_loop.handle(),
    ) {
        Ok(bus) => Some(bus),
        Err(err) => {
            warn!("comp Bus port not started: {err}");
            None
        }
    };
    let bus_port = &mut bus;

    // The Mix Scenes host (scene-host): its own Bus registration (`shell`),
    // opt-in by the `scene_host` preference. Like the comp port, a missing
    // broker never holds the compositor up.
    let mut scenes = scenes::Scenes::start(&state, cli.scene_service.as_deref(), &event_loop.handle());
    let scenes_port = &mut scenes;

    // After WlrLayerShellState::new and event loop is running.
    //
    // NOT when nested, which `announce_session`'s own doc has always demanded and nothing
    // enforced. It writes the per-USER systemd and D-Bus activation environments, so a
    // dev session under `run-host.sh winit` was repointing the HOST session's
    // `WAYLAND_DISPLAY` at its own socket — and inside a container sharing
    // `/run/user/$UID` that is literally the host's bus and user manager, which no probe
    // can distinguish from our own. Nothing is lost: our own launches read
    // `executor.install::base_env`, which is per-process, so apps started from inside the
    // nested session still get the right values. Only launches from OUTSIDE it are
    // affected, and those wanting a nested session can say so per-invocation
    // (`WAYLAND_DISPLAY=wayland-2 app`) instead of a global mutation nothing unwinds.
    if !nested {
        world::environment::interface::lifecycle::lifecycle::announce_session(
            wayland_socket_name_default_subprocess_2.to_str().unwrap(),
            &environment.DesktopName,
        );
    }

    // App-launch executor (kernel.execution): builds the Executor driver, stores
    // it as driver data, and wires its calloop sources (off-thread worker outcome
    // receiver + SIGCHLD reaper). Each completed launch is broadcast by
    // orchestration as the general per-world `Executed` event.
    launch_executor::install(&mut state, &event_loop.handle());

    // All compositor threads exist by now (they inherited the `priority`
    // boost, as intended) — arm the kernel-level inheritance stop: tasks
    // created from here on, the IME below and every launched client included,
    // start at default scheduling.
    crate::priority::arm::arm::arm(
        &model::environment::config::base::get().priority,
    );

    // Launch the compositor-owned input method configured in `preferences.json`
    // (`ime: { exec, args }`); unset ⇒ none is launched. Must be AFTER WAYLAND_DISPLAY is exported
    // (above) so it connects to OUR socket; the spawned process group is the ONLY client
    // authorized to bind the input-method / virtual-keyboard globals (see `text.input.launch`).
    protocols::text::input::launch::launch::launch(
        state.inner.preference.ime.clone(),
    );

    // Sampling heartbeat — a sparing, multi-level demo of live developer logs (so the
    // viewer shows activity over time and its level filters can be exercised). Remove when
    // not demoing.
    // std::thread::spawn(|| {
    //     let mut tick: u64 = 0;
    //     loop {
    //         std::thread::sleep(std::time::Duration::from_secs(4));
    //         tick += 1;
    //         info!("heartbeat tick={tick}");
    //         if tick % 3 == 0 {
    //             trace!("heartbeat detail: {tick} ticks, ~{}s uptime", tick * 4);
    //         }
    //         if tick % 5 == 0 {
    //             warn!("heartbeat milestone: {tick} ticks elapsed");
    //         }
    //     }
    // });

    // Sampler::Drop closes its internal registration channel, which causes
    // the thread to exit cleanly when state's sampler field is dropped.
    info!("Event Loop start");
    event_loop.run(None, &mut state, move |state| {
        // Apply the world effects this iteration's protocol handlers recorded
        // (map/commit/destroy/fullscreen/layer/dmabuf) — see SMITHAY_DECOUPLING.md.
        //
        // HERE, and not in the source callbacks, because wayland and X11 arrive on two
        // different calloop sources and the compositor must not care which one calloop
        // polled first. Draining per source made the boundary between them a
        // scheduling detail: an X11 destroy and the wayland surface destruction it
        // implies could be applied in either order, in one pass or two, depending on fd
        // readiness. Draining once after every source has been dispatched makes a frame's
        // protocol traffic one ordered batch regardless of which protocol carried it —
        // which is the whole reason the outboxes are shared between the two rather than
        // split per shell (`find::Shell`).
        //
        // Safe with respect to rendering, which also runs from sources: a redraw is
        // requested by writing a calloop ping from inside a handler, and a ping written
        // during this iteration is only readable on the next poll — so a render never
        // observes state this drain has not yet applied.
        //
        // Gated on the marker, so a wake with nothing behind it — a vblank, a timer —
        // does not pay for the drain's tail. `drain_protocol` consumes it. The rule is
        // that whatever WRITES a drained queue arms it (`Dispatch::arm_drain`): the
        // protocol sources for their own dispatch, and the few writers reachable from
        // input at the write itself. What must never arm it is the input SOURCE — that
        // would run the drain on every pointer motion for queues that fill when a lock
        // is released or a drag ends. See `Dispatch::protocol_pending`.
        //
        // (Input-independent control-plane work — display drains, lock engage — is
        // ping-driven via `state.inner.ping_control()`, drained in the backend's
        // control-plane ping source, NOT polled per dispatch. Foreign-toplevel
        // re-advertisement is event-driven off the `WORLD_SWITCHED` bus channel above.)
        if state.state.protocol_pending {
            state.drain_protocol();
        }
        // Camera pinned to identity: world == output-logical
        // for every comp-facing coordinate. Here, not only in the Bus pass, so it
        // holds with no Bus port too. Idempotent; true only when a camera moved.
        if world::camera::pin::pin(&mut state.inner) {
            state.state.schedule_redraw(dispatcher::state::state::RedrawReason::Output);
        }
        // The pointer starts at the first output's centre (a warp, not input;
        // once only), so it never rests in a hot corner at startup.
        world::camera::pin::centre_pointer_once(state);
        // The key bindings with or without a Bus port: this backend's table,
        // then the chords the keyboard queued this dispatch.
        state.inner.comp.bindings.ensure(if nested { "nested" } else { "kms-live" });
        policy_host::control::apply_bindings(state);
        // The session lock's policy, with or without a Bus port.
        // Held keys are released to their client first, while it still has the
        // keyboard (once `service` enters the lock, focus is gone).
        policy_host::input::release_keys_for_lock(state);
        world::comp::session_lock::service(state);
        // The exclusive-keyboard latch, with or without a Bus port.
        world::comp::latch::service(state);
        // A window that left fullscreen gets its band back.
        world::comp::fullscreen::service(state);
        // The comp policy's hidden decision, stamped for the hit driver, with or without a Bus port.
        world::comp::visibility::sync_hidden(state);
        // Then the Bus, so a read never sees state the drain has not applied.
        // A no-op unless the port's wake source fired.
        if let Some(bus) = bus_port.as_mut() {
            bus.service(state);
        }
        // The scene host's requests, after the comp port. A no-op unless its
        // wake source fired.
        if let Some(scenes) = scenes_port.as_mut() {
            scenes.service(state);
        }
        // Deliver what this pass queued on world channels NOW, not at the next
        // frame's dispatch. Input systems announce effects on channels: an iced
        // button RELEASE goes canvas -> ICED_BUTTON -> surface system -> registry,
        // and an iced Button fires on release. Left for the frame, nothing owed
        // one, so the click only acted when pointer motion drew the next frame
        // (the "wiggle to click" bug). Renderer-free (receivers only queue into
        // buffers; no Platform, seat or output) and a no-op on empty queues; it
        // runs dark or lit, so it supersedes the old dark-only pump. Before the
        // registry check below, so the delivered release asks for its frame.
        {
            let (worlds, kernel) = (&mut state.inner.worlds, &state.inner.kernel);
            worlds.active_mut().dispatch(kernel);
            worlds.kernel_mut().dispatch(kernel);
        }
        // Input (or a message) this pass queued for a compositor-owned iced
        // surface — the Mix Scenes host, the capture dialogs — is applied only
        // when a frame ticks the registry, and nothing else owes one when the
        // cursor is the host's (nested) or simply moves. One frame drains it, but
        // only a frame that runs: while frames are parked (session paused, panel
        // off, no output) the request was answered by a skip and pinged the loop
        // straight back, 6,600 times a second. What waits is drained by the frame
        // resume (or DPMS-on, or output recovery) requests anyway.
        #[cfg(feature = "backend-native")]
        let parked = native::render::execute::execute::frames_parked(state);
        #[cfg(not(feature = "backend-native"))]
        let parked = false;
        // A delivered release lands in the ACTIVE world's registry, presses and
        // frames use the spawn target's (`surface()`, scene.rs). They are one
        // world because `switch_to_world` moves both; nothing else enforces it.
        // Checking the active one too would request frames that never tick it
        // (a redraw loop); checking only one would silently drop clicks. So the
        // invariant is made loud instead: report the first divergence.
        if state.inner.worlds.active_id() != state.inner.worlds.spawn_target()
            && !WORLD_DIVERGENCE_REPORTED.swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            error!(
                "active world differs from the spawn target: iced input delivered to the active world is never ticked (clicks need a wiggle again)"
            );
        }
        if !parked && state.inner.surface().registry.as_ref().is_some_and(|registry| registry.has_queued()) {
            state.state.schedule_redraw(dispatcher::state::state::RedrawReason::Iced);
        }
        // Last, flush what this iteration queued for clients (configures, acks,
        // releases). The present path flushes too, but a compositor that only
        // draws when owed would otherwise hold a new client's initial configure
        // until some unrelated frame, and it never maps. (World-channel events
        // while dark are delivered by the every-pass dispatch above.)
        let _ = state.inner.loader.display_handle.flush_clients();
    })?;

    // THE DISPLAY FIRST, before anything slower, because it is the half the user is
    // looking at: re-commit the mode the console had before we took the card, so the
    // getty comes back instead of a monitor rejecting the timing.
    //
    // Called rather than left to `Drop`. Reaching smithay's destructor by refcount would
    // mean every strong reference to the device being gone — each live `DrmSurface`, the
    // `DrmDeviceNotifier`, and the seven event-source closures holding the render context
    // — and the event loop never frees its sources at all (see `drm_restore` above). A
    // missed reference there is a silently broken console, so the restore is explicit.
    //
    // A NO-OP when this session is not the one on screen: `restore_state` is guarded on
    // the device being active, which a VT switch away already cleared along with our DRM
    // master. That is correct rather than merely safe — the foreground VT owns the
    // display and has programmed its own mode, so there is nothing of ours to put back.
    //
    // `try_borrow` because panicking here would replace one bad exit with another.
    slots::sdnotify::stopping();
    #[cfg(feature = "backend-native")]
    if let Some(drm_restore) = drm_restore.as_ref() {
        match drm_restore.try_borrow() {
            Ok(manager) => {
                manager.device().restore_state();
                info!("DRM state restored");
            }
            Err(err) => warn!("DRM manager still borrowed at teardown; skipping restore: {err:?}"),
        }
    }

    // Leave the Bus before the session environment goes (bounded, about 300 ms).
    if let Some(bus) = bus.as_mut() {
        bus.shutdown();
    }
    if let Some(scenes) = scenes.take() {
        scenes.shutdown();
    }

    // Then the session environment, which outlives this process and would otherwise point
    // the next login at a socket nobody is listening on.
    shutdown::teardown(&mut state, nested);

    // Our own teardown first, so anything EGL has to say about it is still
    // reported — the compositor's contexts and surfaces go out with `state`.
    drop(state);
    // Then hand EGL's debug callback back. What remains is the driver tearing
    // down its own contexts from a library destructor that runs after `main`
    // returns, past the point where thread-local storage still exists; the
    // callback reaches through TLS for the logger and panics, and the panic hook
    // prints two backtraces over what was a clean logout. Nothing it could report
    // that late is ours to act on, so the callback goes away rather than being
    // taught to survive.
    smithay::backend::egl::ffi::unset_debug_log();
    Ok(())
}

pub mod execution;
pub mod priority;

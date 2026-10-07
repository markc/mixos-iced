use crate::camera::transform::translate::transform::Transform;
use crate::state::Loop;
use crate::state::state::{Orchestrator, StateDRMBinding};
use crate::window::interface::record::data::{DiscardPlaceholder, WindowData};
use crate::window::lifecycle::event::event::WindowLifecycleEvent;
use crate::window::lifecycle::state::lifecycle::WindowLifecycle;
use dispatcher::state::state::Dispatch;
use dispatcher::wayland::xdg::activation::dispatch::wire::ActivationDetails;
use dispatcher::wire::trait_::wire_trait::{ActivationOrigin, WireTrait};
use dispatcher::wire::wire::Wire;
use protocols::window::find::find;
use protocols::window::ident::ident;
use smithay::backend::renderer::ImportDma;
use smithay::desktop::{Space, Window};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::IsAlive;
use smithay::utils::{Logical, Physical, Point, Rectangle};
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::xdg::ToplevelSurface;
use smithay::xwayland::X11Surface;
use std::ops::DerefMut;
use std::sync::{Arc, Mutex};

impl WireTrait for Orchestrator {
    fn host_space(&self) -> &protocols::space::state::SpaceState {
        self.space_state()
    }
    fn host_space_mut(&mut self) -> &mut protocols::space::state::SpaceState {
        self.space_state_mut()
    }
    fn owning_space(&self, surface: &WlSurface) -> &protocols::space::state::SpaceState {
        match self.surface_world(surface) {
            Some(world) => {
                &self
                    .worlds
                    .get(world)
                    .storage()
                    .get(&crate::host::space::base::SPACE)
                    .inner
            }
            None => self.space_state(),
        }
    }
    fn owning_space_mut(
        &mut self,
        surface: &WlSurface,
    ) -> &mut protocols::space::state::SpaceState {
        match self.surface_world(surface) {
            Some(world) => {
                &mut self
                    .worlds
                    .get_mut(world)
                    .storage_mut()
                    .get_mut(&crate::host::space::base::SPACE_MUT)
                    .inner
            }
            None => self.space_state_mut(),
        }
    }
    fn all_world_spaces(&self) -> Vec<&protocols::space::state::SpaceState> {
        self.worlds
            .ids()
            .into_iter()
            .filter_map(|id| {
                self.worlds
                    .get(id)
                    .storage()
                    .try_get(&crate::host::space::base::SPACE)
                    .map(|w| &w.inner)
            })
            .collect()
    }

    fn remember_focus_of(&mut self, surface: &WlSurface) {
        // Resolve the focused surface to its mapped toplevel window (across every world),
        // then stash it under the world it lives on. Non-toplevel focus (layer/iced) or a
        // surface with no window → nothing to remember.
        let focused = self
            .all_world_spaces()
            .iter()
            .flat_map(|s| s.state.elements())
            .find(|w| find::is_surface(w, surface))
            .cloned();
        if let Some(window) = focused {
            if let Some(world) = self.world_of_window(&window) {
                self.world_focus_memory.insert(world, window);
            }
        }
    }

    fn restore_focus_for_current_world(&mut self) -> Option<WlSurface> {
        let target = self.worlds.spawn_target();
        // Only restore a remembered window that is still alive and still on this world;
        // a stale entry (window closed, or moved worlds) is dropped and treated as none.
        let restore = self
            .world_focus_memory
            .get(&target)
            .filter(|w| w.alive() && self.world_of_window(w) == Some(target))
            .cloned();
        match restore {
            Some(window) => {
                self.set_activated_exclusive(Some(&window));
                ident::surface(&window)
            }
            None => {
                self.world_focus_memory.remove(&target);
                self.set_activated_exclusive(None);
                None
            }
        }
    }

    fn active_output(&self) -> Option<smithay::output::Output> {
        // The monitor the user is on (cursor's output, else primary). Non-panicking
        // variant of the inherent `active_output()` so a NULL-output layer surface
        // mapped before any output exists just falls back, not aborts.
        let key = self.active_output_key();
        let space = self.space_state();
        space
            .state
            .outputs()
            .find(|o| crate::state::state::output_key(o) == key)
            .or_else(|| space.state.outputs().next())
            .cloned()
    }

    fn session_restore_size(
        &self,
        surface: &WlSurface,
    ) -> Option<smithay::utils::Size<i32, Logical>> {
        // No placeholder record exists to restore a returning session-managed
        // toplevel's size from: the client sizes itself.
        let _ = surface;
        None
    }

    fn initialize_surface_data(&mut self, window: Window) {
        let uuid = uuid::Uuid::now_v7();
        let user_data = window.user_data().get::<WindowData>();
        if user_data.is_some() {
            abort!("new_toplevel: WindowData is set")
        }
        // Window basic data ( UUID )
        window
            .user_data()
            .insert_if_missing_threadsafe(|| WindowData { UUID: uuid });
        // The comp registry record took its role earlier this drain; bind the uuid
        // to it (a changed uuid mints a new generation there).
        if let Some(handle) =
            dispatcher::wire::trait_::surface_event::SurfaceHandle::of_window(&window)
        {
            let pid = ident::pid(&window, &self.loader.display_handle);
            self.comp.bind_uuid(&handle, uuid, pid);
        }

        // Place the uuid into surface as well. required for ondestroy.
        //
        // An X11 window may have no wl_surface yet — Xwayland associates one only once
        // the client's serial round-trip completes, which can land after the map
        // request. Nothing is lost: the uuid was stamped on the window's own user data
        // above, and the destroy path reads it back from there rather than off the
        // surface.
        let Some(surface) = ident::surface(&window) else {
            info!("initialize_surface_data (x11, unassociated): {:?}", uuid);
            return;
        };
        info!("initialize_surface_Data: {:?}", uuid);
        with_states(&surface, |states| {
            let inserted = states
                .data_map
                .insert_if_missing_threadsafe(|| Mutex::new(WindowData { UUID: uuid }));
            if !inserted {
                // CHECK: Its interesting behaviour:
                // Windows (and their user_data) can completely recreated without a surface re-creation.
                // CHECK: THis means some stuff: The windowdata could be a mismatch between whats actually set in window.
                // Therefore must be updated here.
                // CHECK: But it still wont solve discrepancies with what PH expects, etc. as surface is destroyed and recreated.
                // panic!("Duplication of UUID set in surface.");
                // Replace the entire value:
                let slot = states.data_map.get::<Mutex<WindowData>>().unwrap();
                *slot.lock().unwrap() = WindowData { UUID: uuid };
            }
        });

        // Skip setting it. Activation probably occurs after toplevel as its part of the activation requirements?.
        // Extract activation details. optional
        // let activation = with_states(surface, |states| {
        //     let data = states.data_map.get::<ActivationDetails>().cloned();
        //     data
        // });

        // Activation obtain, place inside window data. Can use surface here if needed. on destroy it should remove the token if it wasnt already. important.
        // if let Some(activation) = activation {
        //     window
        //         .user_data()
        //         .insert_if_missing_threadsafe(|| activation);
        // }
    }

    fn destroy_surface_data(&mut self, surface: ToplevelSurface, drag_discard: bool) {
        let activation_details =
            dispatcher::wayland::xdg::activation::dispatch::wire::activations(surface.wl_surface());

        info!("destroy_surface_data...");
        with_states(surface.wl_surface(), |states| {
            let data = states
                .data_map
                .get::<Mutex<WindowData>>()
                .unwrap_or_else(|| abort!("toplevels to have window data"))
                .lock()
                .unwrap()
                .UUID
                .clone();
            info!("destroy_surface_data: {:?}", data);
            // Shift-close mark: surface user data set by the selection toolbar —
            // the placeholder destroy path must not spawn a placeholder for this window.
            // `drag_discard` is the same verdict reached the other way: a torn-off
            // tab destroyed mid-drag because another toplevel adopted it.
            //
            // `Ephemeral` is the third route to the same verdict, and the only one
            // the user never asked for: a modal dialog, or a window from a
            // `NoDisplay=true` entry (the portal file chooser being the one that
            // actually bites). Both facts were latched while the window lived —
            // neither is still readable here.
            let discard_placeholder = drag_discard
                || states.data_map.get::<DiscardPlaceholder>().is_some()
                || protocols::ephemeral::mark::mark::is_marked(states);
            self.window_lifecycle_mut()
                .incoming
                .push(WindowLifecycleEvent::Destroyed(
                    data,
                    activation_details,
                    discard_placeholder,
                ));
        });
    }

    // Every world, not `host_space`: an X11 window unmaps and dies in the world it
    // lives in, which is not necessarily the one on screen. See the trait doc.
    fn owning_x11_window(&self, surface: &X11Surface) -> Option<(uuid::Uuid, Window)> {
        // The withdrawn record first, and it is cheap: one hash lookup against a map that
        // is empty in the ordinary case, versus a walk of every world's elements. A
        // window is in exactly one of the two, so order is a matter of cost only.
        if let Some((world, window, _)) = self.withdrawn_x11.get(&surface.window_id()) {
            return Some((*world, window.clone()));
        }
        self.worlds.ids().into_iter().find_map(|id| {
            let found = self
                .worlds
                .get(id)
                .storage()
                .try_get(&crate::host::space::base::SPACE)?
                .inner
                .state
                .elements();
            let found = find::by_x11(found, surface)?;
            Some((id, found))
        })
    }

    fn space_of_world_mut(
        &mut self,
        world: uuid::Uuid,
    ) -> &mut protocols::space::state::SpaceState {
        self.space_of_mut(world)
    }

    fn destroy_x11_data(&mut self, window: Window) {
        use crate::window::interface::record::window::LoopWindow;
        let Some(uuid) = window.uuid() else { return };

        // An X11 window carries no xdg-activation token (that is a wayland protocol),
        // so there is nothing to retire alongside it.
        //
        // Both verdicts are read off the WINDOW, never the surface. smithay clears the
        // wl_surface association inside `unmapped_window` — the very event that queued
        // this destroy — so by the time the drain runs there is no surface left, and a
        // mark read from there would be unconditionally absent: every ephemeral X11
        // window would leave a placeholder, and Shift-close would never discard one.
        let discard_placeholder = window.user_data().get::<DiscardPlaceholder>().is_some()
            || protocols::ephemeral::mark::mark::is_window_marked(&window);
        self.window_lifecycle_mut()
            .incoming
            .push(WindowLifecycleEvent::Destroyed(
                uuid,
                Vec::new(),
                discard_placeholder,
            ));
    }

    fn withdraw_x11(&mut self, world: uuid::Uuid, window: Window) {
        use crate::window::interface::record::window::LoopWindow;
        let Some(uuid) = window.uuid() else { return };
        // WHERE it sat, taken before the caller unmaps it — `element_location` answers
        // from the Space, so this is the last moment the position exists anywhere.
        let at = self
            .space_of_mut(world)
            .state
            .element_location(&window)
            .unwrap_or_default();
        if let Some(x11) = window.x11_surface() {
            self.withdrawn_x11
                .insert(x11.window_id(), (world, window.clone(), at));
        }
        // Out of the Space only NOW: the location above had to be read while it was still
        // an element, and `element_location` answers from the Space.
        self.space_of_mut(world).state.unmap_elem(&window);
        // Then the WITHDRAW teardown, which is not the destroy one. Both leave a
        // placeholder — X11 cannot tell a hide from a close, so both must — but a destroy
        // moves the window's record into the placeholder while a withdrawal copies it and
        // leaves the original in place. That is what keeps the invariant a remap depends
        // on: the window still has its own record when it comes back.
        //
        // The verdicts are read off the WINDOW, as at destroy: smithay clears the surface
        // association inside `unmapped_window`, so a mark read from there is gone by now.
        let discard = window.user_data().get::<DiscardPlaceholder>().is_some()
            || protocols::ephemeral::mark::mark::is_window_marked(&window);
        self.window_lifecycle_mut()
            .incoming
            .push(WindowLifecycleEvent::Withdrawn(uuid, discard));
    }

    fn readmit_x11(&mut self, window: Window) {
        use crate::window::interface::record::window::LoopWindow;
        let Some(uuid) = window.uuid() else { return };
        let Some(x11) = window.x11_surface().map(|s| s.window_id()) else {
            return;
        };
        let Some((world, _, at)) = self.withdrawn_x11.remove(&x11) else {
            return;
        };
        // Back in its own world at its own position — not `host_space`, and not centred.
        // A window that hid while the user was elsewhere returns where it left.
        let override_redirect = window
            .x11_surface()
            .is_some_and(|s| s.is_override_redirect());
        let pid = ident::pid(&window, &self.loader.display_handle);
        // A role take clears the names; the X window still has them.
        let names = window
            .x11_surface()
            .map(dispatcher::wire::trait_::surface_event::SurfaceEvent::x11_names);
        self.space_of_mut(world)
            .state
            .map_element(window, at, false);
        self.comp.readmit(
            dispatcher::wire::trait_::surface_event::SurfaceHandle::X11(x11),
            override_redirect,
            uuid,
            pid,
        );
        if let Some(names) = names {
            self.comp.apply(names);
        }
        // The draw-order slot, which `_withdraw` dropped: the window was not drawn while
        // hidden, and `raise_drawable` both re-registers an unknown id and puts it back on
        // top, which is what a window reappearing should do.
        self.raise_drawable(uuid);
        // The placeholder the withdrawal left is KEPT — it is a placeholder in its own
        // right, with its own uuid, standing for a close that X11 could not distinguish
        // from a hide. The window's OWN record never moved, so the invariant every
        // `modify` caller relies on holds without anyone guarding it.
    }

    fn surface_event(&mut self, event: dispatcher::wire::trait_::surface_event::SurfaceEvent) {
        // A window took the primary seat's keyboard (a click, comp.window.focus,
        // an activation, focus-on-map, a workspace switch): the compositor iced
        // surface that held the keyboard lets go, since keys go iced-first when
        // the registry holds them (seat should_forward). Moving the seat's
        // focus to None (a click on iced, a scene dialog's grab) clears nothing.
        if let dispatcher::wire::trait_::surface_event::SurfaceEvent::Focus(Some(_)) = &event
            && let Some(registry) = self.surface_mut().registry.as_mut()
        {
            registry.set_keyboard_focus(None);
        }
        self.comp.apply(event);
    }

    fn committed_input_geometry(&mut self, surface: &WlSurface) {
        let mut root = surface.clone();
        while let Some(parent) = smithay::wayland::compositor::get_parent(&root) {
            root = parent;
        }
        if let Some(window) = find::in_space(&self.owning_space(&root).state, &root)
            && crate::comp::input_geometry::observe(&window)
        {
            self.comp.mark_input_geometry_dirty();
        }
    }

    fn forget_withdrawn_x11(&mut self, surface: &X11Surface) {
        self.withdrawn_x11.remove(&surface.window_id());
    }

    fn is_space_element(&self, window: &Window) -> bool {
        self.worlds.ids().into_iter().any(|id| {
            self.worlds
                .get(id)
                .storage()
                .try_get(&crate::host::space::base::SPACE)
                .is_some_and(|w| w.inner.state.elements().any(|e| e == window))
        })
    }

    fn place_window(&mut self, window: Window, geometry: Rectangle<i32, Logical>) {
        // Side effect- because of the dispatcher wiring problem, the place_window returns a location rather than calling map_element.
        // However this function should be treated as if it called the initial space map_element call.
        self.window_lifecycle_mut()
            .incoming
            .push(WindowLifecycleEvent::InitialMap(window));
    }

    fn fullscreen_request(&mut self, window: Window, fullscreen: bool) {
        self.window_lifecycle_mut()
            .incoming
            .push(WindowLifecycleEvent::Fullscreen(window, fullscreen));
    }

    fn request_activation(&mut self, window: Window, origin: ActivationOrigin) {
        self.window_lifecycle_mut()
            .incoming
            .push(WindowLifecycleEvent::Activate(window, origin));
    }

    fn settle_toplevel_drag(&mut self, surface: WlSurface) {
        self.window_lifecycle_mut()
            .incoming
            .push(WindowLifecycleEvent::DragSettled(surface));
    }

    fn surface_point_to_world(
        &self,
        surface: &WlSurface,
        local: Point<f64, Logical>,
    ) -> Option<Point<f64, Logical>> {
        crate::camera::transform::translate::map::surface_map(&self.host_space().state, surface)
            .map(|m| m.to_world(local))
    }

    fn apply_pointer(&mut self, storage_point: Point<f64, Logical>) {
        // Read the hosted space's output geometry, then drop the borrow before
        // mutating camera/pointer (both live in the same world storage).
        let (mode_size, scale) = {
            // The cursor's output (via current/cursor resolution), not the primary —
            // a pointer warp onto a secondary monitor must project through THAT
            // monitor's mode/scale to land at the right physical position.
            let output = self.current_output();
            let mode = output
                .current_mode()
                .unwrap_or_else(|| abort!("output has a current mode"));
            (mode.size, output.current_scale().fractional_scale())
        };
        let mode = smithay::output::Mode {
            size: mode_size,
            refresh: 0,
        };
        let camera = &self.camera().transform;

        let ctx = crate::camera::transform::translate::transform::Context::new(
            (camera.position.x, camera.position.y),
            camera.zoom,
            (mode.size.w as f64, mode.size.h as f64),
            scale,
        );

        let warp_phys: Point<f64, Physical> = {
            let t: Transform = (storage_point, ctx).into();
            t.into()
        };
        let pointer = self.pointer_mut();
        pointer.motion.x = warp_phys.x;
        pointer.motion.y = warp_phys.y;

        // Reset for camera as well.
        self.camera_mut().position_previous = pointer.motion;
    }

    fn reanchor_pointer(&mut self) -> Point<f64, Logical> {
        // Resolve against the CURSOR's monitor, not the one being drawn. The caller
        // runs from the frame hook, which on a multi-monitor session is inside the
        // per-output loop with `render_output` set — and `current_output()` /
        // `camera()` answer that first. Projecting the cursor through a monitor it
        // is not on would place it by another mode, scale and camera. Dropped for
        // the duration so both fall back to `cursor_output`, which is the resolution
        // every other pointer path gets; `render_target` is already unset this early
        // in the frame, so `camera()` gives the focused pane either way — the same
        // full-output view `reconcile_finger_pan` compares against later this frame.
        let render_output = self.render_output.take();
        let anchored = self.reanchor_pointer_here();
        self.render_output = render_output;
        anchored
    }

    fn dmabuf_import(
        &mut self,
        dispatch: &mut Dispatch,
        _global: &smithay::wayland::dmabuf::DmabufGlobal,
        _dmabuf: smithay::backend::allocator::dmabuf::Dmabuf,
        notifier: smithay::wayland::dmabuf::ImportNotifier,
    ) -> Option<(
        smithay::backend::allocator::dmabuf::Dmabuf,
        smithay::wayland::dmabuf::ImportNotifier,
    )> {
        // REFUSE what the compositing renderer cannot import, before validating.
        // The validation below runs on GLES whatever composites, so a format only
        // GLES can take passed here and then failed at draw — every frame, as a
        // blank window, with nothing the client could react to. `failed()` is a
        // protocol answer it CAN react to (fall back to another format, or shm).
        // Fourcc-level: a v3/wl_drm client sends modifier INVALID and must not be
        // judged on the modifier. Advertising is narrowed the same way, so a
        // well-behaved client never reaches this.
        {
            let format = smithay::backend::allocator::Buffer::format(&_dmabuf);
            let (code, modifier) = (format.code, format.modifier);
            // The MODIFIER is judged too, not just the fourcc. A legacy v3/`wl_drm` client
            // sends `INVALID`, and the vulkan import path has no way to take it: it builds
            // `VkImageDrmFormatModifierExplicitCreateInfoEXT` from the buffer's modifier, and
            // no device lists INVALID among its supported ones, so `vkCreateImage` fails and
            // the window never draws. Judging the pair turns that into `failed()`, which the
            // client can act on — fall back to shm, or renegotiate.
            //
            // This does NOT punish the gles path: it publishes its EGL set, which carries an
            // INVALID entry per fourcc, so an implicit buffer still passes there. One rule,
            // no renderer detection — the published set answers for whoever is compositing.
            if !render_gles::format::resolve::resolve::importable_pair(
                self.kernel
                    .get(&render_gles::format::registrar::registrar::FORMATS),
                code,
                modifier,
            ) {
                warn!(
                    "Refusing client dmabuf {code:?} modifier {modifier:?}: the compositing \
                     renderer cannot import that pair (nor is it in the advertised set)"
                );
                dispatch.dmabuf_ledger.record_failed(
                    format,
                    dispatcher::wayland::dmabuf::ledger::reason::INVALID_METADATA,
                    "format/modifier pair not importable by the compositing renderer",
                );
                notifier.failed();
                return None;
            }
        }

        let Some(gpu_ref) = self.kernel.get(&crate::state::state::GPU_BINDING).as_ref() else {
            return Some((_dmabuf, notifier));
        };

        // Borrow once, hold the guard for the whole operation.
        let mut binding = gpu_ref.borrow_mut();

        // Split borrow: extract a mutable reference to `gpus` and an immutable
        // reference to `primary` from the same guard. The compiler can do this
        // because field accesses are disjoint.
        let StateDRMBinding { gpus, primary, .. } = &mut *binding;

        // Validate on the device that will DRAW this buffer, not on the scanout
        // device. They are the same until `render_node` moves the composite, and
        // then validating on the wrong one accepts a buffer the draw path cannot
        // import — blank windows with a success reported to the client.
        //
        // `single_renderer` enumerates on demand and returns `NoDevice` for a node
        // it cannot reach, which falls through to the default import below. So a
        // node without a usable GL driver degrades rather than rejecting clients.
        let node = render_gles::format::registrar::registrar::composite_node(*primary);
        let mut renderer = match gpus.single_renderer(&node) {
            Ok(r) => r,
            Err(err) => {
                // ONCE PER NODE, not once per buffer. This fires for every client dmabuf a
                // client commits — thousands per minute — and it says the same thing every
                // time, because the condition is static for the session: a node the GLES
                // `GpuManager` cannot reach now will not become reachable later.
                //
                // `NoDevice` here is EXPECTED on a split machine and is not a fault. The
                // composite node is `render_node`, and this manager is the GLES multigpu one
                // paired with the SCANOUT device — so when the composite moved to a separate
                // GPU, there is legitimately no GL device here for it. The buffer still gets
                // smithay's default import, and the fourcc-level refusal above has already
                // applied the compositing renderer's own answer.
                static WARNED: std::sync::OnceLock<
                    std::sync::Mutex<std::collections::HashSet<i64>>,
                > = std::sync::OnceLock::new();
                let seen = WARNED.get_or_init(Default::default);
                let first = seen
                    .lock()
                    .map(|mut g| g.insert(node.dev_id() as i64))
                    .unwrap_or(false);
                if first {
                    warn!(
                        "No GLES renderer for the composite node {:?} (err={err:?}); client \
                         dmabufs fall through to the default import for the rest of this \
                         session. Expected when the composite is on a separate GPU from the \
                         scanout device. Logged once per node.",
                        node.dev_path()
                    );
                } else {
                    trace!(
                        "dmabuf import: still no GLES renderer for {:?}",
                        node.dev_path()
                    );
                }
                // Already moved..
                return Some((_dmabuf, notifier));
            }
        };

        match renderer.import_dmabuf(&_dmabuf, None) {
            Ok(_) => {
                dispatch.dmabuf_ledger.record_accepted();
                if let Err(err) = notifier.successful::<Dispatch>() {
                    warn!(
                        "Dmabuf explicit imported successfully, butclient disappeared while signaling dmabuf import success: err={err:?}"
                    );
                } else {
                    // tracing::info!("Dmabuf explicit imported successfully");
                }
                // // Remains unused
                // let _ = notifier.successful::<Dispatch>();
            }
            Err(err) => {
                warn!("Failed to import client dmabuf: err={err:?}");
                dispatch.dmabuf_ledger.record_failed(
                    smithay::backend::allocator::Buffer::format(&_dmabuf),
                    dispatcher::wayland::dmabuf::ledger::reason::GLES_REJECTED,
                    format!("{err}"),
                );
                notifier.failed();
            }
        }

        None // we handled it
    }
}

impl Orchestrator {
    /// [`WireTrait::reanchor_pointer`]'s body, with the output already resolved by
    /// the caller (see there for why it has to be).
    fn reanchor_pointer_here(&mut self) -> Point<f64, Logical> {
        // The same full-output context `apply_pointer` builds, for the same reason:
        // the cursor is screen-space content, not pane content.
        let (mode_size, scale) = {
            let output = self.current_output();
            let mode = output
                .current_mode()
                .unwrap_or_else(|| abort!("output has a current mode"));
            (mode.size, output.current_scale().fractional_scale())
        };
        let (pw, ph) = (mode_size.w as f64, mode_size.h as f64);
        let camera = &self.camera().transform;
        let ctx = crate::camera::transform::translate::transform::Context::new(
            (camera.position.x, camera.position.y),
            camera.zoom,
            (pw, ph),
            scale,
        );

        // Pin into the output BEFORE projecting. A world that has never been entered
        // starts its accumulator at physical `(0, 0)` — the top-left corner — and a
        // switch that follows a mode change or an unplug can leave it outside the
        // panel entirely. Either way the incoming world would start with the cursor
        // at (or past) an extent, which the screen-extent policy reads as a push.
        let motion = self.pointer().motion;
        let phys = Point::<f64, Physical>::from((motion.x.clamp(0.0, pw), motion.y.clamp(0.0, ph)));
        let world: Point<f64, Logical> = {
            let t: Transform = (phys, ctx).into();
            t.into_storage_point_f64()
        };

        let pointer = self.pointer_mut();
        pointer.motion.x = phys.x;
        pointer.motion.y = phys.y;
        // Nothing is pushing an edge across a switch, and the hold is per-world.
        pointer.edge_hold = None;
        // The camera's own screen accumulator is the SAME point, and left stale it
        // flings the camera by the whole switch on the first press-drag —
        // `apply_pointer` and `pointer.pan::reconcile_finger_pan` pair these two
        // writes for exactly this reason.
        self.camera_mut().position_previous = Point::from((phys.x, phys.y));
        world
    }
}
// Problem:
// Also, calling request_activation is not set until new top level. place window needs to check the placeholder restoration, not top_level.
// the activation (request_activation) is for focus and raising. like clicking a link sends a dbus to activate a new window in chrome. it is not necessarily init behavior.
// this can be great afterwards for navigator
//
// OK. Here is what I've done:
// 1. request_activation now wired. it sets surface data with the activation token. I assume it is called before new top level. When a restoration is deleted/consumed, i remove its relevant token.
//
// pub struct ActivationDetails{
//     token: XdgActivationToken,
//     token_data: XdgActivationTokenData,
// }
//
// pub fn request_activation<WireObject: DispatchWire>(
//     dispatch: &mut Dispatch<WireObject>,
//     surface: WlSurface,
//     token: XdgActivationToken,
//     token_data: XdgActivationTokenData,
// ) {
//     with_states(
//         &surface,
//         |states| {
//             let inserted = states
//                 .data_map
//                 .insert_if_missing_threadsafe(|| ActivationDetails{
//                     token,
//                     token_data,
//                 });
//             if !inserted {
//                 panic!("Duplication of token data set in surface."); // Dev safeguards
//             }
//         },
//     );
// }
// 2. When the surface is destroyed, the token is cleared:
//          xdg_activation.remove_token(&activation.token); // <-- a cloned activation_token based on toplevel surface data.
//    GC: Important. However hard to manage in this scope. Clients may have their own tokens and it may be used for actual activation stuff like raising the window.
// 3.
//
// CHECK: SKipped for now. GC later. important nevertheless. Tokens remain in store and clients may use their own tokens.,
// 3. Attached a 1 minute timer to calloop to retain only non stale tokens. If a token did never activate then it must be removed since its surface is non existent.
// external tokens: if a single toplevel has only a single token, it may not be accurate.
// So no GC for now.

// When a restoration is deleted/consumed, i remove its relevant token.

//! [`TestHost`]: the `WireTrait` host the harness runs `Dispatch` against.
//!
//! Mirrors the `Orchestrator`'s answers where a headless engine can give them
//! (`world::state::wire`): registry events go straight to `CompState`, a new
//! window gets a uuid v7 bound to its record, `place_window` queues the initial
//! map for renderer-free lifecycle service. Everything that needs worlds, a
//! camera, placeholders or a renderer stays headless. A second Space lets tests
//! exercise foreign activation across worlds.

use dispatcher::state::state::Dispatch;
use dispatcher::wire::trait_::surface_event::{SurfaceEvent, SurfaceHandle};
use dispatcher::wire::trait_::wire_trait::{ActivationOrigin, WireTrait};
use protocols::space::state::SpaceState;
use protocols::window::ident::ident;
use world::comp::CompState;
use world::window::interface::data::data::WindowData;
use world::window::interface::record::window::LoopWindow;
use world::window::lifecycle::event::event::WindowLifecycleEvent;
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::desktop::{Space, Window};
use smithay::output::{Mode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Point, Rectangle, Size, Transform};
use smithay::wayland::dmabuf::{DmabufGlobal, ImportNotifier};
use smithay::wayland::shell::xdg::ToplevelSurface;
use smithay::xwayland::X11Surface;

/// The fake output's mode.
pub const OUTPUT_SIZE: (i32, i32) = (1920, 1080);

pub struct TestHost {
    pub space: SpaceState,
    pub other_space: SpaceState,
    pub active_world: usize,
    pub world_ids: [uuid::Uuid; 2],
    /// The comp registry, exactly as the `Orchestrator` holds it.
    pub comp: CompState,
    pub output: Output,
    display_handle: DisplayHandle,
    /// Windows the protocol drain queued for lifecycle service.
    to_place: Vec<(Window, Rectangle<i32, Logical>)>,
    /// Per-world queues, indexed by stable world identity like production.
    pub lifecycle: [Vec<WindowLifecycleEvent>; 2],
    withdrawn_x11: std::collections::HashMap<u32, (uuid::Uuid, Window, Point<i32, Logical>)>,
    /// Applied events, for order and exactly-once assertions.
    pub serviced_lifecycle: Vec<(uuid::Uuid, &'static str)>,
    /// Real per-world draw-order registries; indices stay stable across swaps.
    pub draw_orders: [world::order::track::base::DrawOrder; 2],
}

impl TestHost {
    pub fn new(display_handle: DisplayHandle) -> Self {
        let output = Output::new(
            "testkit-0".to_string(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "testkit".to_string(),
                model: "headless".to_string(),
                serial_number: String::new(),
            },
        );
        let mode = Mode {
            size: OUTPUT_SIZE.into(),
            refresh: 60_000,
        };
        output.change_current_state(
            Some(mode),
            Some(Transform::Normal),
            Some(Scale::Integer(1)),
            Some((0, 0).into()),
        );
        output.set_preferred(mode);
        let mut space = Space::default();
        space.map_output(&output, (0, 0));
        let mut other_space = Space::default();
        other_space.map_output(&output, (0, 0));
        Self {
            space: SpaceState { state: space },
            other_space: SpaceState { state: other_space },
            active_world: 0,
            world_ids: std::array::from_fn(|_| uuid::Uuid::now_v7()),
            comp: CompState::default(),
            output,
            display_handle,
            to_place: Vec::new(),
            lifecycle: std::array::from_fn(|_| Vec::new()),
            withdrawn_x11: std::collections::HashMap::new(),
            serviced_lifecycle: Vec::new(),
            draw_orders: std::array::from_fn(|_| world::order::track::base::DrawOrder::new()),
        }
    }

    /// Simulate renderer-free initial map service (frames `drain_lifecycle`): each
    /// window the drain placed goes into the Space at its placement and the
    /// registry hears `Placed`. Returns how many windows were placed. A window
    /// that died, lost its role or left the Space is dropped; a live window
    /// waiting for its buffer or owning world retains only its per-window tail.
    /// Uses production deferral and eligibility checks; placement/focus policy
    /// still needs a Loop.
    pub fn service_lifecycle(&mut self) -> usize {
        let mut placed = 0;
        let mut waiting = Vec::new();
        let queue_world = self.active_world;
        for event in std::mem::take(&mut self.lifecycle[queue_world]) {
            if event.defer_for_placement(
                &self.comp,
                self.all_world_spaces(),
                &self.space,
                &waiting,
            ) {
                waiting.push(event);
                continue;
            }
            match event {
                WindowLifecycleEvent::InitialMap(window) => {
                    let index = self
                        .to_place
                        .iter()
                        .position(|(candidate, _)| candidate == &window)
                        .expect("a queued map has placement geometry");
                    let (_, geometry) = self.to_place.remove(index);
                    let Some(owner) = world::comp::live_window_space(
                        &self.comp,
                        self.all_world_spaces(),
                        &window,
                    ) else {
                        continue;
                    };
                    if !world::comp::initial_map_is_live(&self.comp, &owner.state, &window) {
                        continue;
                    }
                    debug_assert!(std::ptr::eq(owner, &self.space));
                    self.space
                        .state
                        .map_element(window.clone(), geometry.loc, false);
                    if let Some(handle) = SurfaceHandle::of_window(&window) {
                        self.surface_event(SurfaceEvent::Placed(handle));
                    }
                    self.serviced_lifecycle
                        .push((window.uuid().unwrap(), "placed"));
                    self.draw_orders[self.active_world].insert_top(
                        world::order::track::base::ComponentId(window.uuid().unwrap()),
                        world::order::track::base::DrawLayer::CONTENT,
                    );
                    placed += 1;
                }
                WindowLifecycleEvent::Activate(window, _) => {
                    if world::comp::live_window_space(&self.comp, self.all_world_spaces(), &window)
                        .is_none()
                    {
                        continue;
                    }
                    if self.space.state.element_location(&window).is_none() {
                        self.switch_world();
                    }
                    self.space.state.raise_element(&window, true);
                    for space in self.all_world_spaces() {
                        for candidate in space.state.elements() {
                            candidate.set_activated(candidate == &window);
                            protocols::window::shell::shell::send_pending(candidate);
                        }
                    }
                    self.serviced_lifecycle
                        .push((window.uuid().unwrap(), "activated"));
                }
                WindowLifecycleEvent::Fullscreen(window, fullscreen) => {
                    if let Some(owner) = world::comp::live_window_space(
                        &self.comp, self.all_world_spaces(), &window,
                    ) {
                        let hosted = std::ptr::eq(owner, &self.space);
                        let target = owner.state.outputs_for_element(&window).into_iter().next()
                            .or_else(|| owner.state.outputs().next().cloned())
                            .and_then(|output| owner.state.output_geometry(&output))
                            .map(|geometry| (geometry.loc, geometry.size));
                        let space = if hosted { &mut self.space } else { &mut self.other_space };
                        world::window::interface::draw::fullscreen::fullscreen_set_in_space(
                            &mut space.state, &window, fullscreen, target, hosted,
                        );
                        self.serviced_lifecycle
                            .push((window.uuid().unwrap(), "fullscreen"));
                    }
                }
                WindowLifecycleEvent::Destroyed(uuid, _, _) => {
                    self.remove_drawable(uuid);
                    self.serviced_lifecycle.push((uuid, "destroyed"));
                }
                WindowLifecycleEvent::DragSettled(_) => {}
            }
        }
        // Match frames' retained-tail migration, even if activation switched
        // the hosted world while this queue was being drained.
        let mut retained: [Vec<WindowLifecycleEvent>; 2] = std::array::from_fn(|_| Vec::new());
        for event in waiting {
            let owner = event.owning_space(self.all_world_spaces())
                .map(|space| if std::ptr::eq(space, &self.space) {
                    self.active_world
                } else {
                    self.active_world ^ 1
                }).unwrap_or(queue_world);
            retained[owner].push(event);
        }
        for (owner, mut events) in retained.into_iter().enumerate() {
            events.append(&mut self.lifecycle[owner]);
            self.lifecycle[owner] = events;
        }
        placed
    }

    /// Use the production routing rule for every lifecycle producer.
    pub fn enqueue_lifecycle(&mut self, event: WindowLifecycleEvent) {
        let owner = event.pending_queue(self.lifecycle.iter().enumerate()
            .map(|(owner, events)| (owner, events.as_slice())))
            .unwrap_or(self.active_world);
        self.lifecycle[owner].push(event);
    }

    /// Swap the hosted and parked Spaces without moving any windows.
    pub fn switch_world(&mut self) {
        std::mem::swap(&mut self.space, &mut self.other_space);
        self.active_world ^= 1;
    }

    fn remove_drawable(&mut self, uuid: uuid::Uuid) {
        // Teardown can follow an activation, and Space unmap can precede both.
        world::order::track::base::DrawOrder::remove_from_all(
            world::order::track::base::ComponentId(uuid), &mut self.draw_orders,
        );
    }

    /// Active frames service the same lifecycle queue.
    pub fn tick_frame(&mut self) -> usize {
        self.service_lifecycle()
    }

    /// Windows waiting for lifecycle service.
    pub fn pending_placements(&self) -> usize {
        self.to_place.len()
    }
}

impl WireTrait for TestHost {
    fn host_space(&self) -> &SpaceState {
        &self.space
    }
    fn host_space_mut(&mut self) -> &mut SpaceState {
        &mut self.space
    }
    fn owning_space(&self, surface: &WlSurface) -> &SpaceState {
        if self
            .other_space
            .state
            .elements()
            .any(|window| protocols::window::find::find::is_surface(window, surface))
        {
            &self.other_space
        } else {
            &self.space
        }
    }
    fn owning_space_mut(&mut self, surface: &WlSurface) -> &mut SpaceState {
        if self
            .other_space
            .state
            .elements()
            .any(|window| protocols::window::find::find::is_surface(window, surface))
        {
            &mut self.other_space
        } else {
            &mut self.space
        }
    }
    fn all_world_spaces(&self) -> Vec<&SpaceState> {
        vec![&self.space, &self.other_space]
    }
    fn active_output(&self) -> Option<Output> {
        Some(self.output.clone())
    }
    fn initialize_surface_data(&mut self, window: Window) {
        // Match the Orchestrator's UUID stamp so per-window lifecycle events
        // can be ordered against teardown as well as Window-bearing events.
        if let Some(handle) = SurfaceHandle::of_window(&window) {
            let pid = ident::pid(&window, &self.display_handle);
            let uuid = uuid::Uuid::now_v7();
            window
                .user_data()
                .insert_if_missing(|| WindowData { UUID: uuid });
            self.comp.bind_uuid(&handle, uuid, pid);
            if let Some(surface) = ident::surface(&window) {
                smithay::wayland::compositor::with_states(&surface, |states| {
                    states.data_map.insert_if_missing_threadsafe(|| {
                        std::sync::Mutex::new(WindowData { UUID: uuid })
                    });
                });
            }
        }
    }
    fn session_restore_size(&self, _surface: &WlSurface) -> Option<Size<i32, Logical>> {
        None
    }
    fn destroy_surface_data(&mut self, surface: ToplevelSurface, drag_discard: bool) {
        // The real host leaves this to the Space's `refresh`; there is no frame
        // loop here to run it, so the window leaves the Space now.
        // Keep queued candidates: lifecycle service must reject stale work,
        // rather than relying on this test host to filter it first.
        let gone = self
            .all_world_spaces()
            .into_iter()
            .flat_map(|space| space.state.elements())
            .find(|window| window.toplevel() == Some(&surface))
            .cloned();
        if let Some(window) = gone {
            self.owning_space_mut(surface.wl_surface())
                .state
                .unmap_elem(&window);
            self.enqueue_lifecycle(WindowLifecycleEvent::Destroyed(
                window.uuid().unwrap(),
                Vec::new(),
                drag_discard,
            ));
        }
    }
    fn owning_x11_window(&self, surface: &X11Surface) -> Option<(uuid::Uuid, Window)> {
        if let Some((world, window, _)) = self.withdrawn_x11.get(&surface.window_id()) {
            return Some((*world, window.clone()));
        }
        [(&self.space, self.active_world), (&self.other_space, self.active_world ^ 1)]
            .into_iter().find_map(|(space, owner)| {
                protocols::window::find::find::by_x11(space.state.elements(), surface)
                    .map(|window| (self.world_ids[owner], window))
            })
    }
    fn space_of_world_mut(&mut self, world: uuid::Uuid) -> &mut SpaceState {
        assert!(self.world_ids.contains(&world));
        if self.world_ids[self.active_world] == world {
            &mut self.space
        } else {
            &mut self.other_space
        }
    }
    fn destroy_x11_data(&mut self, window: Window) {
        if let Some(uuid) = window.uuid() {
            self.enqueue_lifecycle(WindowLifecycleEvent::Destroyed(uuid, Vec::new(), false));
        }
    }
    fn withdraw_x11(&mut self, world: uuid::Uuid, window: Window) {
        let Some(uuid) = window.uuid() else { return };
        let at = self.space_of_world_mut(world).state.element_location(&window).unwrap_or_default();
        if let Some(surface) = window.x11_surface() {
            self.withdrawn_x11.insert(surface.window_id(), (world, window.clone(), at));
        }
        self.space_of_world_mut(world).state.unmap_elem(&window);
        self.remove_drawable(uuid);
        self.serviced_lifecycle.push((uuid, "withdrawn"));
    }
    fn readmit_x11(&mut self, window: Window) {
        let Some(uuid) = window.uuid() else { return };
        let Some(surface) = window.x11_surface() else { return };
        let xid = surface.window_id();
        let override_redirect = surface.is_override_redirect();
        let names = SurfaceEvent::x11_names(surface);
        let pid = ident::pid(&window, &self.display_handle);
        let Some((world, _, at)) = self.withdrawn_x11.remove(&xid) else { return };
        self.space_of_world_mut(world).state.map_element(window, at, false);
        self.comp.readmit(SurfaceHandle::X11(xid), override_redirect, uuid, pid);
        self.comp.apply(names);
        let owner = self.world_ids.iter().position(|id| *id == world).unwrap();
        self.draw_orders[owner].raise(world::order::track::base::ComponentId(uuid));
    }
    fn is_space_element(&self, window: &Window) -> bool {
        self.all_world_spaces()
            .iter()
            .any(|space| space.state.element_location(window).is_some())
    }
    fn forget_withdrawn_x11(&mut self, surface: &X11Surface) {
        self.withdrawn_x11.remove(&surface.window_id());
    }
    fn apply_pointer(&mut self, _storage_point: Point<f64, Logical>) {}
    fn surface_point_to_world(
        &self,
        _surface: &WlSurface,
        _local: Point<f64, Logical>,
    ) -> Option<Point<f64, Logical>> {
        None
    }
    fn reanchor_pointer(&mut self) -> Point<f64, Logical> {
        Point::from((0.0, 0.0))
    }
    fn place_window(&mut self, window: Window, geometry: Rectangle<i32, Logical>) {
        self.enqueue_lifecycle(WindowLifecycleEvent::InitialMap(window.clone()));
        self.to_place.push((window, geometry));
    }
    fn fullscreen_request(&mut self, window: Window, fullscreen: bool) {
        self.enqueue_lifecycle(WindowLifecycleEvent::Fullscreen(window, fullscreen));
    }
    fn request_activation(&mut self, window: Window, origin: ActivationOrigin) {
        self.enqueue_lifecycle(WindowLifecycleEvent::Activate(window, origin));
    }
    fn settle_toplevel_drag(&mut self, surface: WlSurface) {
        self.enqueue_lifecycle(WindowLifecycleEvent::DragSettled(surface));
    }
    fn dmabuf_import(
        &mut self,
        _dispatch: &mut Dispatch,
        _global: &DmabufGlobal,
        _dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) -> Option<(Dmabuf, ImportNotifier)> {
        // No renderer to import into: refuse, which the client can act on.
        notifier.failed();
        None
    }
    fn remember_focus_of(&mut self, _surface: &WlSurface) {}
    fn restore_focus_for_current_world(&mut self) -> Option<WlSurface> {
        None
    }
    fn surface_event(&mut self, event: SurfaceEvent) {
        self.comp.apply(event);
    }
}

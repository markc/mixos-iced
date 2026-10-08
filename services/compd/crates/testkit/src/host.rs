//! [`TestHost`]: the `WireTrait` host the harness runs `Dispatch` against.
//!
//! Mirrors the `Orchestrator`'s answers where a headless engine can give them
//! (`world::state::wire`): registry events go straight to `CompState`, a new
//! window gets a uuid v7 bound to its record, `place_window` queues the initial
//! map for the frame step. Everything that needs worlds, a camera, placeholders
//! or a renderer answers as a single-world compositor with nothing to say.

use dispatcher::state::state::Dispatch;
use dispatcher::wire::trait_::surface_event::{SurfaceEvent, SurfaceHandle};
use dispatcher::wire::trait_::wire_trait::{ActivationOrigin, WireTrait};
use protocols::space::state::SpaceState;
use protocols::window::ident::ident;
use world::comp::CompState;
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::desktop::{Space, Window};
use smithay::output::{Mode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{IsAlive, Logical, Point, Rectangle, Size, Transform};
use smithay::wayland::dmabuf::{DmabufGlobal, ImportNotifier};
use smithay::wayland::shell::xdg::ToplevelSurface;
use smithay::xwayland::X11Surface;

/// The fake output's mode.
pub const OUTPUT_SIZE: (i32, i32) = (1920, 1080);

pub struct TestHost {
    pub space: SpaceState,
    /// The comp registry, exactly as the `Orchestrator` holds it.
    pub comp: CompState,
    pub output: Output,
    display_handle: DisplayHandle,
    /// Windows the drain placed, waiting for the frame step.
    to_place: Vec<(Window, Rectangle<i32, Logical>)>,
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
        Self {
            space: SpaceState { state: space },
            comp: CompState::default(),
            output,
            display_handle,
            to_place: Vec::new(),
        }
    }

    /// The frame step's initial map (frames `hook/window/interface.rs`): each
    /// window the drain placed goes into the Space at its placement and the
    /// registry hears `Placed`. Returns how many windows were placed. A window
    /// that died before its frame is dropped, as the real hook never sees it.
    pub fn tick_frame(&mut self) -> usize {
        let mut placed = 0;
        for (window, geometry) in std::mem::take(&mut self.to_place) {
            if !window.alive() {
                continue;
            }
            self.space
                .state
                .map_element(window.clone(), geometry.loc, false);
            if let Some(handle) = SurfaceHandle::of_window(&window) {
                self.surface_event(SurfaceEvent::Placed(handle));
            }
            placed += 1;
        }
        placed
    }

    /// Windows waiting for the frame step.
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
    fn owning_space(&self, _surface: &WlSurface) -> &SpaceState {
        &self.space
    }
    fn owning_space_mut(&mut self, _surface: &WlSurface) -> &mut SpaceState {
        &mut self.space
    }
    fn all_world_spaces(&self) -> Vec<&SpaceState> {
        vec![&self.space]
    }
    fn active_output(&self) -> Option<Output> {
        Some(self.output.clone())
    }
    fn initialize_surface_data(&mut self, window: Window) {
        // The Orchestrator's binding, minus the WindowData stamps nothing
        // headless reads.
        if let Some(handle) = SurfaceHandle::of_window(&window) {
            let pid = ident::pid(&window, &self.display_handle);
            self.comp.bind_uuid(&handle, uuid::Uuid::now_v7(), pid);
        }
    }
    fn session_restore_size(&self, _surface: &WlSurface) -> Option<Size<i32, Logical>> {
        None
    }
    fn destroy_surface_data(&mut self, surface: ToplevelSurface, _drag_discard: bool) {
        // The real host leaves this to the Space's `refresh`; there is no frame
        // loop here to run it, so the window leaves the Space now.
        self.to_place
            .retain(|(window, _)| window.toplevel() != Some(&surface));
        let gone = self
            .space
            .state
            .elements()
            .find(|window| window.toplevel() == Some(&surface))
            .cloned();
        if let Some(window) = gone {
            self.space.state.unmap_elem(&window);
        }
    }
    fn owning_x11_window(&self, _surface: &X11Surface) -> Option<(uuid::Uuid, Window)> {
        None
    }
    fn space_of_world_mut(&mut self, _world: uuid::Uuid) -> &mut SpaceState {
        &mut self.space
    }
    fn destroy_x11_data(&mut self, _window: Window) {}
    fn withdraw_x11(&mut self, _world: uuid::Uuid, window: Window) {
        self.space.state.unmap_elem(&window);
    }
    fn readmit_x11(&mut self, _window: Window) {}
    fn is_space_element(&self, window: &Window) -> bool {
        self.space.state.elements().any(|element| element == window)
    }
    fn forget_withdrawn_x11(&mut self, _surface: &X11Surface) {}
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
        self.to_place.push((window, geometry));
    }
    fn fullscreen_request(&mut self, window: Window, fullscreen: bool) {
        let target = world::comp::fullscreen::target_geometry(&self.comp, &self.space.state, &window);
        world::window::interface::draw::fullscreen::apply(
            &mut self.space.state, &window, fullscreen, target,
        );
    }
    fn request_activation(&mut self, _window: Window, _origin: ActivationOrigin) {}
    fn settle_toplevel_drag(&mut self, _surface: WlSurface) {}
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

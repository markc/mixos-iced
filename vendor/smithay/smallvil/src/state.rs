use std::{ffi::OsString, sync::Arc};

use smithay::{
    desktop::{PopupManager, Space, Window, WindowSurfaceType},
    input::{Seat, SeatState},
    reexports::{
        calloop::{EventLoop, Interest, LoopSignal, Mode, PostAction, generic::Generic},
        wayland_server::{
            Display, DisplayHandle,
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::wl_surface::WlSurface,
        },
    },
    utils::{Logical, Point},
    wayland::{
        compositor::{CompositorClientState, CompositorState},
        output::OutputManagerState,
        selection::data_device::DataDeviceState,
        shell::xdg::XdgShellState,
        shm::ShmState,
        socket::ListeningSocketSource,
    },
};
use smithay::wayland::dmabuf::{DmabufState, DmabufGlobal, DmabufHandler, ImportNotifier};
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::wayland::shell::xdg::ToplevelSurface;

use smithay::wayland::shell::xdg::decoration::{
    XdgDecorationState, XdgDecorationHandler
};

pub struct Smallvil {
    pub start_time: std::time::Instant,
    pub socket_name: OsString,
    pub display_handle: DisplayHandle,

    pub space: Space<Window>,
    pub loop_signal: LoopSignal,

    // Smithay State
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub shm_state: ShmState,
    pub output_manager_state: OutputManagerState,
    pub seat_state: SeatState<Smallvil>,
    pub data_device_state: DataDeviceState,
    pub popups: PopupManager,

    pub seat: Seat<Self>,
    pub dmabuf_state: DmabufState,
    pub dmabuf_global: Option<DmabufGlobal>,

    pub camera_pos: smithay::utils::Point<f64, smithay::utils::Logical>,
    pub zoom: f64,

    pub is_panning: bool,
    pub last_screen_pos: smithay::utils::Point<f64, smithay::utils::Physical>,

    pub xdg_decoration_state: XdgDecorationState,
}

impl Smallvil {
    pub fn new(event_loop: &mut EventLoop<Self>, display: Display<Self>) -> Self {
        let start_time = std::time::Instant::now();

        let dh = display.handle();

        // Here we initialize implementations of some wayland protocols
        // Some of them require us to implement traits on the Smallvil state,
        // you can find those implementations in the `crate::handlers` module

        // Initialize protocols needed for displaying windows
        let compositor_state = CompositorState::new::<Self>(&dh);
        let xdg_shell_state = XdgShellState::new::<Self>(&dh);
        let shm_state = ShmState::new::<Self>(&dh, vec![]);
        let popups = PopupManager::default();

        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&dh);

        // Data device is responsible for clipboard and drag-and-drop
        let data_device_state = DataDeviceState::new::<Self>(&dh);

        // A seat is a group of keyboards, pointer and touch devices.
        // A seat typically has a pointer and maintains a keyboard focus and a pointer focus.
        let mut seat_state = SeatState::new();
        let mut seat: Seat<Self> = seat_state.new_wl_seat(&dh, "winit");

        // Notify clients that we have a keyboard, for the sake of the example we assume that keyboard is always present.
        // You may want to track keyboard hot-plug in real compositor.
        seat.add_keyboard(Default::default(), 200, 25).unwrap();

        // Notify clients that we have a pointer (mouse)
        // Here we assume that there is always pointer plugged in
        seat.add_pointer();

        // A space represents a two-dimensional plane. Windows and Outputs can be mapped onto it.
        //
        // Windows get a position and stacking order through mapping.
        // Outputs become views of a part of the Space and can be rendered via Space::render_output.
        let space = Space::default();

        // Setup a wayland socket that will be used to accept clients
        let socket_name = Self::init_wayland_listener(display, event_loop);

        // Get the loop signal, used to stop the event loop
        let loop_signal = event_loop.get_signal();
        let dmabuf_state = DmabufState::new();
        let xdg_decoration_state = XdgDecorationState::new::<Smallvil>(&dh);


        Self {
            start_time,
            display_handle: dh,

            space,
            loop_signal,
            socket_name,

            compositor_state,
            xdg_shell_state,
            shm_state,
            output_manager_state,
            seat_state,
            data_device_state,
            popups,
            seat,
            dmabuf_state,
            is_panning: false,
            last_screen_pos: (0.0, 0.0).into(),
            camera_pos: (0.0, 0.0).into(),
            zoom: 1.0,
            dmabuf_global: None, // Empty until the GPU starts
            xdg_decoration_state,

        }
    }

    fn init_wayland_listener(display: Display<Smallvil>, event_loop: &mut EventLoop<Self>) -> OsString {
        // Creates a new listening socket, automatically choosing the next available `wayland` socket name.
        let listening_socket = ListeningSocketSource::new_auto().unwrap();

        // Get the name of the listening socket.
        // Clients will connect to this socket.
        let socket_name = listening_socket.socket_name().to_os_string();

        let loop_handle = event_loop.handle();

        loop_handle
            .insert_source(listening_socket, move |client_stream, _, state| {
                // Inside the callback, you should insert the client into the display.
                //
                // You may also associate some data with the client when inserting the client.
                state
                    .display_handle
                    .insert_client(client_stream, Arc::new(ClientState::default()))
                    .unwrap();
            })
            .expect("Failed to init the wayland event source.");

        // You also need to add the display itself to the event loop, so that client events will be processed by wayland-server.
        loop_handle
            .insert_source(
                Generic::new(display, Interest::READ, Mode::Level),
                |_, display, state| {
                    // Safety: we don't drop the display
                    unsafe {
                        display.get_mut().dispatch_clients(state).unwrap();
                    }
                    Ok(PostAction::Continue)
                },
            )
            .unwrap();

        socket_name
    }

    pub fn surface_under(&self, pos: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        self.space.element_under(pos).and_then(|(window, location)| {
            window
                .surface_under(pos - location.to_f64(), WindowSurfaceType::ALL)
                .map(|(s, p)| (s, (p + location).to_f64()))
        })
    }
}

// The Handler (DmabufHandler)
// What it is: The logic that manages the "handshake."
//
// The logic: When Alacritty sends a frame, it says "Here is a memory address." The dmabuf_imported function is the receipt. By calling notifier.successful(), you tell the GPU: "I’ve got the memory, you can keep working."
// You must tell Smithay how to handle incoming GPU memory
impl DmabufHandler for Smallvil {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_imported(&mut self, _global: &DmabufGlobal, _dmabuf: Dmabuf, notifier: ImportNotifier) {
        // This tells the 4090 that the memory transfer was successful
        let _ = notifier.successful::<Smallvil>();
    }

//     /// The global advertisement for hardware-accelerated buffers.
    //     /// Without this, clients like Alacritty will fall back to software rendering
    //     /// or fail to launch.
    //     pub dmabuf_global: Option<DmabufGlobal>,
    //
    //     /// Tracks the state of all GPU-resident memory buffers currently
    //     /// shared between the compositor and apps.
    //     pub dmabuf_state: DmabufState,
}
/// Data associated with a wayland client that connects to Smallvil.
/// One instance of this type per client.
#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}



impl XdgDecorationHandler for Smallvil {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        // Tell the client we are handling the decorations (so it strips its own)
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(XdgDecorationMode::ServerSide);
        });
        toplevel.send_pending_configure();
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, _mode: XdgDecorationMode) {
        // Ignore the client's preference, strictly force ServerSide
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(XdgDecorationMode::ServerSide);
        });
        toplevel.send_pending_configure();
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(XdgDecorationMode::ServerSide);
        });
        toplevel.send_pending_configure();
    }
}
// use smithay::delegate_xdg_decoration;
use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as XdgDecorationMode;

// use smithay::reexports::wayland_server::{delegate_dispatch, delegate_global_dispatch};
// use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::{
//     zxdg_decoration_manager_v1::ZxdgDecorationManagerV1,
//     zxdg_toplevel_decoration_v1::ZxdgToplevelDecorationV1,
// };
// use smithay::wayland::shell::xdg::decoration::{
//     XdgDecorationState, XdgDecorationUserData, XdgDecorationHandler
// };
// use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as XdgDecorationMode;
// use smithay::wayland::shell::xdg::ToplevelSurface;
// delegate_global_dispatch!(Smallvil: [ZxdgDecorationManagerV1: ()] => XdgDecorationState);
// delegate_dispatch!(Smallvil: [ZxdgDecorationManagerV1: ()] => XdgDecorationState);
// delegate_dispatch!(Smallvil: [ZxdgToplevelDecorationV1: XdgDecorationUserData] => XdgDecorationState);
// smithay::delegate_xdg_decoration!(Smallvil);
// /Don't forget the delegate macro! This wires up the Wayland protocol events.
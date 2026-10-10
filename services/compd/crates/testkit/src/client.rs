//! [`TestClient`]: an in-process `wayland-client` connection.
//!
//! Never blocks: [`TestClient::read`] reads only what is already on the socket,
//! and the harness interleaves it with the server's dispatch. Configures are
//! acked as they arrive (xdg_surface and layer surface), so a test writes the
//! protocol sequence a well-behaved client sends: role, commit, roundtrip,
//! attach, commit, roundtrip.

use std::fs::File;
use std::os::fd::{AsFd, FromRawFd};
use std::os::unix::net::UnixStream;

use wayland_client::backend::WaylandError;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor, wl_pointer,
    wl_pointer::WlPointer, wl_registry::WlRegistry, wl_seat::WlSeat, wl_shm, wl_shm::WlShm,
    wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, delegate_noop};
use wayland_protocols::xdg::shell::client::{
    xdg_popup::XdgPopup, xdg_positioner::XdgPositioner, xdg_surface, xdg_surface::XdgSurface,
    xdg_toplevel::XdgToplevel, xdg_wm_base, xdg_wm_base::XdgWmBase,
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1, zwlr_layer_shell_v1::ZwlrLayerShellV1, zwlr_layer_surface_v1,
    zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
};
use wayland_protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1::{self, ZwlrForeignToplevelHandleV1},
    zwlr_foreign_toplevel_manager_v1::{self, ZwlrForeignToplevelManagerV1},
};
use wayland_protocols_wlr::screencopy::v1::client::{
    zwlr_screencopy_frame_v1, zwlr_screencopy_frame_v1::ZwlrScreencopyFrameV1,
    zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1,
};

/// What the client's event handlers record.
#[derive(Default)]
pub struct ClientState {
    /// `(name, interface, version)` of every advertised global.
    pub globals: Vec<(u32, String, u32)>,
    /// `wl_display.sync` callbacks that have come back.
    pub syncs: u64,
    /// Configures acked (xdg_surface and layer surface together).
    pub configures: u64,
    /// Default false preserves automatic ACK for existing fixtures. Tests may
    /// hold an xdg configure while independently controlling buffer commits.
    pub hold_xdg_configures: bool,
    /// `(xdg_surface protocol id, serial)` in receive order, including held ACKs.
    pub xdg_configures: Vec<(u32, u32)>,
    /// Actual toplevel configure sizes/states, associated with their object.
    pub toplevel_configures: Vec<ToplevelConfigure>,
    /// The serial of the last `wl_pointer.enter`, on any bound seat.
    pub enter_serial: Option<u32>,
    /// Every `wl_pointer.button` received: `(serial, button, pressed, seat)`, with
    /// `seat` an index into the client's bound seats ([`TestClient::seat`]). The
    /// serial and that seat are what `xdg_toplevel.move` / `.resize` must name.
    pub buttons: Vec<(u32, u32, bool, usize)>,
    /// Every screencopy frame's events, by the index [`TestClient::capture`] returned.
    pub frames: Vec<FrameEvents>,
    /// Dock-facing handles and their advertised app IDs.
    pub foreign_toplevels: Vec<(ZwlrForeignToplevelHandleV1, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToplevelConfigure {
    pub toplevel: u32,
    pub size: (i32, i32),
    pub states: Vec<u32>,
}

pub struct TestClient {
    conn: Connection,
    queue: EventQueue<ClientState>,
    qh: QueueHandle<ClientState>,
    pub state: ClientState,
    registry: WlRegistry,
    syncs_sent: u64,
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    wm_base: Option<XdgWmBase>,
    layer_shell: Option<ZwlrLayerShellV1>,
    /// Every advertised `wl_seat` (the primary and the agent seat), each with
    /// its `wl_pointer`.
    seats: Vec<(WlSeat, WlPointer)>,
    /// The first advertised `wl_output`, and `zwlr_screencopy_manager_v1`.
    output: Option<WlOutput>,
    screencopy: Option<ZwlrScreencopyManagerV1>,
    /// Pool backing files, held for the client's lifetime.
    files: Vec<File>,
}

impl TestClient {
    pub fn new(stream: UnixStream) -> Self {
        stream
            .set_nonblocking(true)
            .expect("client socket nonblocking");
        let conn = Connection::from_socket(stream).expect("wayland-client over the socketpair");
        let queue = conn.new_event_queue::<ClientState>();
        let qh = queue.handle();
        let registry = conn.display().get_registry(&qh, ());
        Self {
            conn,
            queue,
            qh,
            state: ClientState::default(),
            registry,
            syncs_sent: 0,
            compositor: None,
            shm: None,
            wm_base: None,
            layer_shell: None,
            seats: Vec::new(),
            output: None,
            screencopy: None,
            files: Vec::new(),
        }
    }

    /// Send everything queued. A full socket is not an error here: the next
    /// pump flushes the rest.
    pub fn flush(&mut self) {
        match self.conn.flush() {
            Ok(()) => {}
            Err(WaylandError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => panic!("client flush failed: {e}"),
        }
    }

    /// Read whatever the server has sent so far and run the handlers.
    pub fn read(&mut self) {
        if let Err(e) = self.try_read() {
            panic!("client read failed (protocol error from compd?): {e}");
        }
    }

    /// [`Self::read`] that hands back the connection's failure instead of
    /// panicking — for tests that expect compd to post a protocol error.
    pub fn try_read(&mut self) -> Result<(), WaylandError> {
        self.queue
            .dispatch_pending(&mut self.state)
            .map_err(dispatch_error)?;
        if let Some(guard) = self.queue.prepare_read() {
            match guard.read() {
                Ok(_) => {}
                Err(WaylandError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e),
            }
        }
        self.queue
            .dispatch_pending(&mut self.state)
            .map_err(dispatch_error)?;
        Ok(())
    }

    /// The protocol error compd posted on this connection, if any.
    pub fn protocol_error(&self) -> Option<wayland_client::backend::protocol::ProtocolError> {
        self.conn.protocol_error()
    }

    /// Ask for a `wl_display.sync`; returns the `syncs` count that means it
    /// came back.
    pub fn sync(&mut self) -> u64 {
        self.conn.display().sync(&self.qh, ());
        self.syncs_sent += 1;
        self.syncs_sent
    }

    /// Bind the globals the tests use (after the first roundtrip delivered
    /// them). Panics if compd did not advertise one.
    pub fn bind_globals(&mut self) {
        let find = |interface: &str| {
            self.state
                .globals
                .iter()
                .find(|(_, name, _)| name == interface)
                .map(|(name, _, version)| (*name, *version))
                .unwrap_or_else(|| panic!("compd advertised no {interface}"))
        };
        let (name, version) = find("wl_compositor");
        self.compositor = Some(self.registry.bind(name, version.min(6), &self.qh, ()));
        let (name, _) = find("wl_shm");
        self.shm = Some(self.registry.bind(name, 1, &self.qh, ()));
        let (name, version) = find("xdg_wm_base");
        self.wm_base = Some(self.registry.bind(name, version.min(6), &self.qh, ()));
        let (name, version) = find("zwlr_layer_shell_v1");
        self.layer_shell = Some(self.registry.bind(name, version.min(4), &self.qh, ()));
        // Every seat, each with a pointer: the harness drives input through the
        // server's primary seat, and the client cannot tell which global that is
        // before binding it.
        let seats: Vec<(u32, u32)> = self
            .state
            .globals
            .iter()
            .filter(|(_, interface, _)| interface == "wl_seat")
            .map(|(name, _, version)| (*name, *version))
            .collect();
        for (name, version) in seats {
            let seat: WlSeat = self.registry.bind(name, version.min(7), &self.qh, ());
            let pointer = seat.get_pointer(&self.qh, self.seats.len());
            self.seats.push((seat, pointer));
        }
        assert!(!self.seats.is_empty(), "compd advertised no wl_seat");
        let (name, version) = find_in(&self.state.globals, "wl_output");
        self.output = Some(self.registry.bind(name, version.min(4), &self.qh, ()));
        let (name, version) = find_in(&self.state.globals, "zwlr_screencopy_manager_v1");
        self.screencopy = Some(self.registry.bind(name, version.min(3), &self.qh, ()));
    }

    /// The bound seat with this index (see [`ClientState::buttons`]).
    pub fn seat(&self, index: usize) -> &WlSeat {
        &self.seats[index].0
    }

    /// Bind the dock protocol on demand; ordinary fixtures need no mirror.
    pub fn bind_foreign_toplevels(&mut self) {
        let (name, version) = find_in(&self.state.globals, "zwlr_foreign_toplevel_manager_v1");
        let _: ZwlrForeignToplevelManagerV1 =
            self.registry.bind(name, version.min(3), &self.qh, ());
    }

    pub fn activate_foreign(&self, app_id: &str) {
        let (handle, _) = self
            .state
            .foreign_toplevels
            .iter()
            .find(|(_, id)| id == app_id)
            .expect("the dock knows the target toplevel");
        handle.activate(self.seat(0));
    }

    pub fn create_surface(&mut self) -> WlSurface {
        self.compositor
            .as_ref()
            .expect("globals bound")
            .create_surface(&self.qh, ())
    }

    /// `surface` becomes an xdg toplevel.
    pub fn toplevel(&mut self, surface: &WlSurface) -> (XdgSurface, XdgToplevel) {
        let xdg = self
            .wm_base
            .as_ref()
            .expect("globals bound")
            .get_xdg_surface(surface, &self.qh, ());
        let toplevel = xdg.get_toplevel(&self.qh, ());
        (xdg, toplevel)
    }

    /// `surface` becomes an xdg popup of `parent`, 20x20 off its top-left.
    pub fn popup(&mut self, surface: &WlSurface, parent: &XdgSurface) -> (XdgSurface, XdgPopup) {
        let wm_base = self.wm_base.as_ref().expect("globals bound");
        let positioner: XdgPositioner = wm_base.create_positioner(&self.qh, ());
        positioner.set_size(20, 20);
        positioner.set_anchor_rect(0, 0, 1, 1);
        let xdg = wm_base.get_xdg_surface(surface, &self.qh, ());
        let popup = xdg.get_popup(Some(parent), &positioner, &self.qh, ());
        positioner.destroy();
        (xdg, popup)
    }

    /// `surface` becomes a 100x30 layer surface on the top layer, output left
    /// to the compositor.
    pub fn layer(&mut self, surface: &WlSurface, namespace: &str) -> ZwlrLayerSurfaceV1 {
        let layer = self
            .layer_shell
            .as_ref()
            .expect("globals bound")
            .get_layer_surface(
                surface,
                None,
                zwlr_layer_shell_v1::Layer::Top,
                namespace.to_string(),
                &self.qh,
                (),
            );
        layer.set_size(100, 30);
        layer
    }

    /// Commit `surface` with no change.
    pub fn commit(&mut self, surface: &WlSurface) {
        surface.commit();
    }

    /// Attach a fresh `width` x `height` ARGB shm buffer, damage it, commit.
    pub fn attach(&mut self, surface: &WlSurface, width: i32, height: i32) {
        let buffer = self.shm_buffer(width, height);
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, width, height);
        surface.commit();
    }

    /// Attach the null buffer and commit: the client unmaps the surface.
    pub fn attach_null(&mut self, surface: &WlSurface) {
        surface.attach(None, 0, 0);
        surface.commit();
    }

    fn shm_buffer(&mut self, width: i32, height: i32) -> WlBuffer {
        self.shm_buffer_with(width, height, width * 4, wl_shm::Format::Argb8888)
            .0
    }

    /// An shm buffer of any layout, and the index of its backing file for
    /// [`Self::read_buffer`].
    pub fn shm_buffer_with(
        &mut self,
        width: i32,
        height: i32,
        stride: i32,
        format: wl_shm::Format,
    ) -> (WlBuffer, usize) {
        let size = stride * height;
        let fd = unsafe { libc::memfd_create(c"testkit".as_ptr(), libc::MFD_CLOEXEC) };
        assert!(fd >= 0, "memfd_create: {}", std::io::Error::last_os_error());
        let file = unsafe { File::from_raw_fd(fd) };
        file.set_len(size as u64).expect("size the shm pool");
        let pool: WlShmPool =
            self.shm
                .as_ref()
                .expect("globals bound")
                .create_pool(file.as_fd(), size, &self.qh, ());
        let buffer = pool.create_buffer(0, width, height, stride, format, &self.qh, ());
        pool.destroy();
        self.files.push(file);
        (buffer, self.files.len() - 1)
    }

    /// The bytes of a buffer made by [`Self::shm_buffer_with`].
    pub fn read_buffer(&self, index: usize) -> Vec<u8> {
        use std::os::unix::fs::FileExt;
        let file = &self.files[index];
        let len = file.metadata().expect("shm file metadata").len() as usize;
        let mut bytes = vec![0; len];
        file.read_exact_at(&mut bytes, 0)
            .expect("read the shm file");
        bytes
    }

    /// `zwlr_screencopy_manager_v1.capture_output` (or `_region` with `region`
    /// as `(x, y, width, height)` in output-local logical coordinates) of the
    /// first `wl_output`. Its events land in [`ClientState::frames`] under the
    /// returned index.
    pub fn capture(
        &mut self,
        region: Option<(i32, i32, i32, i32)>,
    ) -> (ZwlrScreencopyFrameV1, usize) {
        self.capture_with(region, false)
    }

    /// [`capture`](Self::capture) with `overlay_cursor` chosen (`capture` asks
    /// for no cursor, as grim does by default).
    pub fn capture_with(
        &mut self,
        region: Option<(i32, i32, i32, i32)>,
        overlay_cursor: bool,
    ) -> (ZwlrScreencopyFrameV1, usize) {
        let cursor = i32::from(overlay_cursor);
        let manager = self.screencopy.as_ref().expect("globals bound");
        let output = self.output.as_ref().expect("globals bound");
        let index = self.state.frames.len();
        self.state.frames.push(FrameEvents::default());
        let frame = match region {
            None => manager.capture_output(cursor, output, &self.qh, index),
            Some((x, y, w, h)) => {
                manager.capture_output_region(cursor, output, x, y, w, h, &self.qh, index)
            }
        };
        (frame, index)
    }
}

fn find_in(globals: &[(u32, String, u32)], interface: &str) -> (u32, u32) {
    globals
        .iter()
        .find(|(_, name, _)| name == interface)
        .map(|(name, _, version)| (*name, *version))
        .unwrap_or_else(|| panic!("compd advertised no {interface}"))
}

fn dispatch_error(e: wayland_client::DispatchError) -> WaylandError {
    match e {
        wayland_client::DispatchError::Backend(e) => e,
        other => panic!("client dispatch failed: {other}"),
    }
}

/// What one screencopy frame has been told.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FrameEvents {
    /// `buffer`: (wl_shm format code, width, height, stride).
    pub buffer: Option<(u32, u32, u32, u32)>,
    pub buffer_done: bool,
    pub flags: Option<u32>,
    pub damage: Vec<(u32, u32, u32, u32)>,
    pub ready: bool,
    pub failed: bool,
}

impl Dispatch<ZwlrScreencopyFrameV1, usize> for ClientState {
    fn event(
        state: &mut Self,
        _frame: &ZwlrScreencopyFrameV1,
        event: zwlr_screencopy_frame_v1::Event,
        index: &usize,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let record = &mut state.frames[*index];
        match event {
            zwlr_screencopy_frame_v1::Event::Buffer {
                format,
                width,
                height,
                stride,
            } => {
                let format = match format {
                    wayland_client::WEnum::Value(f) => f as u32,
                    wayland_client::WEnum::Unknown(f) => f,
                };
                record.buffer = Some((format, width, height, stride));
            }
            zwlr_screencopy_frame_v1::Event::BufferDone => record.buffer_done = true,
            zwlr_screencopy_frame_v1::Event::Flags { flags } => {
                record.flags = Some(match flags {
                    wayland_client::WEnum::Value(f) => f.bits(),
                    wayland_client::WEnum::Unknown(f) => f,
                });
            }
            zwlr_screencopy_frame_v1::Event::Damage {
                x,
                y,
                width,
                height,
            } => {
                record.damage.push((x, y, width, height));
            }
            zwlr_screencopy_frame_v1::Event::Ready { .. } => record.ready = true,
            zwlr_screencopy_frame_v1::Event::Failed => record.failed = true,
            _ => {}
        }
    }
}

impl Dispatch<WlRegistry, ()> for ClientState {
    fn event(
        state: &mut Self,
        _registry: &WlRegistry,
        event: wayland_client::protocol::wl_registry::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wayland_client::protocol::wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            state.globals.push((name, interface, version));
        }
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for ClientState {
    wayland_client::event_created_child!(ClientState, ZwlrForeignToplevelManagerV1, [
        0 => (ZwlrForeignToplevelHandleV1, ())
    ]);

    fn event(
        state: &mut Self,
        _manager: &ZwlrForeignToplevelManagerV1,
        event: zwlr_foreign_toplevel_manager_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let zwlr_foreign_toplevel_manager_v1::Event::Toplevel { toplevel } = event {
            state.foreign_toplevels.push((toplevel, String::new()));
        }
    }
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for ClientState {
    fn event(
        state: &mut Self,
        handle: &ZwlrForeignToplevelHandleV1,
        event: zwlr_foreign_toplevel_handle_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_foreign_toplevel_handle_v1::Event::AppId { app_id } => {
                state
                    .foreign_toplevels
                    .iter_mut()
                    .find(|(candidate, _)| candidate == handle)
                    .expect("the manager announced this handle")
                    .1 = app_id;
            }
            zwlr_foreign_toplevel_handle_v1::Event::Closed => {
                state
                    .foreign_toplevels
                    .retain(|(candidate, _)| candidate != handle);
                handle.destroy();
            }
            _ => {}
        }
    }
}

impl Dispatch<WlCallback, ()> for ClientState {
    fn event(
        state: &mut Self,
        _callback: &WlCallback,
        event: wayland_client::protocol::wl_callback::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wayland_client::protocol::wl_callback::Event::Done { .. } = event {
            state.syncs += 1;
        }
    }
}

impl Dispatch<XdgWmBase, ()> for ClientState {
    fn event(
        _state: &mut Self,
        wm_base: &XdgWmBase,
        event: xdg_wm_base::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for ClientState {
    fn event(
        state: &mut Self,
        xdg: &XdgSurface,
        event: xdg_surface::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            state.xdg_configures.push((protocol_id(xdg), serial));
            if !state.hold_xdg_configures {
                xdg.ack_configure(serial);
            }
            state.configures += 1;
        }
    }
}

impl Dispatch<XdgToplevel, ()> for ClientState {
    fn event(
        state: &mut Self,
        toplevel: &XdgToplevel,
        event: wayland_protocols::xdg::shell::client::xdg_toplevel::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wayland_protocols::xdg::shell::client::xdg_toplevel::Event::Configure {
            width,
            height,
            states,
        } = event
        {
            state.toplevel_configures.push(ToplevelConfigure {
                toplevel: protocol_id(toplevel),
                size: (width, height),
                states: states
                    .chunks_exact(4)
                    .map(|bytes| u32::from_ne_bytes(bytes.try_into().expect("four-byte state")))
                    .collect(),
            });
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for ClientState {
    fn event(
        state: &mut Self,
        layer: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let zwlr_layer_surface_v1::Event::Configure { serial, .. } = event {
            layer.ack_configure(serial);
            state.configures += 1;
        }
    }
}

impl Dispatch<WlPointer, usize> for ClientState {
    fn event(
        state: &mut Self,
        _pointer: &WlPointer,
        event: wl_pointer::Event,
        seat: &usize,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter { serial, .. } => state.enter_serial = Some(serial),
            wl_pointer::Event::Button {
                serial,
                button,
                state: pressed,
                ..
            } => {
                let pressed = matches!(
                    pressed,
                    wayland_client::WEnum::Value(wl_pointer::ButtonState::Pressed)
                );
                state.buttons.push((serial, button, pressed, *seat));
            }
            _ => {}
        }
    }
}

// No events at all.
delegate_noop!(ClientState: WlCompositor);
delegate_noop!(ClientState: WlShmPool);
delegate_noop!(ClientState: XdgPositioner);
delegate_noop!(ClientState: ZwlrLayerShellV1);
// Events the tests do not read.
delegate_noop!(ClientState: ignore WlSurface);
delegate_noop!(ClientState: ignore WlSeat);
delegate_noop!(ClientState: ignore WlOutput);
delegate_noop!(ClientState: ZwlrScreencopyManagerV1);
delegate_noop!(ClientState: ignore WlShm);
delegate_noop!(ClientState: ignore WlBuffer);
delegate_noop!(ClientState: ignore XdgPopup);

/// The client half of a proxy's id, for the server-side lookup.
pub fn protocol_id(proxy: &impl Proxy) -> u32 {
    proxy.id().protocol_id()
}

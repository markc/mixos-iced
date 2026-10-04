//! [`Harness`]: one compd engine, one client, pumped in one thread.

use std::os::unix::net::UnixStream;
use std::sync::{Arc, Once};
use std::time::Duration;

use dispatcher::state::state::Dispatch;
use dispatcher::wire::trait_::surface_event::SurfaceHandle;
use dispatcher::wire::wire::Wire;
use surfaces::SurfaceRecord;
use protocols::wayland::connection::record::record::WaylandClientSession;
use world::comp::CompState;
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface as ServerSurface;
use smithay::reexports::wayland_server::{Client, Display};
use smithay::wayland::compositor::CompositorClientState;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_protocols::xdg::shell::client::{xdg_surface::XdgSurface, xdg_toplevel::XdgToplevel};

use crate::client::{TestClient, protocol_id};
use crate::host::TestHost;

/// Pumps a roundtrip may take before the harness calls it hung.
const ROUNDTRIP_PUMPS: usize = 64;

pub struct Harness {
    event_loop: EventLoop<'static, Wire<TestHost>>,
    display: Display<Dispatch>,
    /// compd's engine: the real `Dispatch` in `wire.state`, the test host in
    /// `wire.inner`.
    pub wire: Wire<TestHost>,
    /// The client as the server sees it.
    server_client: Client,
    pub client: TestClient,
}

impl Harness {
    /// A fresh engine with one connected client whose globals are bound.
    pub fn new() -> Self {
        isolate_config();
        let event_loop: EventLoop<'static, Wire<TestHost>> =
            EventLoop::try_new().expect("calloop event loop");
        let display: Display<Dispatch> = Display::new().expect("wayland display");
        let mut display_handle = display.handle();
        let host = TestHost::new(display_handle.clone());
        // The fake output as a `wl_output` clients can name (screencopy captures
        // one), and wlr-screencopy itself — advertised here as a backend would.
        host.output.create_global::<Dispatch>(&display_handle);
        screencopy::create_global(&display_handle);
        let wire = Wire::new(host, &display_handle, None, event_loop.handle());
        let (server_end, client_end) = UnixStream::pair().expect("socketpair");
        let server_client = display_handle
            .insert_client(
                server_end,
                Arc::new(WaylandClientSession {
                    compositor_state: CompositorClientState::default(),
                    proprietary: false,
                }),
            )
            .expect("insert the test client");
        let client = TestClient::new(client_end);
        let mut harness = Self {
            event_loop,
            display,
            wire,
            server_client,
            client,
        };
        harness.roundtrip();
        harness.client.bind_globals();
        harness.roundtrip();
        harness
    }

    /// One loop iteration as compd's `main` runs it: client writes land, the
    /// display dispatches them, the protocol outboxes drain, replies go out,
    /// the client reads them.
    pub fn pump(&mut self) {
        self.client.flush();
        self.event_loop
            .dispatch(Some(Duration::ZERO), &mut self.wire)
            .expect("calloop dispatch");
        self.display
            .dispatch_clients(&mut self.wire.state)
            .expect("dispatch the test client");
        self.wire.drain_protocol();
        self.display
            .flush_clients()
            .expect("flush to the test client");
        self.client.read();
    }

    /// Pump until everything the client sent so far has been handled and
    /// answered (a `wl_display.sync` round trip). Panics if it never is.
    /// Pump until compd posts a protocol error on the client, and return it.
    /// Panics if the round trip completes without one.
    pub fn roundtrip_expecting_error(&mut self) -> wayland_client::backend::protocol::ProtocolError {
        let target = self.client.sync();
        for _ in 0..ROUNDTRIP_PUMPS {
            self.client.flush();
            let _ = self.event_loop.dispatch(Some(Duration::ZERO), &mut self.wire);
            let _ = self.display.dispatch_clients(&mut self.wire.state);
            self.wire.drain_protocol();
            let _ = self.display.flush_clients();
            if self.client.try_read().is_err() {
                return self.client.protocol_error().expect("the connection failed with a protocol error");
            }
            if self.client.state.syncs >= target {
                panic!("the round trip completed without a protocol error");
            }
        }
        panic!("no protocol error within {ROUNDTRIP_PUMPS} pumps");
    }

    /// Service screencopy as a backend does after rendering a frame of the fake
    /// output, reading from a top-down pixman framebuffer whose pixel `(x, y)`
    /// is `pixel(x, y)` (`0x00RRGGBB`). `damage` is the frame's damage in output
    /// physical coordinates. The fake scene has no cursor, so a cursorless copy
    /// reads the same picture.
    pub fn service_screencopy(
        &mut self,
        pixel: impl Fn(i32, i32) -> u32,
        damage: Option<&[smithay::utils::Rectangle<i32, smithay::utils::Physical>]>,
    ) {
        self.service_screencopy_with(&pixel, &pixel, damage);
    }

    /// [`service_screencopy`](Self::service_screencopy) where the frame WITHOUT
    /// its cursor is `cursorless(x, y)`: rendered (a second pixman image) only
    /// when `screencopy::sources_due` asks for it, as a backend does. The
    /// picture shown (`pixel`) is offered every frame, as the nested backend does.
    /// Returns whether it was rendered.
    pub fn service_screencopy_with(
        &mut self,
        pixel: &dyn Fn(i32, i32) -> u32,
        cursorless: &dyn Fn(i32, i32) -> u32,
        damage: Option<&[smithay::utils::Rectangle<i32, smithay::utils::Physical>]>,
    ) -> bool {
        self.service_frame(pixel, cursorless, damage, true, false)
            .cursorless
    }

    /// Service a frame the way the NATIVE backend does: it has no readable
    /// picture of its own (it scans out a KMS swapchain buffer), so it renders
    /// only the pictures `screencopy::sources_due` asks for, the shown one
    /// (`pixel`) included. Returns what was due, i.e. rendered.
    pub fn service_screencopy_native(
        &mut self,
        pixel: &dyn Fn(i32, i32) -> u32,
        cursorless: &dyn Fn(i32, i32) -> u32,
        damage: Option<&[smithay::utils::Rectangle<i32, smithay::utils::Physical>]>,
    ) -> screencopy::SourcesDue {
        self.service_frame(pixel, cursorless, damage, true, true)
    }

    /// Service a frame but DROP the captures instead of finishing them (a
    /// backend that lost the frame): every copy it served must answer `failed`.
    pub fn service_screencopy_unfinished(&mut self, pixel: impl Fn(i32, i32) -> u32) {
        self.service_frame(&pixel, &pixel, None, false, false);
    }

    fn service_frame(
        &mut self,
        pixel: &dyn Fn(i32, i32) -> u32,
        cursorless: &dyn Fn(i32, i32) -> u32,
        damage: Option<&[smithay::utils::Rectangle<i32, smithay::utils::Physical>]>,
        finish: bool,
        native: bool,
    ) -> screencopy::SourcesDue {
        use smithay::backend::renderer::Bind;
        use smithay::backend::renderer::pixman::PixmanRenderer;
        use smithay::reexports::pixman::{FormatCode, Image};
        let output = self.wire.inner.output.clone();
        let (w, h) = crate::host::OUTPUT_SIZE;
        let paint = |pixel: &dyn Fn(i32, i32) -> u32| {
            let mut image =
                Image::new(FormatCode::X8R8G8B8, w as usize, h as usize, true).expect("pixman image");
            let stride = image.stride() / 4;
            // SAFETY: the image owns `stride * h` u32s, alive while `image` is.
            let data = unsafe { std::slice::from_raw_parts_mut(image.data(), stride * h as usize) };
            for y in 0..h {
                for x in 0..w {
                    data[y as usize * stride + x as usize] = pixel(x, y);
                }
            }
            image
        };
        let due = screencopy::sources_due(&output, damage);
        let mut image = (!native || due.cursor).then(|| paint(pixel));
        let mut cursorless_image = due.cursorless.then(|| paint(cursorless));
        let readback = screencopy::Readback {
            size: (w, h).into(),
            origin_bottom_left: false,
        };
        let mut renderer = PixmanRenderer::new().expect("pixman renderer");
        // Both steps, in a backend's order: start the readbacks against the bound
        // frame, hand the frame away (here: drop the bindings), then finish.
        let captures = {
            let framebuffer = image
                .as_mut()
                .map(|image| renderer.bind(image).expect("bind the pixman image"));
            let cursorless_framebuffer = cursorless_image
                .as_mut()
                .map(|image| renderer.bind(image).expect("bind the cursorless image"));
            screencopy::service(
                &mut renderer,
                framebuffer
                    .as_ref()
                    .map(|framebuffer| screencopy::Source { framebuffer, readback }),
                cursorless_framebuffer
                    .as_ref()
                    .map(|framebuffer| screencopy::Source { framebuffer, readback }),
                &output,
                damage,
            )
        };
        if finish {
            captures.finish(&mut renderer);
        } else {
            drop(captures);
        }
        self.roundtrip();
        due
    }

    pub fn roundtrip(&mut self) {
        let target = self.client.sync();
        for _ in 0..ROUNDTRIP_PUMPS {
            self.pump();
            if self.client.state.syncs >= target {
                return;
            }
        }
        panic!("roundtrip did not complete in {ROUNDTRIP_PUMPS} pumps");
    }

    /// The frame step: place what the drain queued (see [`TestHost::tick_frame`]).
    pub fn tick_frame(&mut self) -> usize {
        self.wire.inner.tick_frame()
    }

    /// The comp registry.
    pub fn comp(&self) -> &CompState {
        &self.wire.inner.comp
    }

    /// The registry handle of a live client surface.
    pub fn handle_of(&self, surface: &WlSurface) -> SurfaceHandle {
        let server: ServerSurface = self
            .server_client
            .object_from_protocol_id(&self.display.handle(), protocol_id(surface))
            .expect("the server knows this surface");
        SurfaceHandle::wl(&server)
    }

    /// Press `button` (a linux input code, e.g. `BTN_LEFT` = 0x110) over
    /// `surface` through the server's PRIMARY seat — what an input backend does —
    /// and return the button serial the client received. That serial is what
    /// `xdg_toplevel.move` / `.resize` must name for a real grab.
    ///
    /// The pointer is first moved onto the surface (its origin at the surface's
    /// top-left), so the press has a focus. Panics if the client never sees the
    /// button event.
    pub fn press(&mut self, surface: &WlSurface, button: u32) -> u32 {
        self.pointer_button(surface, button, smithay::backend::input::ButtonState::Pressed)
    }

    /// Release `button` over `surface` (see [`Self::press`]); returns its serial.
    pub fn release(&mut self, surface: &WlSurface, button: u32) -> u32 {
        self.pointer_button(surface, button, smithay::backend::input::ButtonState::Released)
    }

    fn pointer_button(
        &mut self,
        surface: &WlSurface,
        button: u32,
        state: smithay::backend::input::ButtonState,
    ) -> u32 {
        use smithay::input::pointer::{ButtonEvent, MotionEvent};
        use smithay::utils::{Point, SERIAL_COUNTER};
        let server: ServerSurface = self
            .server_client
            .object_from_protocol_id(&self.display.handle(), protocol_id(surface))
            .expect("the server knows this surface");
        let pointer = self
            .wire
            .state
            .seat
            .seat
            .get_pointer()
            .expect("the primary seat has a pointer");
        let dispatch = &mut self.wire.state;
        pointer.motion(
            dispatch,
            Some((server, Point::from((0.0, 0.0)))),
            &MotionEvent { location: Point::from((1.0, 1.0)), serial: SERIAL_COUNTER.next_serial(), time: 0 },
        );
        pointer.frame(dispatch);
        let serial = SERIAL_COUNTER.next_serial();
        pointer.button(dispatch, &ButtonEvent { serial, time: 0, button, state });
        pointer.frame(dispatch);
        self.roundtrip();
        let serial = u32::from(serial);
        assert!(
            self.client.state.buttons.iter().any(|(s, b, _, _)| *s == serial && *b == button),
            "the client did not receive button {button:#x} with serial {serial}"
        );
        serial
    }

    /// The registry record behind `handle`, if it has one.
    pub fn record(&self, handle: &SurfaceHandle) -> Option<&SurfaceRecord<SurfaceHandle>> {
        let registry = &self.comp().registry;
        registry
            .id_for_handle(handle)
            .and_then(|id| registry.get(id))
    }

    /// A `width` x `height` xdg toplevel through the whole map sequence:
    /// role, initial commit, configure, buffer, frame.
    pub fn mapped_toplevel(
        &mut self,
        width: i32,
        height: i32,
    ) -> (WlSurface, XdgSurface, XdgToplevel) {
        let surface = self.client.create_surface();
        let (xdg, toplevel) = self.client.toplevel(&surface);
        self.client.commit(&surface);
        self.roundtrip();
        self.client.attach(&surface, width, height);
        self.roundtrip();
        self.tick_frame();
        (surface, xdg, toplevel)
    }
}

impl Default for Harness {
    fn default() -> Self {
        Self::new()
    }
}

/// Point the config dir (settings.json, preferences.json) at an empty
/// per-process directory, once, before anything reads it, and install the
/// default settings in place of the file compd's `main` would read. `Wire::new`
/// needs them: it asks `config::base::get().hdr` whether to advertise the
/// color-management global (`dispatcher::wire::color::color::create_global`).
fn isolate_config() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        model::environment::config::base::init_with(
            model::environment::config::base::default_settings(),
        );
        let dir = std::env::temp_dir().join(format!("testkit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create the test config dir");
        // SAFETY: runs once, before this process's first engine exists; the
        // only concurrent readers are other tests' harnesses, which all wait on
        // this `Once` before reading the environment.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &dir) };
    });
}

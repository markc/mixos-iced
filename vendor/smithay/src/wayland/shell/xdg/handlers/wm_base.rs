use std::sync::{Arc, Mutex, atomic::AtomicBool};

use indexmap::IndexSet;

use crate::{
    utils::{IsAlive, Serial, alive_tracker::AliveTracker},
    wayland::{
        Dispatch2, GlobalData, GlobalDispatch2,
        shell::xdg::{XDG_POPUP_ROLE, XDG_TOPLEVEL_ROLE},
    },
};

use wayland_server::protocol::wl_surface::WlSurface;

use wayland_protocols::xdg::shell::server::{
    xdg_positioner::XdgPositioner, xdg_surface, xdg_surface::XdgSurface, xdg_wm_base, xdg_wm_base::XdgWmBase,
};

use wayland_server::{DataInit, Dispatch, DisplayHandle, New, Resource, Weak, backend::ClientId};

use super::{ShellClient, ShellClientData, XdgPositionerUserData, XdgShellHandler, XdgSurfaceUserData};

impl<D> GlobalDispatch2<XdgWmBase, D> for GlobalData
where
    D: Dispatch<XdgWmBase, XdgWmBaseUserData>,
    D: Dispatch<XdgSurface, XdgSurfaceUserData>,
    D: Dispatch<XdgPositioner, XdgPositionerUserData>,
    D: XdgShellHandler,
    D: 'static,
{
    fn bind(
        &self,
        state: &mut D,
        _dh: &DisplayHandle,
        _client: &wayland_server::Client,
        resource: New<XdgWmBase>,
        data_init: &mut DataInit<'_, D>,
    ) {
        let shell = data_init.init(resource, XdgWmBaseUserData::default());

        XdgShellHandler::new_client(state, ShellClient::new(&shell));
    }
}

impl<D> Dispatch2<XdgWmBase, D> for XdgWmBaseUserData
where
    D: Dispatch<XdgSurface, XdgSurfaceUserData>,
    D: Dispatch<XdgPositioner, XdgPositionerUserData>,
    D: XdgShellHandler,
    D: 'static,
{
    fn request(
        &self,
        state: &mut D,
        _client: &wayland_server::Client,
        wm_base: &XdgWmBase,
        request: xdg_wm_base::Request,
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            xdg_wm_base::Request::CreatePositioner { id } => {
                data_init.init(id, XdgPositionerUserData::default());
            }
            xdg_wm_base::Request::GetXdgSurface { id, surface } => {
                // compd: role/wrapper rule at get_xdg_surface.
                //
                // Two cases, and they are not the same rule.
                //
                // A role from any *other* protocol (subsurface, layer, cursor,
                // drag icon, session lock, …) is always refused: an
                // `xdg_surface` can never legitimately wrap it.
                //
                // An xdg role (`xdg_toplevel` / `xdg_popup`) is refused only
                // while a LIVE `xdg_surface` for this `wl_surface` still exists.
                // The stamp itself is permanent (`set_role` never clears it), but
                // xdg_shell releases the surface for the same role once its role
                // object and `xdg_surface` are destroyed, and Qt's hide→show does
                // exactly that: it destroys both and later asks
                // `get_xdg_surface` for the same `wl_surface`. `give_role` with
                // the same role stays a no-op, so the fresh wrapper can take the
                // role back.
                //
                // Refusing before `data_init.init` while a wrapper is live is the
                // point: an initialised duplicate wrapper can call
                // `set_window_geometry` / `ack_configure`, which reach the
                // *shared* per-`wl_surface` geometry and configure serials of the
                // live role.
                let refuse = match crate::wayland::compositor::get_role(&surface) {
                    None => false,
                    Some(role) if role == XDG_TOPLEVEL_ROLE || role == XDG_POPUP_ROLE => {
                        XdgSurfaceWrappers::any_live(&surface)
                    }
                    Some(_) => true,
                };
                if refuse {
                    wm_base.post_error(
                        xdg_wm_base::Error::Role,
                        "wl_surface already has an assigned role",
                    );
                    return;
                }
                // compd: refuse a wl_surface with a buffer
                // attached or committed.
                //
                // xdg_surface: "Creating an xdg_surface from a wl_surface which
                // has a buffer attached or committed is a client error." The
                // role-release path above makes this reachable on a surface that
                // already showed content. The code is `invalid_surface_state` on
                // the shell (Mutter's choice): `role` means a role conflict, and
                // `xdg_surface`'s own `unconfigured_buffer` names an object that
                // does not exist yet.
                if XdgShellHandler::surface_has_buffer(state, &surface) {
                    wm_base.post_error(
                        xdg_wm_base::Error::InvalidSurfaceState,
                        "wl_surface has a buffer attached or committed",
                    );
                    return;
                }
                // Do not assign a role to the surface here
                // xdg_surface is not role, only xdg_toplevel and
                // xdg_popup are defined as roles
                let xdg_surface = data_init.init(
                    id,
                    XdgSurfaceUserData {
                        known_surfaces: self.known_surfaces.clone(),
                        wl_surface: surface.clone(),
                        wm_base: wm_base.clone(),
                        has_active_role: AtomicBool::new(false),
                    },
                );
                // compd: per-wl_surface wrapper registry.
                XdgSurfaceWrappers::register(&surface, &xdg_surface);
                self.known_surfaces
                    .lock()
                    .unwrap()
                    .insert(xdg_surface.downgrade());
            }
            xdg_wm_base::Request::Pong { serial } => {
                let serial = Serial::from(serial);
                let valid = {
                    let mut guard = self.client_data.lock().unwrap();
                    if guard.pending_ping == Some(serial) {
                        guard.pending_ping = None;
                        true
                    } else {
                        false
                    }
                };
                if valid {
                    XdgShellHandler::client_pong(state, ShellClient::new(wm_base));
                }
            }
            xdg_wm_base::Request::Destroy => {
                if !self.known_surfaces.lock().unwrap().is_empty() {
                    wm_base.post_error(
                        xdg_wm_base::Error::DefunctSurfaces,
                        "xdg_wm_base was destroyed before children",
                    );
                }
            }
            _ => unreachable!(),
        }
    }

    fn destroyed(&self, state: &mut D, _client_id: ClientId, wm_base: &XdgWmBase) {
        XdgShellHandler::client_destroyed(state, ShellClient::new(wm_base));
        self.alive_tracker.destroy_notify();
    }
}

impl IsAlive for XdgWmBase {
    #[inline]
    fn alive(&self) -> bool {
        let data: &XdgWmBaseUserData = self.data().unwrap();
        data.alive_tracker.alive()
    }
}

// compd: per-wl_surface xdg_surface wrapper registry.
/// The `xdg_surface` wrappers created for one `wl_surface`, held weakly in the
/// surface's own data map.
///
/// Per-surface rather than the per-`xdg_wm_base` `known_surfaces`, so a client
/// that binds `xdg_wm_base` twice cannot slip a second live wrapper past the
/// `get_xdg_surface` guard through the other binding.
#[derive(Debug, Default)]
pub(crate) struct XdgSurfaceWrappers(Mutex<Vec<Weak<XdgSurface>>>);

impl XdgSurfaceWrappers {
    fn register(surface: &WlSurface, xdg_surface: &XdgSurface) {
        crate::wayland::compositor::with_states(surface, |states| {
            states.data_map.insert_if_missing_threadsafe(Self::default);
            let wrappers = states.data_map.get::<Self>().unwrap();
            let mut guard = wrappers.0.lock().unwrap();
            guard.retain(|weak| weak.upgrade().is_ok());
            guard.push(xdg_surface.downgrade());
        });
    }

    /// Drop one wrapper from the registry. Called from `xdg_surface.destroy`
    /// so the answer does not depend on when the backend marks the object dead.
    pub(crate) fn unregister(surface: &WlSurface, xdg_surface: &XdgSurface) {
        crate::wayland::compositor::with_states(surface, |states| {
            if let Some(wrappers) = states.data_map.get::<Self>() {
                let gone = xdg_surface.downgrade();
                wrappers
                    .0
                    .lock()
                    .unwrap()
                    .retain(|weak| weak != &gone && weak.upgrade().is_ok());
            }
        });
    }

    fn any_live(surface: &WlSurface) -> bool {
        crate::wayland::compositor::with_states(surface, |states| {
            states.data_map.get::<Self>().is_some_and(|wrappers| {
                let mut guard = wrappers.0.lock().unwrap();
                guard.retain(|weak| weak.upgrade().is_ok());
                !guard.is_empty()
            })
        })
    }
}

/*
 * xdg_shell
 */

/// User data for Xdg Wm Base
#[derive(Default, Debug)]
pub struct XdgWmBaseUserData {
    pub(crate) client_data: Mutex<ShellClientData>,
    known_surfaces: Arc<Mutex<IndexSet<Weak<xdg_surface::XdgSurface>>>>,
    alive_tracker: AliveTracker,
}

// compd: smithay-level guards for the `get_xdg_surface`
// wrapper and buffer rules:
// `destroying_the_xdg_surface_releases_the_wl_surface_for_a_fresh_wrapper`,
// `a_fresh_xdg_surface_on_a_surface_with_a_{committed,pending}_buffer_is_refused`
// and `smithay_default_buffer_check_ignores_an_uncommitted_null_attach`.
// A raw-wire client against a real `Display`, so every case runs
// through the actual request handlers.
#[cfg(test)]
mod tests {
    use std::io::{ErrorKind, Read, Write};
    use std::os::unix::net::UnixStream;
    use std::sync::Arc;

    use wayland_protocols::xdg::shell::server::xdg_wm_base;
    use wayland_server::backend::{ClientData, ClientId, DisconnectReason};
    use wayland_server::protocol::{
        wl_buffer::{self, WlBuffer},
        wl_seat,
        wl_surface::WlSurface,
    };
    use wayland_server::{Client, DataInit, Display, DisplayHandle, Resource};

    use crate::utils::Serial;
    use crate::wayland::Dispatch2;
    use crate::wayland::compositor::{CompositorClientState, CompositorHandler, CompositorState};
    use crate::wayland::shell::xdg::{
        PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
        surface_has_attached_or_committed_buffer,
    };

    struct TestState {
        compositor: CompositorState,
        xdg: XdgShellState,
    }

    struct TestClient(CompositorClientState);
    impl ClientData for TestClient {
        fn initialized(&self, _: ClientId) {}
        fn disconnected(&self, _: ClientId, _: DisconnectReason) {}
    }

    impl CompositorHandler for TestState {
        fn compositor_state(&mut self) -> &mut CompositorState {
            &mut self.compositor
        }
        fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
            &client.get_data::<TestClient>().unwrap().0
        }
        // Deliberately does not consume `SurfaceAttributes::current`, so the
        // default `surface_has_buffer` answers from Smithay's own state.
        fn commit(&mut self, _surface: &WlSurface) {}
    }

    impl XdgShellHandler for TestState {
        fn xdg_shell_state(&mut self) -> &mut XdgShellState {
            &mut self.xdg
        }
        fn new_toplevel(&mut self, _surface: ToplevelSurface) {}
        fn new_popup(&mut self, _surface: PopupSurface, _positioner: PositionerState) {}
        fn grab(&mut self, _surface: PopupSurface, _seat: wl_seat::WlSeat, _serial: Serial) {}
        fn reposition_request(&mut self, _surface: PopupSurface, _positioner: PositionerState, _token: u32) {}
    }

    /// A server-created `wl_buffer` the client can name in `wl_surface.attach`
    /// without needing shm fd passing.
    struct TestBuffer;
    impl Dispatch2<WlBuffer, TestState> for TestBuffer {
        fn request(
            &self,
            _state: &mut TestState,
            _client: &Client,
            _resource: &WlBuffer,
            _request: wl_buffer::Request,
            _dhandle: &DisplayHandle,
            _data_init: &mut DataInit<'_, TestState>,
        ) {
        }
    }

    crate::delegate_dispatch2!(TestState);

    // Client object ids, allocated in this order by `Harness::new` (the
    // backend refuses gaps in the client id range).
    const REGISTRY: u32 = 2;
    const COMPOSITOR: u32 = 3;
    const SUBCOMPOSITOR: u32 = 4;
    const WM_BASE: u32 = 5;
    const WM_BASE_2: u32 = 6;
    const SURFACE: u32 = 7;
    const PARENT: u32 = 8;

    // Request opcodes used below.
    const WL_SURFACE_ATTACH: u16 = 1;
    const WL_SURFACE_COMMIT: u16 = 6;
    const WL_SUBCOMPOSITOR_GET_SUBSURFACE: u16 = 1;
    const XDG_WM_BASE_GET_XDG_SURFACE: u16 = 2;
    const XDG_SURFACE_DESTROY: u16 = 0;
    const XDG_SURFACE_GET_TOPLEVEL: u16 = 1;
    const XDG_TOPLEVEL_DESTROY: u16 = 0;

    fn words(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_ne_bytes()).collect()
    }

    fn string_arg(s: &str) -> Vec<u8> {
        let mut out = ((s.len() + 1) as u32).to_ne_bytes().to_vec();
        out.extend_from_slice(s.as_bytes());
        out.push(0);
        while out.len() % 4 != 0 {
            out.push(0);
        }
        out
    }

    fn word_at(bytes: &[u8], at: usize) -> u32 {
        u32::from_ne_bytes(bytes[at..at + 4].try_into().unwrap())
    }

    /// The registry name of the first advertised `interface`, from raw events.
    fn global_name(bytes: &[u8], interface: &str) -> Option<u32> {
        let mut at = 0;
        while at + 8 <= bytes.len() {
            let (object, size_op) = (word_at(bytes, at), word_at(bytes, at + 4));
            let size = (size_op >> 16) as usize;
            if size < 8 || at + size > bytes.len() {
                break;
            }
            if object == REGISTRY && size_op & 0xffff == 0 {
                let name = word_at(bytes, at + 8);
                let len = word_at(bytes, at + 12) as usize;
                if &bytes[at + 16..at + 16 + len - 1] == interface.as_bytes() {
                    return Some(name);
                }
            }
            at += size;
        }
        None
    }

    struct Harness {
        display: Display<TestState>,
        state: TestState,
        client: UnixStream,
        server_client: Client,
        inbox: Vec<u8>,
        next_id: u32,
        buffers: Vec<WlBuffer>,
    }

    impl Harness {
        fn new() -> Self {
            let display = Display::<TestState>::new().unwrap();
            let mut dh = display.handle();
            let compositor = CompositorState::new::<TestState>(&dh);
            let xdg = XdgShellState::new::<TestState>(&dh);
            let (server_end, client) = UnixStream::pair().unwrap();
            client.set_nonblocking(true).unwrap();
            let server_client = dh
                .insert_client(server_end, Arc::new(TestClient(CompositorClientState::default())))
                .unwrap();
            let mut h = Harness {
                display,
                state: TestState { compositor, xdg },
                client,
                server_client,
                inbox: Vec::new(),
                next_id: PARENT,
                buffers: Vec::new(),
            };

            h.send(1, 1, &words(&[REGISTRY])); // wl_display.get_registry
            h.roundtrip();
            let compositor = global_name(&h.inbox, "wl_compositor").expect("wl_compositor advertised");
            let subcompositor =
                global_name(&h.inbox, "wl_subcompositor").expect("wl_subcompositor advertised");
            let wm_base = global_name(&h.inbox, "xdg_wm_base").expect("xdg_wm_base advertised");
            h.bind(compositor, "wl_compositor", 5, COMPOSITOR);
            h.bind(subcompositor, "wl_subcompositor", 1, SUBCOMPOSITOR);
            h.bind(wm_base, "xdg_wm_base", 1, WM_BASE);
            h.bind(wm_base, "xdg_wm_base", 1, WM_BASE_2);
            h.send(COMPOSITOR, 0, &words(&[SURFACE])); // wl_compositor.create_surface
            h.send(COMPOSITOR, 0, &words(&[PARENT]));
            h.roundtrip();
            assert_eq!(h.error(), None, "harness setup raised a protocol error");
            h
        }

        fn send(&mut self, object: u32, opcode: u16, args: &[u8]) {
            let size = 8 + args.len() as u32;
            let mut out = words(&[object, (size << 16) | opcode as u32]);
            out.extend_from_slice(args);
            self.client.write_all(&out).unwrap();
        }

        fn bind(&mut self, name: u32, interface: &str, version: u32, id: u32) {
            let mut args = words(&[name]);
            args.extend(string_arg(interface));
            args.extend(words(&[version, id]));
            self.send(REGISTRY, 0, &args); // wl_registry.bind
        }

        fn new_id(&mut self) -> u32 {
            self.next_id += 1;
            self.next_id
        }

        /// Dispatch everything the client sent, then collect every event the
        /// server flushed back.
        fn roundtrip(&mut self) {
            self.display.dispatch_clients(&mut self.state).unwrap();
            let _ = self.display.flush_clients();
            let mut buf = [0u8; 4096];
            loop {
                match self.client.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => self.inbox.extend_from_slice(&buf[..n]),
                    Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::ConnectionReset) => break,
                    Err(e) => panic!("client read failed: {e}"),
                }
            }
        }

        /// The first `wl_display.error` the client received, as (object, code).
        fn error(&self) -> Option<(u32, u32)> {
            let bytes = &self.inbox;
            let mut at = 0;
            while at + 8 <= bytes.len() {
                let (object, size_op) = (word_at(bytes, at), word_at(bytes, at + 4));
                let size = (size_op >> 16) as usize;
                if object == 1 && size_op & 0xffff == 0 && at + 16 <= bytes.len() {
                    return Some((word_at(bytes, at + 8), word_at(bytes, at + 12)));
                }
                if size < 8 {
                    break;
                }
                at += size;
            }
            None
        }

        /// A fresh server-created `wl_buffer`; returns its protocol id.
        fn buffer(&mut self) -> u32 {
            let dh = self.display.handle();
            let buffer = self
                .server_client
                .create_resource::<WlBuffer, TestBuffer, TestState>(&dh, 1, TestBuffer)
                .unwrap();
            let id = buffer.id().protocol_id();
            self.buffers.push(buffer);
            id
        }

        fn attach(&mut self, buffer: u32) {
            self.send(SURFACE, WL_SURFACE_ATTACH, &words(&[buffer, 0, 0]));
        }

        fn commit(&mut self) {
            self.send(SURFACE, WL_SURFACE_COMMIT, &[]);
        }

        fn get_xdg_surface(&mut self, wm_base: u32) -> u32 {
            let id = self.new_id();
            self.send(wm_base, XDG_WM_BASE_GET_XDG_SURFACE, &words(&[id, SURFACE]));
            id
        }

        fn get_toplevel(&mut self, xdg_surface: u32) -> u32 {
            let id = self.new_id();
            self.send(xdg_surface, XDG_SURFACE_GET_TOPLEVEL, &words(&[id]));
            id
        }

        fn server_surface(&self) -> WlSurface {
            self.server_client
                .object_from_protocol_id::<WlSurface>(&self.display.handle(), SURFACE)
                .expect("wl_surface exists")
        }
    }

    fn refused_with(code: xdg_wm_base::Error) -> Option<(u32, u32)> {
        Some((WM_BASE, code as u32))
    }

    /// Qt hide→show: destroy the toplevel and its wrapper, then wrap the same
    /// `wl_surface` again and take the same role back.
    #[test]
    fn destroying_the_xdg_surface_releases_the_wl_surface_for_a_fresh_wrapper() {
        let mut h = Harness::new();
        let xdg_surface = h.get_xdg_surface(WM_BASE);
        let toplevel = h.get_toplevel(xdg_surface);
        h.roundtrip();
        assert_eq!(h.error(), None);

        h.send(toplevel, XDG_TOPLEVEL_DESTROY, &[]);
        h.send(xdg_surface, XDG_SURFACE_DESTROY, &[]);
        let again = h.get_xdg_surface(WM_BASE);
        h.get_toplevel(again);
        h.roundtrip();
        assert_eq!(
            h.error(),
            None,
            "re-wrapping a released wl_surface must be accepted"
        );
    }

    /// While a wrapper is live, a second `get_xdg_surface` is refused — on the
    /// same binding and on a second `xdg_wm_base` binding alike.
    #[test]
    fn a_second_wrapper_while_one_is_live_is_refused() {
        for second_binding in [WM_BASE, WM_BASE_2] {
            let mut h = Harness::new();
            let xdg_surface = h.get_xdg_surface(WM_BASE);
            h.get_toplevel(xdg_surface);
            h.roundtrip();
            assert_eq!(h.error(), None);

            h.get_xdg_surface(second_binding);
            h.roundtrip();
            assert_eq!(
                h.error(),
                Some((second_binding, xdg_wm_base::Error::Role as u32)),
                "a duplicate live wrapper must be refused (binding {second_binding})"
            );
        }
    }

    /// A role from another protocol is always refused, wrapper or not.
    #[test]
    fn a_non_xdg_role_is_refused() {
        let mut h = Harness::new();
        let subsurface = h.new_id();
        h.send(
            SUBCOMPOSITOR,
            WL_SUBCOMPOSITOR_GET_SUBSURFACE,
            &words(&[subsurface, SURFACE, PARENT]),
        );
        h.roundtrip();
        assert_eq!(h.error(), None);

        h.get_xdg_surface(WM_BASE);
        h.roundtrip();
        assert_eq!(h.error(), refused_with(xdg_wm_base::Error::Role));
    }

    #[test]
    fn a_fresh_xdg_surface_on_a_surface_with_a_pending_buffer_is_refused() {
        let mut h = Harness::new();
        let buffer = h.buffer();
        h.attach(buffer);
        h.get_xdg_surface(WM_BASE);
        h.roundtrip();
        assert_eq!(h.error(), refused_with(xdg_wm_base::Error::InvalidSurfaceState));
    }

    #[test]
    fn a_fresh_xdg_surface_on_a_surface_with_a_committed_buffer_is_refused() {
        let mut h = Harness::new();
        let buffer = h.buffer();
        h.attach(buffer);
        h.commit();
        h.get_xdg_surface(WM_BASE);
        h.roundtrip();
        assert_eq!(h.error(), refused_with(xdg_wm_base::Error::InvalidSurfaceState));
    }

    /// The NULL-attach rule through the request path: an uncommitted NULL
    /// attach leaves the committed buffer in force; a committed one clears it.
    #[test]
    fn an_uncommitted_null_attach_does_not_release_a_committed_buffer() {
        let mut h = Harness::new();
        let buffer = h.buffer();
        h.attach(buffer);
        h.commit();
        h.attach(0);
        h.get_xdg_surface(WM_BASE);
        h.roundtrip();
        assert_eq!(h.error(), refused_with(xdg_wm_base::Error::InvalidSurfaceState));

        let mut h = Harness::new();
        let buffer = h.buffer();
        h.attach(buffer);
        h.commit();
        h.attach(0);
        h.commit();
        h.get_xdg_surface(WM_BASE);
        h.roundtrip();
        assert_eq!(h.error(), None, "a committed NULL attach clears the buffer");
    }

    /// The default check itself, step by step (fails on the pre-fix default at
    /// the third assert).
    #[test]
    fn smithay_default_buffer_check_ignores_an_uncommitted_null_attach() {
        let mut h = Harness::new();
        let surface = h.server_surface();
        assert!(
            !surface_has_attached_or_committed_buffer(&surface),
            "a fresh surface has no buffer"
        );

        let buffer = h.buffer();
        h.attach(buffer);
        h.roundtrip();
        assert!(
            surface_has_attached_or_committed_buffer(&surface),
            "a pending buffer counts"
        );
        h.commit();
        h.roundtrip();
        assert!(
            surface_has_attached_or_committed_buffer(&surface),
            "a committed buffer counts"
        );

        h.attach(0);
        h.roundtrip();
        assert!(
            surface_has_attached_or_committed_buffer(&surface),
            "an UNCOMMITTED NULL attach must not hide the committed buffer"
        );
        h.commit();
        h.roundtrip();
        assert!(
            !surface_has_attached_or_committed_buffer(&surface),
            "only a committed NULL clears it"
        );
        assert_eq!(h.error(), None);
    }
}

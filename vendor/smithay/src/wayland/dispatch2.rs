use wayland_server::{Client, DataInit, DisplayHandle, New, Resource, backend::ClientId};

/// A simplified version of [`wayland_server::Dispatch`]
///
/// A future version of `wayland-server` will replace `Dispatch` with this.
pub trait Dispatch2<I: Resource, State> {
    /// Called when a request from a client is processed.
    fn request(
        &self,
        state: &mut State,
        client: &Client,
        resource: &I,
        request: I::Request,
        dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, State>,
    );

    /// Called when the object this user data is associated with has been destroyed.
    fn destroyed(&self, _state: &mut State, _client: ClientId, _resource: &I) {}
}

/// A simplified version of [`wayland_server::GlobalDispatch`]
///
/// A future version of `wayland-server` will replace `GlobalDispatch` with this.
pub trait GlobalDispatch2<I: Resource, State> {
    /// Called when a client has bound this global.
    fn bind(
        &self,
        state: &mut State,
        handle: &DisplayHandle,
        client: &Client,
        resource: New<I>,
        data_init: &mut DataInit<'_, State>,
    );

    /// Checks if the global should be advertised to some client.
    fn can_view(&self, _client: &Client) -> bool {
        true
    }
}

/// compd hook: what a [`RequestInterposer`] decides about a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreRequest {
    /// Hand the request to its [`Dispatch2`] handler as usual.
    Continue,
    /// Drop the request: the handler never sees it.
    ///
    /// Refusing a request that CREATES an object (a `New<…>` argument) leaves that
    /// object uninitialised, which wayland-server does not allow; refuse such
    /// requests only after posting a protocol error on `resource`, which
    /// disconnects the client.
    Refuse,
}

/// compd hook: a pre-request seam for states that use
/// `delegate_dispatch2!(@interpose State)`.
///
/// On the `Dispatch2` base every request reaches its handler through ONE blanket
/// `Dispatch` impl, so a compositor can no longer add its own `Dispatch` impl for a
/// specific interface to police requests (resource budgets, damage caps, role
/// checks): that impl would overlap the blanket one (E0119). This trait puts the
/// seam inside the blanket impl instead. `pre_request` runs before EVERY request
/// of every `Dispatch2`-backed object; an implementation picks the interfaces it
/// cares about by downcasting `request` (`(request as &dyn Any).downcast_ref::<
/// wl_surface::Request>()`) and returns [`PreRequest::Continue`] for the rest.
pub trait RequestInterposer: Sized {
    /// Inspect a request before its handler runs. Default: continue.
    #[allow(unused_variables)]
    fn pre_request<I>(
        &mut self,
        client: &Client,
        resource: &I,
        request: &I::Request,
        dhandle: &DisplayHandle,
    ) -> PreRequest
    where
        I: Resource + 'static,
        I::Request: 'static,
    {
        PreRequest::Continue
    }
}

/// Implement `Dispatch` and `GlobalDispatch` for every implementation of [`Dispatch2`] and
/// [`GlobalDispatch2`].
///
/// `delegate_dispatch2!(@interpose State)` generates the same impls, but every request
/// first passes through `<State as RequestInterposer>::pre_request` (compd hook, merge
/// map item 23); the state type must implement [`RequestInterposer`].
#[macro_export]
macro_rules! delegate_dispatch2 {
    (@interpose $ty:ty) => {
        impl<I, UserData> $crate::reexports::wayland_server::Dispatch<I, UserData> for $ty
            where
                I: $crate::reexports::wayland_server::Resource + 'static,
                <I as $crate::reexports::wayland_server::Resource>::Request: 'static,
                UserData: $crate::wayland::Dispatch2<I, $ty> {
            fn request(
                state: &mut Self,
                client: &$crate::reexports::wayland_server::Client,
                resource: &I,
                request: <I as $crate::reexports::wayland_server::Resource>::Request,
                data: &UserData,
                dhandle: &$crate::reexports::wayland_server::DisplayHandle,
                data_init: &mut $crate::reexports::wayland_server::DataInit<'_, Self>,
            ) {
                if <$ty as $crate::wayland::RequestInterposer>::pre_request::<I>(
                    state, client, resource, &request, dhandle,
                ) == $crate::wayland::PreRequest::Refuse
                {
                    return;
                }
                data.request(state, client, resource, request, dhandle, data_init);
            }

            fn destroyed(state: &mut Self, client: $crate::reexports::wayland_server::backend::ClientId, resource: &I, data: &UserData) {
                data.destroyed(state, client, resource);
            }
        }

        impl<I, UserData> $crate::reexports::wayland_server::GlobalDispatch<I, UserData> for $ty
            where
                I: $crate::reexports::wayland_server::Resource,
                UserData: $crate::wayland::GlobalDispatch2<I, $ty> {
            fn bind(
                state: &mut Self,
                dhandle: &$crate::reexports::wayland_server::DisplayHandle,
                client: &$crate::reexports::wayland_server::Client,
                resource: $crate::reexports::wayland_server::New<I>,
                data: &UserData,
                data_init: &mut $crate::reexports::wayland_server::DataInit<'_, Self>,
            ) {
                data.bind(state, dhandle, client, resource, data_init);
            }

            fn can_view(
                client: $crate::reexports::wayland_server::Client,
                data: &UserData
            ) -> bool {
                data.can_view(&client)
            }
        }
    };
    ($(@< $( $lt:tt $( : $clt:tt $(+ $dlt:tt )* )? ),+ >)? $ty:ty) => {
        impl<$( $( $lt $( : $clt $(+ $dlt )* )? ),+, )? I, UserData> $crate::reexports::wayland_server::Dispatch<I, UserData> for $ty
            where
                I: $crate::reexports::wayland_server::Resource,
                UserData: $crate::wayland::Dispatch2<I, $ty> {
            fn request(
                state: &mut Self,
                client: &$crate::reexports::wayland_server::Client,
                resource: &I,
                request: <I as $crate::reexports::wayland_server::Resource>::Request,
                data: &UserData,
                dhandle: &$crate::reexports::wayland_server::DisplayHandle,
                data_init: &mut $crate::reexports::wayland_server::DataInit<'_, Self>,
            ) {
                data.request(state, client, resource, request, dhandle, data_init);
            }

            fn destroyed(state: &mut Self, client: $crate::reexports::wayland_server::backend::ClientId, resource: &I, data: &UserData) {
                data.destroyed(state, client, resource);
            }
        }

        impl<$( $( $lt $( : $clt $(+ $dlt )* )? ),+, )? I, UserData> $crate::reexports::wayland_server::GlobalDispatch<I, UserData> for $ty
            where
                I: $crate::reexports::wayland_server::Resource,
                UserData: $crate::wayland::GlobalDispatch2<I, $ty> {
            fn bind(
                state: &mut Self,
                dhandle: &$crate::reexports::wayland_server::DisplayHandle,
                client: &$crate::reexports::wayland_server::Client,
                resource: $crate::reexports::wayland_server::New<I>,
                data: &UserData,
                data_init: &mut $crate::reexports::wayland_server::DataInit<'_, Self>,
            ) {
                data.bind(state, dhandle, client, resource, data_init);
            }

            fn can_view(
                client: $crate::reexports::wayland_server::Client,
                data: &UserData
            ) -> bool {
                data.can_view(&client)
            }
        }
    };
}

// compd hook guard: drive a REAL request through the
// generated `Dispatch` impl — a raw wire-format client over a socket pair — and
// prove the interposer runs before the handler and can refuse it.
#[cfg(test)]
mod pre_request_tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use std::sync::Arc;
    use wayland_server::backend::{ClientData, DisconnectReason};
    use wayland_server::protocol::wl_output::{self, WlOutput};
    use wayland_server::Display;

    #[derive(Default)]
    struct TestState {
        log: Vec<&'static str>,
        refuse: bool,
    }

    struct TestGlobal;
    struct TestObject;

    impl GlobalDispatch2<WlOutput, TestState> for TestGlobal {
        fn bind(
            &self,
            _state: &mut TestState,
            _handle: &DisplayHandle,
            _client: &Client,
            resource: New<WlOutput>,
            data_init: &mut DataInit<'_, TestState>,
        ) {
            data_init.init(resource, TestObject);
        }
    }

    impl Dispatch2<WlOutput, TestState> for TestObject {
        fn request(
            &self,
            state: &mut TestState,
            _client: &Client,
            _resource: &WlOutput,
            request: wl_output::Request,
            _dhandle: &DisplayHandle,
            _data_init: &mut DataInit<'_, TestState>,
        ) {
            if let wl_output::Request::Release = request {
                state.log.push("handler");
            }
        }
    }

    impl RequestInterposer for TestState {
        fn pre_request<I>(
            &mut self,
            _client: &Client,
            _resource: &I,
            request: &I::Request,
            _dhandle: &DisplayHandle,
        ) -> PreRequest
        where
            I: Resource + 'static,
            I::Request: 'static,
        {
            let is_release = (request as &dyn std::any::Any)
                .downcast_ref::<wl_output::Request>()
                .is_some_and(|r| matches!(r, wl_output::Request::Release));
            if is_release {
                self.log.push("pre");
                if self.refuse {
                    return PreRequest::Refuse;
                }
            }
            PreRequest::Continue
        }
    }

    crate::delegate_dispatch2!(@interpose TestState);

    struct NoData;
    impl ClientData for NoData {
        fn initialized(&self, _: ClientId) {}
        fn disconnected(&self, _: ClientId, _: DisconnectReason) {}
    }

    fn msg(object: u32, opcode: u16, args: &[u8]) -> Vec<u8> {
        let size = 8 + args.len() as u32;
        let mut out = Vec::new();
        out.extend_from_slice(&object.to_ne_bytes());
        out.extend_from_slice(&((size << 16) | opcode as u32).to_ne_bytes());
        out.extend_from_slice(args);
        out
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

    /// The registry name of the first advertised `interface`, from raw events.
    fn global_name(bytes: &[u8], interface: &str) -> Option<u32> {
        let word = |at: usize| u32::from_ne_bytes(bytes[at..at + 4].try_into().unwrap());
        let mut at = 0;
        while at + 8 <= bytes.len() {
            let (object, size_op) = (word(at), word(at + 4));
            let size = (size_op >> 16) as usize;
            let opcode = size_op & 0xffff;
            if object == 2 && opcode == 0 {
                let name = word(at + 8);
                let len = word(at + 12) as usize;
                let text = &bytes[at + 16..at + 16 + len - 1];
                if text == interface.as_bytes() {
                    return Some(name);
                }
            }
            at += size;
        }
        None
    }

    fn run(refuse: bool) -> Vec<&'static str> {
        let mut display = Display::<TestState>::new().unwrap();
        let mut dh = display.handle();
        dh.create_global::<TestState, WlOutput, _>(4, TestGlobal);
        let (server_end, mut client) = UnixStream::pair().unwrap();
        dh.insert_client(server_end, Arc::new(NoData)).unwrap();
        let mut state = TestState { refuse, ..Default::default() };

        // wl_display(1).get_registry(new id 2)
        client.write_all(&msg(1, 1, &2u32.to_ne_bytes())).unwrap();
        display.dispatch_clients(&mut state).unwrap();
        display.flush_clients().unwrap();
        client.set_nonblocking(true).unwrap();
        let mut buf = vec![0u8; 4096];
        let n = client.read(&mut buf).unwrap();
        let name = global_name(&buf[..n], "wl_output").expect("wl_output advertised");

        // wl_registry(2).bind(name, "wl_output", 4, new id 3), then wl_output(3).release
        let mut args = name.to_ne_bytes().to_vec();
        args.extend(string_arg("wl_output"));
        args.extend_from_slice(&4u32.to_ne_bytes());
        args.extend_from_slice(&3u32.to_ne_bytes());
        client.set_nonblocking(false).unwrap();
        client.write_all(&msg(2, 0, &args)).unwrap();
        client.write_all(&msg(3, 0, &[])).unwrap();
        display.dispatch_clients(&mut state).unwrap();
        state.log
    }

    #[test]
    fn interposer_runs_before_the_handler() {
        assert_eq!(run(false), vec!["pre", "handler"]);
    }

    #[test]
    fn interposer_can_refuse_a_request() {
        assert_eq!(run(true), vec!["pre"]);
    }
}

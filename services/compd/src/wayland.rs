use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::{EventLoop, Interest, Mode, PostAction};
use smithay::reexports::wayland_server::Display;
use smithay::wayland::compositor::CompositorClientState;
use smithay::wayland::socket::ListeningSocketSource;
use std::ffi::OsString;
use std::sync::Arc;
use world::state::Loop;
use protocols::wayland::connection::record::record::WaylandClientSession;

pub struct Socket {
    pub name: OsString,
    pub listening_socket: ListeningSocketSource,
}

pub fn create_socket(name: Option<&str>) -> Socket {
    // `--socket NAME` binds that exact name; otherwise the next available
    // `wayland-N`.
    let listening_socket = match name {
        Some(n) => ListeningSocketSource::with_name(n)
            .unwrap_or_else(|e| panic!("cannot bind wayland socket {n:?}: {e}")),
        None => ListeningSocketSource::new_auto().unwrap(),
    };

    // Get the name of the listening socket.
    // Clients will connect to this socket.
    let socket_name = listening_socket.socket_name().to_os_string();
    Socket {
        name: socket_name,
        listening_socket,
    }
}

// pub fn create_socket_proprietary(uuid: String) -> Socket {
//     // Creates a new listening socket, automatically choosing the next available `wayland` socket name.
//     let listening_socket = ListeningSocketSource::with_name(format!("compd-{:?}", uuid)).unwrap();
//
//     // Get the name of the listening socket.
//     // Clients will connect to this socket.
//     let socket_name = listening_socket.socket_name().to_os_string();
//     Socket {
//         name: socket_name,
//         listening_socket,
//     }
// }

pub fn register(
    socket: Socket,
    event_loop: &mut EventLoop<Loop>,
    // proprietary: bool,
) {
    let loop_handle = event_loop.handle();

    loop_handle
        .insert_source(socket.listening_socket, move |client_stream, _, state| {
            // Inside the callback, you should insert the client into the display.
            //
            // You may also associate some data with the client when inserting the client.

            // The client connects once. next top levels will not infer UUID. only the first top level will infer the root UUID.
            let client = WaylandClientSession {
                compositor_state: CompositorClientState::default(),
                proprietary: false,
                // proprietary,
            };

            state
                .state
                .output
                .display_handle
                .insert_client(client_stream, Arc::new(client))
                .unwrap();
        })
        .unwrap_or_else(|e| model::abort!("failed to init the wayland event source: {e:?}"));
}

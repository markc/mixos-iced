// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native queue ownership probe. Backend roundtrips read actual callbacks but
//! deliberately never dispatch the typed queue. No compositor outcome is inferred.
use std::sync::Arc;
use wayland_client::{Connection, Dispatch, QueueHandle, protocol::wl_callback};

struct State;
impl Dispatch<wl_callback::WlCallback, Arc<()>> for State {
    fn event(_: &mut Self, _: &wl_callback::WlCallback, _: wl_callback::Event,
        _: &Arc<()>, _: &Connection, _: &QueueHandle<Self>) {
        panic!("the retirement probe never dispatches typed callbacks");
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    for close_first in [false, true] {
        let connection = Connection::connect_to_env()?;
        let queue = connection.new_event_queue::<State>();
        let alive = Arc::new(());
        let weak = Arc::downgrade(&alive);
        let callback = connection.display().sync(&queue.handle(), alive);
        if close_first {
            drop(queue);
            drop(callback);
            assert!(weak.upgrade().is_some(), "the native backend still owns the requested callback");
            connection.roundtrip()?;
            assert!(weak.upgrade().is_none(), "delivery after close must not recreate a queue cycle");
        } else {
            connection.roundtrip()?;
            drop(callback);
            assert!(weak.upgrade().is_some(), "the actual undrained queue owns native callback userdata");
            drop(queue);
            assert!(weak.upgrade().is_none(), "retirement must release native callback userdata while the connection stays alive");
            connection.roundtrip()?;
        }
    }
    println!("QUEUE_RETIREMENT PASS native_undrained=true late_delivery=true typed_dispatch=false");
    Ok(())
}

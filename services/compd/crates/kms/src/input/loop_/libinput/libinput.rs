//! The libinput backend as a loop source. Registration and the closures that
//! touch `Loop` live in `native.wire/wire.input` (Law 4).

use smithay::backend::libinput::LibinputInputBackend;
use smithay::reexports::input::Libinput;

pub use smithay::backend::libinput::LibinputInputBackend as LibinputSource;

pub fn source(context: Libinput) -> LibinputSource {
    LibinputInputBackend::new(context)
}

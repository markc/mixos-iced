//! Libinput context construction + seat assignment, generic over smithay's
//! `Session` trait — this crate never names libseat (Law 1).
//! Failure policy: a native compositor without input cannot run — panic
//! (the original unwrapped here too).

use smithay::backend::libinput::LibinputSessionInterface;
use smithay::backend::session::Session;
use smithay::reexports::input::Libinput;

pub fn create<S>(session: S, seat: &str) -> Libinput
where
    S: Session + 'static,
    LibinputSessionInterface<S>: From<S>,
{
    let mut context = Libinput::new_with_udev::<LibinputSessionInterface<S>>(session.into());
    context
        .udev_assign_seat(seat)
        .unwrap_or_else(|_| abort!("libinput seat assignment failed for seat {seat}"));
    context
}

//! LibSeatSession + notifier construction. (Ex wire.rs `new()` step 1.)
//! Failure policy: a compositor without a session cannot run — panic.
//!
//! compd: on a VT the session is opened only once that VT is the active one
//! ([`super::super::vt_wait`]): seatd re-reads the current VT only when a
//! client is ADDED (or on a VT signal it receives only for a VT it put in
//! process mode, which it does only for an active client), so a session
//! opened on an inactive VT is never enabled and its device opens are
//! refused (EPERM). Refusing to start on an inactive VT and leaving the
//! restart to the unit would also work; compd waits for the switch as an
//! event instead.

use smithay::backend::session::libseat::{LibSeatSession, LibSeatSessionNotifier};
use smithay::backend::session::Session;

pub fn create() -> (LibSeatSession, LibSeatSessionNotifier) {
    let own_vt = super::super::vt_wait::own_vt();
    loop {
        super::super::vt_wait::wait_for_own_vt();
        let (session, notifier) =
            LibSeatSession::new().expect("libseat session creation failed");
        if let Some(own) = own_vt {
            if !session.is_active() {
                if !matches!(super::super::vt_wait::active_vt(), Some(active) if active != own) {
                    panic!("libseat session opened inactive although VT{own} is active: seatd is bound to a different tty than standard input; check TTYPath/StandardInput");
                }
                info!("libseat session opened inactive after switching away from VT{own}; dropping it and waiting for VT activation before retrying");
                drop(notifier);
                drop(session);
                continue;
            }
        }
        info!("libseat session created (seat: {}, active: {})", session.seat(), session.is_active());
        return (session, notifier);
    }
}

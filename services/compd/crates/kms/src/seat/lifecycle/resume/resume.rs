//! Activate ordering protocol (TTY switch back / session resume), including
//! the resume watchdog as an internal module.
//!
//! Order is the protocol (ex wire.rs session closure):
//! 1. resume input
//! 2. activate display, forcing the modeset that reclaims the pipe (e.g. from GDM)
//! 3. reset the running surface state
//! 4. reset buffers
//! 5. remap the output into the space
//!
//! Failure policy: resume-step failures are the SELF-RECOVERING class — the
//! kernel may not have finished handing the device back (a timing matter the
//! watchdog exists for), so steps report and the watchdog drives recovery.
//! This is the explicit exception to the crash-first rule.

pub struct ResumeSteps<RI, AD, RS, RB, RM>
where
    RI: FnOnce() -> Result<(), String>,
    AD: FnOnce(bool) -> Result<(), String>,
    RS: FnOnce() -> Result<(), String>,
    RB: FnOnce(),
    RM: FnOnce(),
{
    pub resume_input: RI,
    /// `force: bool` — `true` forces the reclaiming modeset.
    pub activate_display: AD,
    pub reset_surface: RS,
    pub reset_buffers: RB,
    pub remap_output: RM,
}

pub fn resume<RI, AD, RS, RB, RM>(steps: ResumeSteps<RI, AD, RS, RB, RM>)
where
    RI: FnOnce() -> Result<(), String>,
    AD: FnOnce(bool) -> Result<(), String>,
    RS: FnOnce() -> Result<(), String>,
    RB: FnOnce(),
    RM: FnOnce(),
{
    info!("session resume: resuming input");
    if let Err(e) = (steps.resume_input)() {
        warn!("input resume failed (watchdog will keep kicking): {e}");
    }
    info!("session resume: activating display (forced modeset)");
    if let Err(e) = (steps.activate_display)(true) {
        error!("display activate failed (watchdog will keep kicking): {e}");
    }
    if let Err(e) = (steps.reset_surface)() {
        error!("surface state reset failed (watchdog will keep kicking): {e}");
    }
    (steps.reset_buffers)();
    (steps.remap_output)();
}

pub mod watchdog {
    //! Resume watchdog: kick a full render every interval until a REAL vblank
    //! (observed via the `vblank_seen` probe) arrives, then drop itself.
    //!
    //! CHECK carried from the original: consider a max duration — but nvidia
    //! may legitimately take long to resume after sleep. Bounding is part of
    //! the recorded fixes backlog, not this restructure.

    use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
    use smithay::reexports::calloop::{LoopHandle, RegistrationToken};
    use std::time::Duration;

    pub const KICK_INTERVAL: Duration = Duration::from_millis(16);

    /// Arm the watchdog on a loop with user-data `D` (Law 4: `D` is opaque
    /// here; native instantiates it with `Loop`). Registration failure is not
    /// self-recovering — panic.
    pub fn arm<D, P, C, K>(
        handle: &LoopHandle<'static, D>,
        mut vblank_seen: P,
        mut clear_token: C,
        mut kick: K,
    ) -> RegistrationToken
    where
        D: 'static,
        P: FnMut(&mut D) -> bool + 'static,
        C: FnMut(&mut D) + 'static,
        K: FnMut(&mut D) + 'static,
    {
        handle
            .insert_source(Timer::immediate(), move |_now, _meta, data| {
                if vblank_seen(data) {
                    clear_token(data); // about to Drop — keep token honest
                    return TimeoutAction::Drop;
                }
                kick(data);
                TimeoutAction::ToDuration(KICK_INTERVAL)
            })
            .expect("resume watchdog timer registration failed")
    }
}

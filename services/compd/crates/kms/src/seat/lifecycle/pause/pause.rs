//! Pause ordering protocol (TTY switch away / session pause).
//!
//! Order is the protocol: pause the display path FIRST (stop touching DRM fds
//! that are about to be revoked), THEN suspend input. The affected pieces are
//! passed in as capabilities — this crate knows nothing about DRM or libinput.

pub fn pause(pause_display: impl FnOnce(), suspend_input: impl FnOnce()) {
    info!("session pause: pausing display path");
    pause_display();
    info!("session pause: suspending input");
    suspend_input();
}

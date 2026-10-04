//! systemd readiness notification (`sd_notify(3)`), without libsystemd.
//!
//! The compositor tells its service manager it is
//! READY once the first frame has actually been presented (nested: the first
//! swap; KMS: the first page-flip completion), plus `STATUS=` lines along the
//! way. The protocol is one datagram of newline-separated `KEY=VALUE` pairs to
//! the unix socket named by `$NOTIFY_SOCKET`; a leading `@` names an abstract
//! socket. With no `$NOTIFY_SOCKET` every call is a no-op, so running outside
//! systemd costs nothing.
//!
//! [`init`] captures the socket and removes the variable from the environment,
//! so clients the compositor launches do not inherit it and cannot speak for the
//! compositor (systemd's `NotifyAccess=main` would reject them anyway; this keeps
//! them from trying).

use std::io;
use std::os::unix::net::UnixDatagram;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

static SOCKET: OnceLock<Option<String>> = OnceLock::new();
static READY_SENT: AtomicBool = AtomicBool::new(false);

/// Capture `$NOTIFY_SOCKET` and remove it from the environment. Call first thing in
/// `main`, while the process is still single-threaded (removing an environment
/// variable is only sound then). Later calls are no-ops.
pub fn init() {
    SOCKET.get_or_init(|| {
        let v = std::env::var("NOTIFY_SOCKET").ok().filter(|s| !s.is_empty());
        // SAFETY: called at the top of `main`, before any thread is spawned.
        unsafe { std::env::remove_var("NOTIFY_SOCKET") };
        v
    });
}

fn socket() -> Option<&'static str> {
    SOCKET
        .get_or_init(|| std::env::var("NOTIFY_SOCKET").ok().filter(|s| !s.is_empty()))
        .as_deref()
}

/// Send one notification datagram (`state` is `KEY=VALUE` lines). `Ok(false)` when
/// not running under a notifying service manager.
pub fn notify(state: &str) -> io::Result<bool> {
    let Some(path) = socket() else { return Ok(false) };
    send_to(path, state)?;
    Ok(true)
}

/// One datagram to a `$NOTIFY_SOCKET`-style address: a filesystem path, or `@name`
/// for the abstract namespace.
fn send_to(path: &str, state: &str) -> io::Result<()> {
    let sock = UnixDatagram::unbound()?;
    if let Some(name) = path.strip_prefix('@') {
        use std::os::linux::net::SocketAddrExt;
        let addr = std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes())?;
        sock.send_to_addr(state.as_bytes(), &addr)?;
    } else {
        sock.send_to(state.as_bytes(), path)?;
    }
    Ok(())
}

/// `STATUS=<text>`: free-form, shown by `systemctl status`. Newlines are flattened.
pub fn status(text: &str) {
    let _ = notify(&format!("STATUS={}", text.replace('\n', " ")));
}

/// `READY=1` with a status line — sent at most once per process, so every frame
/// path can call it unconditionally. Returns true on the call that sent it.
pub fn ready_once(text: &str) -> bool {
    if READY_SENT.swap(true, Ordering::AcqRel) {
        return false;
    }
    let _ = notify(&format!("READY=1\nSTATUS={}", text.replace('\n', " ")));
    true
}

/// `STOPPING=1` at the start of an orderly shutdown.
pub fn stopping() {
    let _ = notify("STOPPING=1\nSTATUS=shutting down");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn datagram_reaches_a_path_socket() {
        let dir = std::env::temp_dir().join(format!("compd-sdnotify-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("notify.sock");
        let _ = std::fs::remove_file(&path);
        let rx = UnixDatagram::bind(&path).unwrap();
        send_to(path.to_str().unwrap(), "READY=1\nSTATUS=x").unwrap();
        let mut buf = [0u8; 64];
        let n = rx.recv(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"READY=1\nSTATUS=x");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn datagram_reaches_an_abstract_socket() {
        use std::os::linux::net::SocketAddrExt;
        let name = format!("compd-sdnotify-test-{}", std::process::id());
        let addr = std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
        let rx = UnixDatagram::bind_addr(&addr).unwrap();
        send_to(&format!("@{name}"), "STATUS=abstract").unwrap();
        let mut buf = [0u8; 64];
        let n = rx.recv(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"STATUS=abstract");
    }
}

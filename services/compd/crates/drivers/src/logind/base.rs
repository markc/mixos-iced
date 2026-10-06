//! logind (systemd-login1) client for system power actions the lid policy needs.
//! Owns a blocking D-Bus system connection; lives in `Orchestrator.kernel`
//! storage by token so the kernel request-drain can reach it (kernel → rim dep
//! direction). Plain blocking login1 calls over the system bus.

use slots::storage::token::base::{Token, TokenMut};
#[cfg(feature = "desktop-dbus")]
use zbus::blocking::{Connection, Proxy};

const DEST: &str = "org.freedesktop.login1";
const PATH: &str = "/org/freedesktop/login1";
const MANAGER: &str = "org.freedesktop.login1.Manager";

/// A live logind connection. Cheap to construct lazily; `None` in the token
/// until populated (and stays `None` on systems without logind).
pub struct LogindHandle {
    #[cfg(feature = "desktop-dbus")]
    conn: Connection,
}

#[cfg(feature = "desktop-dbus")]
impl LogindHandle {
    /// Open a blocking connection to the system bus.
    pub fn new() -> zbus::Result<Self> {
        Ok(Self {
            conn: Connection::system()?,
        })
    }

    fn manager(&self) -> zbus::Result<Proxy<'_>> {
        Proxy::new(&self.conn, DEST, PATH, MANAGER)
    }

    /// Request a system suspend. `interactive = false` so it does not block on a
    /// polkit prompt — lid-close suspend should be unattended.
    pub fn suspend(&self) {
        match self.manager() {
            Ok(proxy) => {
                if let Err(e) = proxy.call_method("Suspend", &(false,)) {
                    warn!("logind Suspend failed: {e}");
                }
            }
            Err(e) => warn!("logind manager proxy failed: {e}"),
        }
    }
}

#[cfg(not(feature = "desktop-dbus"))]
impl LogindHandle {
    pub fn new() -> std::io::Result<Self> {
        Err(std::io::Error::new(std::io::ErrorKind::Unsupported,
            "system suspend is unavailable: no native power service or logind adapter"))
    }
    pub fn suspend(&self) {
        warn!("system suspend is unavailable in this build");
    }
}

/// The logind connection, populated post-init by the backend (like `GPU_BINDING`).
/// `None` until populated / when logind is unavailable.
pub static LOGIND: Token<Option<LogindHandle>> = Token::new();
pub static LOGIND_MUT: TokenMut<Option<LogindHandle>> = TokenMut::new(&LOGIND);

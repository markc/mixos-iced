//! `xwayland.enabled`: the persisted startup switch.
//!
//! Startup-read: compd decides once whether to start Xwayland
//! ([`startup`], called by compd's `xwayland::register`). A props set
//! changes the CONFIGURED value that get/describe read and writes it to the
//! per-socket file for the next start; the running lifecycle is untouched.
//! Precedence: `COMPD_XWAYLAND` (env veto, works when the
//! file or the props surface is the problem) > the persisted file > `true`.
//!
//! The file is `<etc>/comp/xwayland-enabled.<socket>`, `<etc>` being the
//! MixOS etc dir (`config::path(Dir::Etc)`): `$MIXOS_ETC`, else
//! `$MIXOS/etc`, else `/etc/mixos` for root, else `$XDG_CONFIG_HOME/mixos`
//! or `$HOME/.config/mixos`.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

/// The env override.
pub const ENV: &str = "COMPD_XWAYLAND";

struct Switch {
    path: PathBuf,
    configured: AtomicBool,
    /// Why the startup resolution fell through a level, for the caller to log.
    note: Option<String>,
}

static SWITCH: OnceLock<Switch> = OnceLock::new();

/// The startup decision, as compd's `register` logs it.
pub struct Startup {
    pub enabled: bool,
    pub path: &'static Path,
    pub note: Option<&'static str>,
}

/// Resolve (once) and report the startup value. Reads `WAYLAND_DISPLAY`
/// for the socket name, so call it after compd exports its own socket.
pub fn startup() -> Startup {
    let switch = switch();
    Startup {
        enabled: switch.configured.load(Ordering::Relaxed),
        path: &switch.path,
        note: switch.note.as_deref(),
    }
}

/// The configured value (`xwayland.enabled`).
pub fn configured() -> bool {
    switch().configured.load(Ordering::Relaxed)
}

/// `xwayland.persist_path`.
pub fn persist_path() -> &'static Path {
    &switch().path
}

/// Set the configured value and persist it. Returns `(old, persisted)`.
/// The persist runs on every set, not only on a change: the file is the
/// durability contract, and a deduped write would make the reply lie on a
/// retry after `persisted: false`.
pub fn set(value: bool) -> (bool, bool) {
    let switch = switch();
    let old = switch.configured.swap(value, Ordering::Relaxed);
    (old, write(&switch.path, value).is_ok())
}

fn switch() -> &'static Switch {
    SWITCH.get_or_init(|| {
        let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_default();
        let socket = Path::new(&display)
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("wayland-0");
        let path = config::path(config::Dir::Etc).join("comp").join(format!("xwayland-enabled.{socket}"));
        let (file, read_note) = match std::fs::read_to_string(&path) {
            Ok(text) => (Some(text), None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (None, None),
            Err(error) => (None, Some(format!("unreadable persisted file ({error}); defaulting to enabled"))),
        };
        let env = std::env::var(ENV).ok();
        let (enabled, note) = resolve(env.as_deref(), file.as_deref());
        Switch {
            path,
            configured: AtomicBool::new(enabled),
            note: note.map(str::to_string).or(read_note),
        }
    })
}

/// The write half: plain write; a torn
/// write reads as an unparseable word, which defaults to `true`.
fn write(path: &Path, value: bool) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, if value { "true\n" } else { "false\n" })
}

/// One env value; `None` = unrecognised, including empty, so a blank
/// override cannot silently disable anything.
fn parse_env(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "0" | "false" | "off" | "no" => Some(false),
        "1" | "true" | "on" | "yes" => Some(true),
        _ => None,
    }
}

/// One persisted body: only the two words the writer produces.
fn parse_file(text: &str) -> Option<bool> {
    match text.trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// The pure precedence decision: env > file > `true`, with the reason a
/// level was skipped.
fn resolve(env: Option<&str>, file: Option<&str>) -> (bool, Option<&'static str>) {
    let mut note = None;
    if let Some(value) = env {
        match parse_env(value) {
            Some(enabled) => return (enabled, None),
            None => note = Some("unrecognised COMPD_XWAYLAND; fell through to the persisted file"),
        }
    }
    if let Some(text) = file {
        match parse_file(text) {
            Some(enabled) => return (enabled, note),
            None => return (true, Some("unparseable persisted xwayland.enabled; defaulting to enabled")),
        }
    }
    (true, note)
}

// ── The running server's lifecycle ──

/// `xwayland.state`: where the X server is in its life. compd's loader
/// writes it; the Bus reads it beside `xwayland.display` (null unless
/// `ready`) and `xwayland.failures`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerState {
    /// Never started: the switch is off, or the session has no X yet.
    Off,
    /// Spawned, waiting for the server's ready signal.
    Starting,
    /// The window manager is up and `DISPLAY` is published.
    Ready,
    /// It died; the one retry is armed (`XWAYLAND_RETRY_DELAY_SECS`).
    Retrying,
    /// It died with no retry left: X11 is down until compd restarts.
    Failed,
}

impl ServerState {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Starting => "starting",
            Self::Ready => "ready",
            Self::Retrying => "retrying",
            Self::Failed => "failed",
        }
    }
}

struct Lifecycle {
    state: ServerState,
    /// Deaths this session (startup crashes included).
    failures: u32,
    /// Generations started this session: the first is 1, the retry 2
    /// (the descriptor's `GENERATION`).
    generation: u64,
    retry: policy::x11::XwaylandRetryPolicy,
}

impl Lifecycle {
    const fn new() -> Self {
        Self {
            state: ServerState::Off,
            failures: 0,
            generation: 0,
            retry: policy::x11::XwaylandRetryPolicy::new(),
        }
    }

    fn failure(&mut self) -> policy::x11::XwaylandRetryDecision {
        self.failures = self.failures.saturating_add(1);
        let decision = self.retry.on_failure();
        self.state = match decision {
            policy::x11::XwaylandRetryDecision::Retry => ServerState::Retrying,
            policy::x11::XwaylandRetryDecision::StayFailed => ServerState::Failed,
        };
        decision
    }
}

static LIFECYCLE: std::sync::Mutex<Lifecycle> = std::sync::Mutex::new(Lifecycle::new());

fn lifecycle() -> std::sync::MutexGuard<'static, Lifecycle> {
    LIFECYCLE.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A generation was spawned (the first, or the retry).
pub fn note_starting() {
    let mut life = lifecycle();
    life.state = ServerState::Starting;
    life.generation = life.generation.saturating_add(1);
}

/// The current generation (0 before the first start).
pub fn generation() -> u64 {
    lifecycle().generation
}

// ── The per-socket DISPLAY descriptor ──
//
// compd publishes `$XDG_RUNTIME_DIR/compd/<socket>.xwayland.env`
// (`DISPLAY=:N`, `GENERATION=G`, mode 0600, written atomically) when a
// generation's window manager is up, and removes it when the generation
// ends. Launchers read it once and hand `DISPLAY` to each X client; nothing
// global is set. The path is a runtime contract other programs read.

/// The descriptor's path under `runtime_dir` for Wayland socket `socket`.
pub fn descriptor_path_in(runtime_dir: &Path, socket: &str) -> PathBuf {
    runtime_dir.join("compd").join(format!("{socket}.xwayland.env"))
}

/// This compositor's descriptor path: `XDG_RUNTIME_DIR` and the socket
/// compd exported as `WAYLAND_DISPLAY`. `None` without either.
pub fn descriptor_path() -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").filter(|dir| !dir.is_empty())?;
    let socket = std::env::var("WAYLAND_DISPLAY").ok().filter(|socket| !socket.is_empty())?;
    Some(descriptor_path_in(Path::new(&dir), &socket))
}

/// A mode-0600 sibling temporary,
/// fsynced, renamed over `path`. A failed write removes the temporary.
pub fn publish_descriptor_at(path: &Path, display: u32, generation: u64) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let parent = path.parent().ok_or_else(|| std::io::Error::other("descriptor path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    let write = |temporary: &Path| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(temporary)?;
        file.write_all(format!("DISPLAY=:{display}\nGENERATION={generation}\n").as_bytes())?;
        file.sync_all()
    };
    let result = write(&temporary).and_then(|()| std::fs::rename(&temporary, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// Publish this compositor's descriptor for the current generation.
pub fn publish_descriptor(display: u32) -> std::io::Result<()> {
    let path = descriptor_path().ok_or_else(|| std::io::Error::other("no XDG_RUNTIME_DIR or WAYLAND_DISPLAY"))?;
    publish_descriptor_at(&path, display, generation())
}

/// Remove the descriptor (the generation ended). Missing is fine.
pub fn remove_descriptor() -> std::io::Result<()> {
    let Some(path) = descriptor_path() else { return Ok(()) };
    match std::fs::remove_file(path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// The window manager started on the new server.
pub fn note_ready() {
    lifecycle().state = ServerState::Ready;
}

/// The server died (or never came up). Counts it and spends the one retry
/// credit: [`Retry`](policy::x11::XwaylandRetryDecision::Retry) the
/// first time, `StayFailed` after.
pub fn note_failure() -> policy::x11::XwaylandRetryDecision {
    lifecycle().failure()
}

/// The retry could not even be armed: X11 is down for the session, with no
/// further death to count.
pub fn note_gave_up() {
    lifecycle().state = ServerState::Failed;
}

/// `xwayland.state`.
pub fn state() -> ServerState {
    lifecycle().state
}

/// `xwayland.failures`.
pub fn failures() -> u32 {
    lifecycle().failures
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_descriptor_is_comps_file_atomic_and_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("policy-host-xdesc-{}", std::process::id()));
        let path = descriptor_path_in(&dir, "wayland-9");
        assert!(path.ends_with("compd/wayland-9.xwayland.env"));
        publish_descriptor_at(&path, 3, 2).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "DISPLAY=:3\nGENERATION=2\n");
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(!path.with_extension("env.tmp").exists(), "no temporary left behind");
        // A rewrite (the retry's generation) replaces it whole.
        publish_descriptor_at(&path, 4, 3).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "DISPLAY=:4\nGENERATION=3\n");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_death_counts_and_spends_the_one_retry() {
        use policy::x11::XwaylandRetryDecision;
        let mut life = Lifecycle::new();
        assert_eq!((life.state, life.failures), (ServerState::Off, 0));
        assert_eq!(life.failure(), XwaylandRetryDecision::Retry);
        assert_eq!((life.state.name(), life.failures), ("retrying", 1));
        assert_eq!(life.failure(), XwaylandRetryDecision::StayFailed);
        assert_eq!((life.state.name(), life.failures), ("failed", 2));
    }

    #[test]
    fn env_beats_file_beats_default() {
        assert_eq!(resolve(Some("off"), Some("true")), (false, None));
        assert_eq!(resolve(None, Some("false\n")), (false, None));
        assert_eq!(resolve(None, None), (true, None));
        assert!(resolve(Some(""), Some("false")).1.is_some());
        assert!(!resolve(Some(""), Some("false")).0);
        assert!(resolve(None, Some("nope")).0);
    }

    #[test]
    fn the_write_round_trips_through_the_parser() {
        let dir = std::env::temp_dir().join(format!("policy-host-xwayland-{}", std::process::id()));
        let path = dir.join("comp").join("xwayland-enabled.wayland-test");
        write(&path, false).unwrap();
        assert_eq!(parse_file(&std::fs::read_to_string(&path).unwrap()), Some(false));
        write(&path, true).unwrap();
        assert_eq!(parse_file(&std::fs::read_to_string(&path).unwrap()), Some(true));
        let _ = std::fs::remove_dir_all(dir);
    }
}

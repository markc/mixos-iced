//! Call-site-agnostic control of **media playback transport** (play / pause /
//! next / …) for whatever application is currently playing — Spotify, a
//! browser, mpv, a music player, etc. — via the freedesktop **MPRIS2** D-Bus
//! interface (`org.mpris.MediaPlayer2.Player`).
//!
//! This is the sibling of `audio.rs`: that module controls the *sink* (system
//! output volume); this one controls the *player* (what is playing). The two
//! are independent — the volume keys never touch the player, and play/pause
//! never touches the sink.
//!
//! The off-thread part lives entirely inside this module — the caller never
//! spawns a thread. A small worker thread owns the D-Bus connection and applies
//! commands; every public method is fire-and-forget: it posts a command and
//! returns immediately, so a hung or slow media player can never stall your
//! compositor's event loop. This mirrors `audio.rs`'s "don't make me manage
//! threads" contract.
//!
//! "Which player?" is delegated to `mpris`'s `find_active()`, which picks, in
//! order: a player that is **Playing**, else one that is **Paused**, else one
//! that has track metadata, else the first it finds. That matches what a user
//! intuitively means by "the media key should affect what I'm listening to".
//!
//! D-Bus round-trip errors happen asynchronously on the worker thread and are
//! reported via `tracing` (a "no active player" situation is logged at debug
//! and otherwise ignored — a media key with nothing playing is a no-op). The
//! only error a *caller* can observe is [`MediaError::Disconnected`], which
//! happens only if the controller has already been dropped.
//!
//! ## Dependencies
//! ```toml
//! mpris = "2"      # pure-API wrapper over the system libdbus client
//! tracing = "0.1"  # smithay already pulls this in
//! ```
//! The `mpris` crate links the system D-Bus client library, so you need its
//! dev headers at build time: Fedora `dnf install dbus-devel`, Debian/Ubuntu
//! `apt install libdbus-1-dev`. (libdbus is present on every Linux desktop
//! session, including PipeWire-only setups.)
//!
//! ## Scope
//! Transport actions only. Reading *what is playing* (title/artist/art +
//! playback status) for the OSD/notch is the next layer; it slots onto the same
//! worker via a reply channel (issue a query command, send the snapshot back
//! over a `Sender`, wake the main loop with a calloop `Ping` exactly as the
//! audio watcher does). Left out here to keep this focused on play/pause.

use std::fmt;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use mpris::{FindingError, Player, PlayerFinder};

/// The only error a caller of [`MediaController`] can observe. The actual D-Bus
/// failures are asynchronous (they happen on the worker thread) and are logged
/// there rather than returned here.
#[derive(Debug)]
pub enum MediaError {
    /// The worker thread is gone (the controller was dropped). You should not
    /// see this in normal use.
    Disconnected,
}

impl fmt::Display for MediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MediaError::Disconnected => f.write_str("media controller worker thread is gone"),
        }
    }
}

impl std::error::Error for MediaError {}

/// One transport command, posted from the caller's thread to the worker.
#[derive(Debug, Clone, Copy)]
enum MediaCommand {
    PlayPause,
    Play,
    Pause,
    Stop,
    Next,
    Previous,
}

/// Controls the currently-active MPRIS media player.
///
/// Construct once (e.g. store it on your compositor state next to the
/// `AudioController`) and call the transport methods from your keyboard handler.
/// Cheap to construct: it does not block or touch D-Bus on the calling thread —
/// the connection is established lazily inside the worker.
///
/// ```ignore
/// let media = MediaController::new();
/// // ... in your AudioPlay shortcut action:
/// let _ = media.play_pause();
/// ```
pub struct MediaController {
    tx: Option<Sender<MediaCommand>>,
    worker: Option<JoinHandle<()>>,
}

impl MediaController {
    /// Spawn the worker and return immediately. Never blocks; never fails to
    /// construct even if no D-Bus session bus / no player is present yet (the
    /// connection is made lazily and re-established on demand).
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel::<MediaCommand>();
        let worker = thread::Builder::new()
            .name("mpris-media".to_owned())
            .spawn(move || run_worker(rx))
            .unwrap_or_else(|e| abort!("failed to spawn MPRIS media worker thread: {e:?}"));
        MediaController {
            tx: Some(tx),
            worker: Some(worker),
        }
    }

    /// Toggle play/pause on the active player. This is what the single
    /// `XF86AudioPlay` media key should call — there is no separate
    /// play-pause keysym, the dedicated key is itself a toggle.
    pub fn play_pause(&self) -> Result<(), MediaError> {
        self.send(MediaCommand::PlayPause)
    }

    /// Start/resume playback on the active player.
    pub fn play(&self) -> Result<(), MediaError> {
        self.send(MediaCommand::Play)
    }

    /// Pause the active player.
    pub fn pause(&self) -> Result<(), MediaError> {
        self.send(MediaCommand::Pause)
    }

    /// Stop the active player.
    pub fn stop(&self) -> Result<(), MediaError> {
        self.send(MediaCommand::Stop)
    }

    /// Skip to the next track on the active player.
    pub fn next(&self) -> Result<(), MediaError> {
        self.send(MediaCommand::Next)
    }

    /// Go to the previous track on the active player.
    pub fn previous(&self) -> Result<(), MediaError> {
        self.send(MediaCommand::Previous)
    }

    fn send(&self, cmd: MediaCommand) -> Result<(), MediaError> {
        self.tx
            .as_ref()
            .ok_or(MediaError::Disconnected)?
            .send(cmd)
            .map_err(|_| MediaError::Disconnected)
    }
}

impl Default for MediaController {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for MediaController {
    fn drop(&mut self) {
        // Dropping the sender closes the channel; the worker's `recv()` then
        // returns `Err`, the loop exits, and we join it. A command in flight
        // finishes within the per-call D-Bus timeout (500 ms) first.
        drop(self.tx.take());
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}

/// Worker loop. Owns the (non-`Send`) [`PlayerFinder`], so it is *created here*
/// inside the thread and never crosses a thread boundary. Re-establishes the
/// connection on demand if it ever errors.
fn run_worker(rx: Receiver<MediaCommand>) {
    let mut finder: Option<PlayerFinder> = None;

    while let Ok(cmd) = rx.recv() {
        // Ensure we have a live connection.
        if finder.is_none() {
            match PlayerFinder::new() {
                Ok(f) => finder = Some(f),
                Err(err) => {
                    warn!("MPRIS: cannot connect to D-Bus session bus: err={err}");
                    continue;
                }
            }
        }
        let finder_ref = finder.as_ref().unwrap_or_else(|| abort!("finder ensured present above"));

        match finder_ref.find_active() {
            Ok(player) => {
                if let Err(err) = apply(cmd, &player) {
                    warn!("MPRIS: command failed: err={err} player={} cmd={cmd:?}", player.identity());
                }
            }
            Err(FindingError::NoPlayerFound) => {
                // A media key pressed with nothing playing is a no-op.
                trace!("MPRIS: no active media player; ignoring: cmd={cmd:?}");
            }
            Err(FindingError::DBusError(err)) => {
                warn!("MPRIS: D-Bus error; will reconnect on next command: err={err}");
                finder = None; // force a fresh connection next time
            }
        }
    }
    // Channel closed -> controller dropped -> exit thread.
}

fn apply(cmd: MediaCommand, player: &Player) -> Result<(), mpris::DBusError> {
    match cmd {
        MediaCommand::PlayPause => player.play_pause(),
        MediaCommand::Play => player.play(),
        MediaCommand::Pause => player.pause(),
        MediaCommand::Stop => player.stop(),
        MediaCommand::Next => player.next(),
        MediaCommand::Previous => player.previous(),
    }
}

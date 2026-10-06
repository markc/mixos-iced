// SPDX-License-Identifier: MIT OR Apache-2.0

//! Session-owned cgroup containment without a user manager or D-Bus.
//! The OS supervisor delegates the session's cgroup. Application attachment
//! happens in the forked child before exec, closing the post-spawn adoption race.

use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::{OnceLock, RwLock, RwLockReadGuard};
use std::time::{Duration, Instant};

static APPS: OnceLock<Result<PathBuf, String>> = OnceLock::new();
static LAUNCHES: RwLock<bool> = RwLock::new(true);

/// Held across configuration and spawn, so shutdown waits for every in-flight
/// fork/exec and refuses all later launches before collecting descendants.
pub struct SpawnPermit {
    _guard: RwLockReadGuard<'static, bool>,
}

pub fn begin_spawn() -> Result<SpawnPermit, String> {
    let guard = LAUNCHES.read().map_err(|_| "launch fence poisoned")?;
    if !*guard {
        return Err("session is shutting down".into());
    }
    Ok(SpawnPermit { _guard: guard })
}

pub fn enabled() -> bool {
    std::env::var_os("MIXOS_CONTAIN_CHILDREN").is_some_and(|value| value == "1")
}

fn relative_cgroup(source: &str) -> Result<PathBuf, String> {
    let group = source
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .ok_or("unified cgroup membership is unavailable")?;
    let path = Path::new(group);
    if !path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::RootDir | Component::Normal(_)))
    {
        return Err("invalid unified cgroup membership".into());
    }
    Ok(path
        .strip_prefix("/")
        .map_err(|error| error.to_string())?
        .to_owned())
}

fn initialise() -> Result<PathBuf, String> {
    let membership = fs::read_to_string("/proc/self/cgroup").map_err(|error| error.to_string())?;
    let parent = Path::new("/sys/fs/cgroup").join(relative_cgroup(&membership)?);
    let apps = parent.join(format!("compd-{}-apps", std::process::id()));
    // Fail closed if a same-name group already exists: never adopt an old or
    // foreign group merely because a PID has been reused.
    fs::create_dir(&apps).map_err(|error| format!("create delegated apps cgroup: {error}"))?;
    if !apps.join("cgroup.kill").is_file() {
        let _ = fs::remove_dir(&apps);
        return Err(
            "kernel cgroup.kill support is required for complete descendant cleanup".into(),
        );
    }
    Ok(apps)
}

/// Configure an application's child-side cgroup placement. Enabling containment
/// makes failed delegation a launch failure, rather than leaving an unowned app.
pub fn configure_command(command: &mut Command, _permit: &SpawnPermit) -> Result<(), String> {
    if !enabled() {
        return Ok(());
    }
    let apps = APPS
        .get_or_init(initialise)
        .as_ref()
        .map_err(Clone::clone)?;
    let procs = OpenOptions::new()
        .write(true)
        .open(apps.join("cgroup.procs"))
        .map_err(|error| format!("open delegated apps membership: {error}"))?;
    // The closure owns the descriptor until spawn finishes. It performs only
    // async-signal-safe syscalls after fork; zero means the calling child PID.
    unsafe {
        command.pre_exec(move || {
            loop {
                let written = libc::write(procs.as_raw_fd(), b"0".as_ptr().cast(), 1);
                if written == 1 {
                    return Ok(());
                }
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::Interrupted {
                    return Err(error);
                }
            }
        });
    }
    Ok(())
}

/// Collect the entire apps subtree, including descendants that changed process
/// group or double-forked. The supervisor collects it too on an ungraceful exit.
pub fn kill_all() -> Result<(), String> {
    let mut launches = LAUNCHES.write().map_err(|_| "launch fence poisoned")?;
    *launches = false;
    if !enabled() {
        return Err("native child containment is disabled".into());
    }
    let Some(apps) = APPS.get() else {
        return Ok(());
    };
    let apps = apps.as_ref().map_err(Clone::clone)?;
    let mut events = fs::File::open(apps.join("cgroup.events"))
        .map_err(|error| format!("open apps cgroup events: {error}"))?;
    fs::write(apps.join("cgroup.kill"), "1").map_err(|error| error.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        events
            .seek(SeekFrom::Start(0))
            .map_err(|error| error.to_string())?;
        let mut status = String::new();
        events
            .read_to_string(&mut status)
            .map_err(|error| error.to_string())?;
        if status.lines().any(|line| line == "populated 0") {
            return Ok(());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("apps cgroup is still populated after cleanup deadline".into());
        }
        let mut pollfd = libc::pollfd {
            fd: events.as_raw_fd(),
            events: libc::POLLPRI,
            revents: 0,
        };
        // cgroup.events changes wake poll; no process-list polling or sleeps.
        let result = unsafe {
            libc::poll(
                &mut pollfd,
                1,
                remaining.as_millis().min(i32::MAX as u128) as i32,
            )
        };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(format!("watch apps cgroup: {error}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn membership_stays_below_the_cgroup_mount() {
        assert_eq!(
            relative_cgroup("0::/session/unit\n").unwrap(),
            PathBuf::from("session/unit")
        );
        assert_eq!(relative_cgroup("0::/\n").unwrap(), PathBuf::new());
        for invalid in ["0::relative", "0::/../other", "1:cpu:/session"] {
            assert!(relative_cgroup(invalid).is_err(), "{invalid}");
        }
    }
}

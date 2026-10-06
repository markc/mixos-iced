// SPDX-License-Identifier: MIT OR Apache-2.0

//! Session-owned cgroup containment without a user manager or D-Bus.
//! The OS supervisor delegates the session's cgroup. Application attachment
//! happens in the forked child before exec, closing the post-spawn adoption race.

use std::fs::{self, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

static APPS: OnceLock<Result<PathBuf, String>> = OnceLock::new();

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
pub fn configure_command(command: &mut Command) -> Result<(), String> {
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
    if !enabled() {
        return Err("native child containment is disabled".into());
    }
    let Some(apps) = APPS.get() else {
        return Ok(());
    };
    let apps = apps.as_ref().map_err(Clone::clone)?;
    fs::write(apps.join("cgroup.kill"), "1").map_err(|error| error.to_string())
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

// SPDX-License-Identifier: MIT OR Apache-2.0
//! Mix-local mixos path resolver. Inlined so mix has no dependency
//! on the cos-side `mixos-lib-config` crate; behaviour-parity with
//! that crate's `mixos_root()` / `mixos_path()` / `current_uid()`.
//! The rules match so a `mix` invocation and any cos daemon running on
//! the same node land on the same paths.
//!
//! **Everything is keyed off one root, `$MIXOS`.** The root is found by:
//!
//! 1. the `MIXOS` environment variable;
//! 2. self-location — an ancestor of the running binary that holds both
//!    `bootstrap` and `src/Cargo.toml` (so `$MIXOS/bin/mix` and a
//!    `cargo run` binary under `$MIXOS/src/target/…` both find their
//!    own checkout with no environment at all);
//! 3. otherwise the root is *unknown*.
//!
//! | Kind | Env override | Root known        | Root unknown (legacy FHS/XDG)        |
//! |------|--------------|-------------------|--------------------------------------|
//! | Src  | MIXOS_SRC   | `$MIXOS/src`     | `~/Projects/mixos/src`              |
//! | Etc  | MIXOS_ETC   | `$MIXOS/etc`     | `~/.config/mixos/` · `/etc/mixos/` |
//! | Bin  | MIXOS_BIN   | `$MIXOS/bin`     | `~/.local/bin/` · `/usr/local/bin/`  |
//! | Share | MIXOS_SHARE | `/opt/mixos/share` | `/opt/mixos/share` (both)         |
//!
//! A system install (`/opt/mixos/bin/mix`, no `$MIXOS`, no checkout
//! above it) therefore keeps the FHS defaults it always had. Mix keeps
//! only Src/Etc/Bin/Share from the parent's full enum. Share deliberately does
//! not depend on the checkout root or user ID.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Dir {
    Src,
    Etc,
    Bin,
    // Mirror the shared path contract even before a Mix caller needs this kind.
    #[allow(dead_code)]
    Share,
}

struct ResolvedPaths {
    root: Option<PathBuf>,
    src: PathBuf,
    etc: PathBuf,
    bin: PathBuf,
    share: PathBuf,
}

static PATHS: OnceLock<ResolvedPaths> = OnceLock::new();

pub fn mixos_path(kind: Dir) -> PathBuf {
    let paths = PATHS.get_or_init(resolve_all);
    match kind {
        Dir::Src => paths.src.clone(),
        Dir::Etc => paths.etc.clone(),
        Dir::Bin => paths.bin.clone(),
        Dir::Share => paths.share.clone(),
    }
}

/// The install root (`$MIXOS`) when it is known — from the environment
/// or by self-location — else `None`. Cached.
pub fn mixos_root() -> Option<PathBuf> {
    PATHS.get_or_init(resolve_all).root.clone()
}

pub fn mixos_src() -> PathBuf {
    mixos_path(Dir::Src)
}

pub fn current_uid() -> u32 {
    unsafe { libc::getuid() }
}

/// Default root when nothing names one: the documented clone location.
pub fn default_root(home: &Path) -> PathBuf {
    home.join("Projects/mixos")
}

/// A directory is a MixOS root iff it carries the two files every
/// checkout has and no runtime tree does.
fn is_root(dir: &Path) -> bool {
    dir.join("AGENTS.md").is_file() && dir.join("Cargo.toml").is_file()
}

/// Root resolution shared with `mixos-lib-config`: `$MIXOS`, else the
/// nearest of the running binary's first six ancestors that [`is_root`].
pub fn locate_root(env_root: Option<PathBuf>, exe: Option<&Path>) -> Option<PathBuf> {
    if let Some(root) = env_root {
        return Some(root);
    }
    exe?.ancestors()
        .skip(1)
        .take(6)
        .find(|d| is_root(d))
        .map(Path::to_path_buf)
}

fn resolve_all() -> ResolvedPaths {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/root"));
    let exe = std::env::current_exe().ok();
    let root = locate_root(std::env::var_os("MIXOS").map(PathBuf::from), exe.as_deref());

    let src = env_or("MIXOS_SRC", || {
        root.clone().unwrap_or_else(|| default_root(&home))
    });

    let etc = config::path(config::Dir::Etc);

    let bin = env_or("MIXOS_BIN", || PathBuf::from("/opt/mixos/bin"));

    let share = config::path(config::Dir::Share);

    ResolvedPaths {
        root,
        src,
        etc,
        bin,
        share,
    }
}

/// Installed resources have the same root for every user and service.
pub fn resolve_share(override_path: Option<PathBuf>) -> PathBuf {
    override_path
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| PathBuf::from("/opt/mixos/share"))
}

fn env_or(var: &str, fallback: impl FnOnce() -> PathBuf) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .unwrap_or_else(fallback)
}

/// Strings only at capture time: no filesystem or name-service work on the
/// bootstrap caller. Resolution later must not call dirs (which reads environ).
pub(crate) struct EtcEnvironment {
    root: Option<PathBuf>,
    etc: Option<PathBuf>,
    home: Option<PathBuf>,
    xdg_config: Option<PathBuf>,
}

impl EtcEnvironment {
    pub(crate) fn capture() -> Self {
        Self {
            root: std::env::var_os("MIXOS").map(PathBuf::from),
            etc: std::env::var_os("MIXOS_ETC").map(PathBuf::from),
            home: std::env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            xdg_config: std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
        }
    }

    pub(crate) fn resolve(mut self) -> (PathBuf, bool) {
        self.root = self.root.filter(|path| !path.as_os_str().is_empty());
        self.etc = self.etc.filter(|path| !path.as_os_str().is_empty());
        self.xdg_config = self.xdg_config.filter(|path| !path.as_os_str().is_empty());
        let isolated = self.etc.is_some() || self.root.is_some();
        let environment = config::Environment {
            root: self.root,
            etc: self.etc,
            home: Some(self.home.unwrap_or_else(home_without_environment)),
            xdg_config_home: self.xdg_config,
            uid: current_uid(),
            ..config::Environment::default()
        };
        let dirs = config::Dirs::resolve(&environment);
        (dirs.get(config::Dir::Etc).to_owned(), isolated)
    }
}

fn home_without_environment() -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut result = std::ptr::null_mut();
    let mut buffer = vec![0u8; 65536];
    // Resident-only NSS fallback, matching dirs' HOME-absent Unix behaviour.
    let rc = unsafe {
        libc::getpwuid_r(
            libc::getuid(),
            entry.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        )
    };
    if rc == 0 && !result.is_null() {
        let entry = unsafe { entry.assume_init() };
        if !entry.pw_dir.is_null() {
            let home = unsafe { std::ffi::CStr::from_ptr(entry.pw_dir) };
            return PathBuf::from(std::ffi::OsStr::from_bytes(home.to_bytes()));
        }
    }
    PathBuf::from("/root")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_resources_use_an_absolute_installation_override() {
        assert_eq!(resolve_share(None), PathBuf::from("/opt/mixos/share"));
        assert_eq!(resolve_share(Some(PathBuf::from(""))), resolve_share(None));
        assert_eq!(
            resolve_share(Some(PathBuf::from("relative"))),
            resolve_share(None)
        );
        assert_eq!(
            resolve_share(Some(PathBuf::from("/srv/resources"))),
            PathBuf::from("/srv/resources")
        );
    }

    #[test]
    fn captured_etc_environment_preserves_override_and_root_precedence() {
        let captured = |etc| EtcEnvironment {
            root: Some(PathBuf::from("/srv/mixos")),
            etc,
            home: Some(PathBuf::from("/home/alice")),
            xdg_config: Some(PathBuf::from("/alternate/config")),
        };
        assert_eq!(
            captured(Some(PathBuf::from("/isolated/etc"))).resolve(),
            (PathBuf::from("/isolated/etc"), true)
        );
        assert_eq!(
            captured(None).resolve(),
            (PathBuf::from("/srv/mixos/etc"), true)
        );
    }

    #[test]
    fn empty_captured_overrides_match_the_shared_directory_rule() {
        let captured = EtcEnvironment {
            root: Some(PathBuf::new()),
            etc: Some(PathBuf::new()),
            home: Some(PathBuf::from("/home/user")),
            xdg_config: Some(PathBuf::new()),
        };
        let expected = config::Dirs::resolve(&config::Environment {
            home: Some(PathBuf::from("/home/user")),
            uid: current_uid(),
            ..Default::default()
        });
        assert_eq!(
            captured.resolve(),
            (expected.get(config::Dir::Etc).to_owned(), false)
        );
    }

    #[test]
    fn env_root_wins_over_self_location() {
        let root = locate_root(
            Some(PathBuf::from("/srv/mixos")),
            Some(Path::new("/nowhere/bin/mix")),
        );
        assert_eq!(root, Some(PathBuf::from("/srv/mixos")));
    }

    #[test]
    fn self_location_finds_the_checkout_above_bin_and_above_target() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("MixOS");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::create_dir_all(root.join("src/target/release")).unwrap();
        std::fs::write(root.join("AGENTS.md"), "").unwrap();
        std::fs::write(root.join("Cargo.toml"), "").unwrap();
        assert_eq!(
            locate_root(None, Some(&root.join("bin/mix"))),
            Some(root.clone())
        );
        assert_eq!(
            locate_root(None, Some(&root.join("src/target/release/mix"))),
            Some(root.clone())
        );
        // a system install has no checkout above it
        assert_eq!(
            locate_root(None, Some(Path::new("/opt/mixos/bin/mix"))),
            None
        );
    }

    #[test]
    fn a_runtime_tree_is_not_a_root() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("bin")).unwrap();
        assert_eq!(locate_root(None, Some(&tmp.path().join("bin/mix"))), None);
    }
}

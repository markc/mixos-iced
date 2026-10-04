// SPDX-License-Identifier: MIT OR Apache-2.0

//! Where MixOS keeps its files.
//!
//! Every directory comes from one rule, keyed off one root, `$MIXOS`:
//!
//! | Dir     | Override      | Root set        | No root: user · root user |
//! |---------|---------------|-----------------|---------------------------|
//! | `Etc`   | `MIXOS_ETC`   | `$MIXOS/etc`    | `$XDG_CONFIG_HOME/mixos` (`~/.config/mixos`) · `/etc/mixos` |
//! | `Var`   | `MIXOS_VAR`   | `$MIXOS/var`    | `$XDG_DATA_HOME/mixos` (`~/.local/share/mixos`) · `/var/lib/mixos` |
//! | `Run`   | `MIXOS_RUN`   | `$MIXOS/run`    | `$XDG_RUNTIME_DIR/mixos` (`/tmp/mixos-run`) · `/run/mixos` |
//! | `Share` | `MIXOS_SHARE` | `/opt/mixos/share` | `/opt/mixos/share` |
//!
//! "User" is a process whose real uid is not 0. An XDG variable counts only
//! when it is absolute, as the XDG base directory specification requires.
//! `Share` is installed read-only data: it ignores the root and the uid,
//! and its override must be absolute. An empty variable is unset.
//!
//! [`path`] resolves from the process environment once and caches the
//! answer for the life of the process. Tests, and anything that must not
//! read the real environment, build an [`Environment`] and resolve
//! [`Dirs`] from it.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The directories MixOS resolves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Dir {
    /// Configuration files (`*.conf.mix`).
    Etc,
    /// Persistent state: databases, saved state files.
    Var,
    /// Runtime sockets and PIDs.
    Run,
    /// Installed read-only data, independent of the root and the user.
    Share,
}

impl Dir {
    /// Every directory, in table order.
    pub const ALL: [Dir; 4] = [Dir::Etc, Dir::Var, Dir::Run, Dir::Share];

    /// The environment variable that overrides this directory.
    pub const fn env_var(self) -> &'static str {
        match self {
            Dir::Etc => "MIXOS_ETC",
            Dir::Var => "MIXOS_VAR",
            Dir::Run => "MIXOS_RUN",
            Dir::Share => "MIXOS_SHARE",
        }
    }
}

/// The environment variable naming the MixOS root.
pub const ROOT_VAR: &str = "MIXOS";

/// Where installed read-only data lives when nothing overrides it.
pub const DEFAULT_SHARE: &str = "/opt/mixos/share";

/// The directory for `dir`, resolved from the process environment on the
/// first call and cached.
pub fn path(dir: Dir) -> PathBuf {
    static DIRS: OnceLock<Dirs> = OnceLock::new();
    DIRS.get_or_init(|| Dirs::resolve(&Environment::current()))
        .get(dir)
        .to_path_buf()
}

/// The inputs the directory rule reads. [`Environment::current`] takes
/// them from the process; a test fills the fields by hand.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Environment {
    /// `$MIXOS`.
    pub root: Option<PathBuf>,
    /// `$MIXOS_ETC`.
    pub etc: Option<PathBuf>,
    /// `$MIXOS_VAR`.
    pub var: Option<PathBuf>,
    /// `$MIXOS_RUN`.
    pub run: Option<PathBuf>,
    /// `$MIXOS_SHARE`.
    pub share: Option<PathBuf>,
    /// `$XDG_CONFIG_HOME`.
    pub xdg_config_home: Option<PathBuf>,
    /// `$XDG_DATA_HOME`.
    pub xdg_data_home: Option<PathBuf>,
    /// `$XDG_RUNTIME_DIR`.
    pub xdg_runtime_dir: Option<PathBuf>,
    /// `$HOME`.
    pub home: Option<PathBuf>,
    /// The real user id; 0 selects the system directories.
    pub uid: u32,
}

impl Environment {
    /// The process environment and real uid. An empty variable is unset.
    pub fn current() -> Self {
        let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
        Self {
            root: var(ROOT_VAR),
            etc: var(Dir::Etc.env_var()),
            var: var(Dir::Var.env_var()),
            run: var(Dir::Run.env_var()),
            share: var(Dir::Share.env_var()),
            xdg_config_home: var("XDG_CONFIG_HOME"),
            xdg_data_home: var("XDG_DATA_HOME"),
            xdg_runtime_dir: var("XDG_RUNTIME_DIR"),
            home: var("HOME"),
            uid: current_uid(),
        }
    }
}

/// Every directory, resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dirs {
    etc: PathBuf,
    var: PathBuf,
    run: PathBuf,
    share: PathBuf,
}

impl Dirs {
    /// Apply the directory rule to `env`.
    pub fn resolve(env: &Environment) -> Self {
        let user = env.uid != 0;
        let home = env.home.clone().unwrap_or_else(|| PathBuf::from("/root"));
        // An XDG base directory, when set and absolute, else its default
        // under the home directory; `mixos` under that.
        let xdg = |base: &Option<PathBuf>, default: &str| {
            base.clone()
                .filter(|p| p.is_absolute())
                .unwrap_or_else(|| home.join(default))
                .join("mixos")
        };

        let etc = env.etc.clone().unwrap_or_else(|| match &env.root {
            Some(root) => root.join("etc"),
            None if user => xdg(&env.xdg_config_home, ".config"),
            None => PathBuf::from("/etc/mixos"),
        });
        let var = env.var.clone().unwrap_or_else(|| match &env.root {
            Some(root) => root.join("var"),
            None if user => xdg(&env.xdg_data_home, ".local/share"),
            None => PathBuf::from("/var/lib/mixos"),
        });
        let run = env.run.clone().unwrap_or_else(|| match &env.root {
            Some(root) => root.join("run"),
            None if user => env
                .xdg_runtime_dir
                .clone()
                .filter(|p| p.is_absolute())
                .map(|runtime| runtime.join("mixos"))
                .unwrap_or_else(|| PathBuf::from("/tmp/mixos-run")),
            None => PathBuf::from("/run/mixos"),
        });
        let share = env
            .share
            .clone()
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| PathBuf::from(DEFAULT_SHARE));

        Self { etc, var, run, share }
    }

    /// The resolved directory for `dir`.
    pub fn get(&self, dir: Dir) -> &Path {
        match dir {
            Dir::Etc => &self.etc,
            Dir::Var => &self.var,
            Dir::Run => &self.run,
            Dir::Share => &self.share,
        }
    }
}

/// The real user id of this process.
fn current_uid() -> u32 {
    // SAFETY: getuid takes no arguments, never fails and returns a plain
    // integer; it has no memory or thread-safety preconditions.
    unsafe { libc::getuid() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user() -> Environment {
        Environment {
            home: Some(PathBuf::from("/home/user")),
            uid: 1000,
            ..Environment::default()
        }
    }

    fn system() -> Environment {
        Environment {
            home: Some(PathBuf::from("/root")),
            uid: 0,
            ..Environment::default()
        }
    }

    #[test]
    fn overrides_win_over_everything() {
        let env = Environment {
            root: Some(PathBuf::from("/srv/mixos")),
            etc: Some(PathBuf::from("/sandbox/etc")),
            var: Some(PathBuf::from("/sandbox/var")),
            run: Some(PathBuf::from("/sandbox/run")),
            share: Some(PathBuf::from("/sandbox/share")),
            ..user()
        };
        let dirs = Dirs::resolve(&env);
        assert_eq!(dirs.get(Dir::Etc), Path::new("/sandbox/etc"));
        assert_eq!(dirs.get(Dir::Var), Path::new("/sandbox/var"));
        assert_eq!(dirs.get(Dir::Run), Path::new("/sandbox/run"));
        assert_eq!(dirs.get(Dir::Share), Path::new("/sandbox/share"));
    }

    #[test]
    fn a_root_places_etc_var_and_run_under_it_for_user_and_system() {
        for mut env in [user(), system()] {
            env.root = Some(PathBuf::from("/srv/mixos"));
            let dirs = Dirs::resolve(&env);
            assert_eq!(dirs.get(Dir::Etc), Path::new("/srv/mixos/etc"));
            assert_eq!(dirs.get(Dir::Var), Path::new("/srv/mixos/var"));
            assert_eq!(dirs.get(Dir::Run), Path::new("/srv/mixos/run"));
            // Share is installed data, not part of the root.
            assert_eq!(dirs.get(Dir::Share), Path::new(DEFAULT_SHARE));
        }
    }

    #[test]
    fn a_user_without_a_root_gets_the_xdg_defaults_under_home() {
        let dirs = Dirs::resolve(&user());
        assert_eq!(dirs.get(Dir::Etc), Path::new("/home/user/.config/mixos"));
        assert_eq!(dirs.get(Dir::Var), Path::new("/home/user/.local/share/mixos"));
        assert_eq!(dirs.get(Dir::Run), Path::new("/tmp/mixos-run"));
        assert_eq!(dirs.get(Dir::Share), Path::new(DEFAULT_SHARE));
    }

    #[test]
    fn a_user_with_xdg_variables_gets_mixos_under_them() {
        let env = Environment {
            xdg_config_home: Some(PathBuf::from("/home/user/cfg")),
            xdg_data_home: Some(PathBuf::from("/home/user/data")),
            xdg_runtime_dir: Some(PathBuf::from("/run/user/1000")),
            ..user()
        };
        let dirs = Dirs::resolve(&env);
        assert_eq!(dirs.get(Dir::Etc), Path::new("/home/user/cfg/mixos"));
        assert_eq!(dirs.get(Dir::Var), Path::new("/home/user/data/mixos"));
        assert_eq!(dirs.get(Dir::Run), Path::new("/run/user/1000/mixos"));
    }

    #[test]
    fn relative_xdg_variables_are_ignored() {
        let env = Environment {
            xdg_config_home: Some(PathBuf::from("cfg")),
            xdg_data_home: Some(PathBuf::from("data")),
            xdg_runtime_dir: Some(PathBuf::from("run")),
            ..user()
        };
        let dirs = Dirs::resolve(&env);
        assert_eq!(dirs.get(Dir::Etc), Path::new("/home/user/.config/mixos"));
        assert_eq!(dirs.get(Dir::Var), Path::new("/home/user/.local/share/mixos"));
        assert_eq!(dirs.get(Dir::Run), Path::new("/tmp/mixos-run"));
    }

    #[test]
    fn root_user_without_a_root_gets_the_system_directories() {
        let dirs = Dirs::resolve(&system());
        assert_eq!(dirs.get(Dir::Etc), Path::new("/etc/mixos"));
        assert_eq!(dirs.get(Dir::Var), Path::new("/var/lib/mixos"));
        assert_eq!(dirs.get(Dir::Run), Path::new("/run/mixos"));
        assert_eq!(dirs.get(Dir::Share), Path::new(DEFAULT_SHARE));
        // XDG variables do not move root's directories.
        let env = Environment {
            xdg_config_home: Some(PathBuf::from("/root/cfg")),
            ..system()
        };
        assert_eq!(Dirs::resolve(&env).get(Dir::Etc), Path::new("/etc/mixos"));
    }

    #[test]
    fn no_home_falls_back_to_root_home() {
        let env = Environment { home: None, ..user() };
        assert_eq!(Dirs::resolve(&env).get(Dir::Etc), Path::new("/root/.config/mixos"));
    }

    #[test]
    fn share_takes_only_an_absolute_override() {
        let absolute = Environment {
            share: Some(PathBuf::from("/srv/resources")),
            ..user()
        };
        assert_eq!(Dirs::resolve(&absolute).get(Dir::Share), Path::new("/srv/resources"));
        let relative = Environment {
            share: Some(PathBuf::from("relative")),
            ..user()
        };
        assert_eq!(Dirs::resolve(&relative).get(Dir::Share), Path::new(DEFAULT_SHARE));
    }

    #[test]
    fn every_dir_has_its_own_variable() {
        let vars: Vec<&str> = Dir::ALL.iter().map(|d| d.env_var()).collect();
        assert_eq!(vars, ["MIXOS_ETC", "MIXOS_VAR", "MIXOS_RUN", "MIXOS_SHARE"]);
    }

    #[test]
    fn path_resolves_from_the_process_and_is_stable() {
        // Whatever the environment, the cached answer is the rule applied
        // to it, and the same on every call.
        let first = path(Dir::Etc);
        assert_eq!(first, Dirs::resolve(&Environment::current()).get(Dir::Etc));
        assert_eq!(path(Dir::Etc), first);
    }
}
